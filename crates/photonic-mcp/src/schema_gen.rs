use serde_json::{json, Value};

/// Returns the MCP tool list manifest (the `tools/list` response payload).
///
/// Public so tooling (e.g. the `dump_tools` binary that regenerates
/// `docs/mcp-api.md`) can read the canonical schema without standing up a server.
pub fn tool_list() -> Value {
    // Effect kinds sourced from the effect manifest catalogue (spec 30 §2.7) so
    // the MCP doc-drift gate covers the catalogue: adding a manifest changes this
    // enum, which changes docs/mcp-api.md, which the gate diffs. Only manifests
    // that project to a legacy `EffectKind` variant (the shape `add_effect`
    // accepts) are listed, in the catalogue's stable id-sorted order.
    let effect_kind_enum: Vec<Value> = photonic_core::timeline::manifests()
        .iter()
        .filter_map(|m| m.id.legacy_kind())
        .map(|k| serde_json::to_value(k).expect("EffectKind serializes to a string tag"))
        .collect();
    let mut tools = json!([
            {
                "name": "search_actions",
                "description": "Search the MCP tool catalog by keywords (name/description). Returns ranked actions with complete input/output schemas and available behavior annotations. Use get_action_schema for exact-name lookup and execute_action to run a hit.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Keywords, e.g. \"split clip\" or \"export\"." },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 15, "description": "Max hits (default 15, max 50)." }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "get_action_schema",
                "description": "Get one complete tool definition by exact name, including inputSchema, outputSchema when available, and behavior annotations. No tool is executed.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Exact, case-sensitive tool name." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "execute_action",
                "description": "Run a tool by name with a params object. Same effect as tools/call. Use after search_actions.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Exact tool name from search_actions or tools/list." },
                        "arguments": { "type": "object", "description": "Tool arguments object." }
                    },
                    "required": ["name"]
                }
            },

            {
                "name": "create_shape",
                "description": "Create a primitive shape (rectangle, rounded_rect, ellipse, arc, polygon, star, line). For arc: x,y,width,height define the bounding box; arc_start_angle and arc_end_angle set the sweep in degrees (0=3 o'clock); arc_open=true for open arc, false for closed pie sector.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "shape_type": { "type": "string", "enum": ["rectangle","rounded_rect","ellipse","arc","polygon","star","line"] },
                        "x": { "type": "number" },
                        "y": { "type": "number" },
                        "width": { "type": "number" },
                        "height": { "type": "number" },
                        "corner_radius": { "type": "number", "description": "Corner radius for rounded_rect shapes in document units (default: 10.0). Clamped to half the shortest side." },
                        "arc_start_angle": { "type": "number", "description": "Arc start angle in degrees (0=3 o'clock, 90=6 o'clock). Default: 0." },
                        "arc_end_angle": { "type": "number", "description": "Arc end angle in degrees. Default: 270 (¾ circle)." },
                        "arc_open": { "type": "boolean", "description": "If true, draw open arc stroke only. If false (default), close back to center (pie sector)." },
                        "rx": { "type": "number", "description": "Reserved" },
                        "sides": { "type": "integer", "description": "Sides (polygon/star)" },
                        "inner_radius": { "type": "number", "description": "Inner radius ratio (star, 0–1)" },
                        "fill": { "type": "object", "description": "Fill — solid: {\"type\":\"solid\",\"color\":\"#rrggbb\"} | none: {\"type\":\"none\"} | linear: {\"type\":\"gradient\",\"gradient_type\":\"linear\",\"colors\":[\"#hex1\",\"#hex2\"],\"coords\":[x0,y0,x1,y1]} | radial: {\"type\":\"gradient\",\"gradient_type\":\"radial\",\"colors\":[\"#hex1\",\"#hex2\"],\"coords\":[cx,cy,r]} | fluid: {\"type\":\"fluid_gradient\",\"points\":[{\"x\":100,\"y\":50,\"color\":\"#ff0000\"},...],\"power\":2.0} | mesh: {\"type\":\"mesh_gradient\",\"rows\":2,\"cols\":2,\"vertices\":[{\"x\":0,\"y\":0,\"color\":\"#ff0000\"},...]}" },
                        "color": { "type": "string", "description": "Shorthand for a solid fill colour, e.g. \"#2277ff\". Ignored when \"fill\" is also provided." },
                        "stroke": { "type": "object", "description": "Stroke outline. Fields: color (#RRGGBB), width (number), enabled (bool), opacity (0-1), line_cap (butt|round|square), line_join (miter|round|bevel), align (center|inside|outside), dash_array ([dash,gap,...] up to 6 values), dash_offset (number). Example: {\"color\":\"#000000\",\"width\":2,\"enabled\":true,\"dash_array\":[8,4]}" },
                        "layer_id": { "type": "string" },
                        "name": { "type": "string" },
                        "tags": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["shape_type","x","y","width","height"]
                }
            },
            {
                "name": "build_shape_from_points",
                "description": "Place any number of [x,y] points and connect them in any order to build a filled/stroked shape. Use connection_order to specify a custom vertex sequence; omit it to connect in the order given.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "points": {
                            "type": "array",
                            "description": "Array of [x, y] coordinate pairs (the vertices)",
                            "items": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 }
                        },
                        "connection_order": {
                            "type": "array",
                            "description": "Indices into 'points' defining connection sequence. Omit for sequential order.",
                            "items": { "type": "integer" }
                        },
                        "closed": { "type": "boolean", "description": "Close the path back to the start (default: true)" },
                        "fill": { "type": "object", "description": "Fill — solid: {\"type\":\"solid\",\"color\":\"#rrggbb\"} | none: {\"type\":\"none\"} | linear: {\"type\":\"gradient\",\"gradient_type\":\"linear\",\"colors\":[\"#hex1\",\"#hex2\"],\"coords\":[x0,y0,x1,y1]} | radial: {\"type\":\"gradient\",\"gradient_type\":\"radial\",\"colors\":[\"#hex1\",\"#hex2\"],\"coords\":[cx,cy,r]} | fluid: {\"type\":\"fluid_gradient\",\"points\":[{\"x\":100,\"y\":50,\"color\":\"#ff0000\"},...],\"power\":2.0} | mesh: {\"type\":\"mesh_gradient\",\"rows\":2,\"cols\":2,\"vertices\":[{\"x\":0,\"y\":0,\"color\":\"#ff0000\"},...]}" },
                        "stroke": { "type": "object", "description": "Stroke outline. Fields: color (#RRGGBB), width (number), enabled (bool), opacity (0-1), line_cap (butt|round|square), line_join (miter|round|bevel), align (center|inside|outside), dash_array ([dash,gap,...] up to 6 values), dash_offset (number). Example: {\"color\":\"#000000\",\"width\":2,\"enabled\":true,\"dash_array\":[8,4]}" },
                        "layer_id": { "type": "string" },
                        "name": { "type": "string" },
                        "tags": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["points"]
                }
            },
            {
                "name": "create_path",
                "description": "Create a vector path from SVG path data (M/L/C/Q/Z commands)",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path_data": { "type": "string", "description": "SVG path data, e.g. 'M 0 0 L 100 0 L 100 100 Z'" },
                        "fill": { "type": "object", "description": "Fill — solid: {\"type\":\"solid\",\"color\":\"#rrggbb\"} | none: {\"type\":\"none\"} | linear: {\"type\":\"gradient\",\"gradient_type\":\"linear\",\"colors\":[\"#hex1\",\"#hex2\"],\"coords\":[x0,y0,x1,y1]} | radial: {\"type\":\"gradient\",\"gradient_type\":\"radial\",\"colors\":[\"#hex1\",\"#hex2\"],\"coords\":[cx,cy,r]} | fluid: {\"type\":\"fluid_gradient\",\"points\":[{\"x\":100,\"y\":50,\"color\":\"#ff0000\"},...],\"power\":2.0} | mesh: {\"type\":\"mesh_gradient\",\"rows\":2,\"cols\":2,\"vertices\":[{\"x\":0,\"y\":0,\"color\":\"#ff0000\"},...]}" },
                        "stroke": { "type": "object", "description": "Stroke outline. Fields: color (#RRGGBB), width (number), enabled (bool), opacity (0-1), line_cap (butt|round|square), line_join (miter|round|bevel), align (center|inside|outside), dash_array ([dash,gap,...] up to 6 values), dash_offset (number). Example: {\"color\":\"#000000\",\"width\":2,\"enabled\":true,\"dash_array\":[8,4]}" },
                        "layer_id": { "type": "string" },
                        "name": { "type": "string" },
                        "tags": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["path_data"]
                }
            },
            {
                "name": "create_vectors_from_css",
                "description": "Compile a bounded CSS snippet into editable Photonic groups and vector paths. Strict mode (the default) rejects unsupported/lossy CSS without mutation; dry_run returns the deterministic plan without changing the document.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "css": { "type": "string", "description": "Inline declaration block or supported selector tree." },
                        "selector": { "type": "string", "description": "Optional virtual root selector." },
                        "origin": { "type": "object", "properties": { "x": {"type":"number"}, "y": {"type":"number"} } },
                        "viewport": { "type": "object", "properties": { "width": {"type":"number"}, "height": {"type":"number"} } },
                        "layer_id": { "type": "string" },
                        "group_name": { "type": "string" },
                        "strict": { "type": "boolean", "default": true },
                        "dry_run": { "type": "boolean", "default": false }
                    },
                    "required": ["css"]
                }
            },
            {
                "name": "create_vectors_from_react",
                "description": "Import a bounded, static local JSX fragment or an allowlisted local React snapshot into editable Photonic groups and vector paths. The snapshot resolver reads only explicitly bounded local modules and assets; it never executes JavaScript, fetches network resources, follows arbitrary imports, or loads external Tailwind configuration. Unsupported or dynamic input is rejected before the document changes.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "jsx": { "type": "string", "description": "Static JSX fragment of intrinsic layout elements." },
                        "source_path": { "type": "string", "description": "Allowlisted local module path; it and every resolved module/asset must be beneath module_roots." },
                        "export_name": { "type": "string", "description": "Named React export to import." },
                        "props": { "type": "object", "description": "JSON-only static props snapshot." },
                        "module_roots": { "type": "array", "items": {"type":"string"}, "description": "Approved local roots for source and bounded imports." },
                        "theme_tokens": { "type": "object", "description": "Optional hex overrides for source-resolved light theme tokens.", "properties": { "card": {"type":"string"}, "foreground": {"type":"string"}, "muted_foreground": {"type":"string"}, "border": {"type":"string"} } },
                        "interaction_policy": { "type": "string", "enum": ["reject", "strip"], "default": "reject", "description": "Reject rendered event handlers, or strip them with warnings/provenance. JavaScript is never executed." },
                        "dynamic_content": { "type": "object", "description": "Pinned bounded wrapper branches, such as backgroundImage:null and enableInactivity:false." },
                        "origin": { "type": "object", "properties": { "x": {"type":"number"}, "y": {"type":"number"} } },
                        "viewport": { "type": "object", "properties": { "width": {"type":"number"}, "height": {"type":"number"} } },
                        "layer_id": { "type": "string" }, "group_name": { "type": "string" },
                        "strict": { "type": "boolean", "default": true }, "dry_run": { "type": "boolean", "default": false }
                    },
                    "oneOf": [{"required":["jsx"]},{"required":["source_path","export_name","props","module_roots"]}]
                }
            },
            {
                "name": "create_flare",
                "description": "Create a procedural lens flare vector effect at the specified position. Generates a grouped set of paths: a semi-transparent halo circle, radiating ray triangles, and concentric stroke rings.\n\nAll parts are grouped as 'Lens Flare'. Useful for light effects, sparkle decorations, and sci-fi/fantasy illustrations.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "cx": { "type": "number", "description": "Center X coordinate" },
                        "cy": { "type": "number", "description": "Center Y coordinate" },
                        "halo_radius": { "type": "number", "description": "Halo circle radius (default: 50)" },
                        "ray_count": { "type": "integer", "description": "Number of radiating rays (default: 12)" },
                        "ray_length": { "type": "number", "description": "Length of rays beyond the halo (default: 80)" },
                        "ring_count": { "type": "integer", "description": "Number of concentric rings (default: 3)" },
                        "halo_color": { "type": "string", "description": "Halo color as hex (default: #fffbe6)" },
                        "ray_opacity": { "type": "number", "description": "Ray opacity 0–1 (default: 0.3)" },
                        "layer_id": { "type": "string", "description": "Target layer UUID (default: active layer)" }
                    },
                    "required": ["cx", "cy"]
                }
            },
            {
                "name": "create_curvature_path",
                "description": "Create a smooth curve that passes through all specified points using Catmull-Rom interpolation. Unlike create_path (which requires manual SVG path data with bezier control points), this tool automatically computes smooth bezier handles from just the on-curve points.\n\nUse this when you want a smooth flowing curve through a set of coordinates without manually calculating control points. Optionally close the path to form a smooth closed shape.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "points": { "type": "array", "items": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 }, "description": "Ordered [x, y] points the curve passes through. Minimum 2 points." },
                        "closed": { "type": "boolean", "description": "Close the path smoothly back to the first point (default: false)" },
                        "fill": { "type": "object", "description": "Fill style (see create_path for format)" },
                        "stroke": { "type": "object", "description": "Stroke style (see create_path for format)" },
                        "layer_id": { "type": "string", "description": "Target layer UUID (default: active layer)" }
                    },
                    "required": ["points"]
                }
            },
            {
                "name": "create_spiral",
                "description": "Create an Archimedean spiral path. Specify center, outer/inner radius, and number of turns.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "number", "description": "X coordinate of spiral center" },
                        "y": { "type": "number", "description": "Y coordinate of spiral center" },
                        "outer_radius": { "type": "number", "description": "Maximum (outer) radius in document units" },
                        "inner_radius": { "type": "number", "description": "Minimum (inner) radius. Use 0 for a true center spiral (default: 0)" },
                        "turns": { "type": "number", "description": "Number of full revolutions (default: 3)" },
                        "segments_per_turn": { "type": "integer", "description": "Bézier segments per revolution for smoothness (default: 16)" },
                        "fill": { "type": "object" },
                        "stroke": { "type": "object" },
                        "layer_id": { "type": "string" },
                        "name": { "type": "string" }
                    },
                    "required": ["x","y","outer_radius"]
                }
            },
            {
                "name": "create_grid",
                "description": "Create a rectangular grid of lines. Specify position, size, and the number of rows and columns. The grid is drawn as a single path of open line subpaths.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "number", "description": "X coordinate of the top-left corner" },
                        "y": { "type": "number", "description": "Y coordinate of the top-left corner" },
                        "width": { "type": "number", "description": "Total grid width in document units" },
                        "height": { "type": "number", "description": "Total grid height in document units" },
                        "cols": { "type": "integer", "minimum": 1, "description": "Number of columns (default: 4)" },
                        "rows": { "type": "integer", "minimum": 1, "description": "Number of rows (default: 4)" },
                        "fill": { "type": "object" },
                        "stroke": { "type": "object" },
                        "layer_id": { "type": "string" },
                        "name": { "type": "string" }
                    },
                    "required": ["x","y","width","height"]
                }
            },
            {
                "name": "create_polar_grid",
                "description": "Create a polar (radial) grid centered at a point. Draws concentric circles and radial spokes.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "number", "description": "X coordinate of the center" },
                        "y": { "type": "number", "description": "Y coordinate of the center" },
                        "outer_radius": { "type": "number", "description": "Outer radius in document units" },
                        "inner_radius": { "type": "number", "description": "Inner radius (0 = full disk, default: 0)" },
                        "rings": { "type": "integer", "minimum": 1, "description": "Number of concentric rings (default: 4)" },
                        "sectors": { "type": "integer", "minimum": 1, "description": "Number of radial sectors/spokes (default: 8)" },
                        "fill": { "type": "object" },
                        "stroke": { "type": "object" },
                        "layer_id": { "type": "string" },
                        "name": { "type": "string" }
                    },
                    "required": ["x","y","outer_radius"]
                }
            },
            {
                "name": "create_text",
                "description": "Create a text node at a position. Use update_node to change content, font, size, or color after creation.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "content": { "type": "string", "description": "The text to display" },
                        "x": { "type": "number", "description": "X position in document space" },
                        "y": { "type": "number", "description": "Y position in document space" },
                        "font_family": { "type": "string", "description": "Font family name (default: sans-serif)" },
                        "font_size": { "type": "number", "description": "Font size in document units (default: 16)" },
                        "font_weight": { "type": "integer", "description": "Font weight 100–900 (default: 400)" },
                        "fill": { "type": "object", "description": "Fill colour — e.g. {\"type\":\"solid\",\"color\":\"#000000\"}" },
                        "stroke": { "type": "object", "description": "Stroke outline" },
                        "align": { "type": "string", "enum": ["left","center","right"], "description": "Text alignment (default: left)" },
                        "line_height": { "type": "number", "description": "Line height multiplier (default: 1.2). 1.0 = tight, 2.0 = double-spaced." },
                        "letter_spacing": { "type": "number", "description": "Letter spacing in document units (default: 0). Positive = wider." },
                        "layer_id": { "type": "string" },
                        "name": { "type": "string" },
                        "tags": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["content","x","y"]
                }
            },
            {
                "name": "place_image",
                "description": "Place a raster (pixel) image as a new layer — the Photoshop-style bitmap import. Supply either `path` (a file on disk: PNG/JPEG/WebP/…) or `data_base64` (base64-encoded image bytes). The image becomes a Raster node positioned at (x,y); edit it afterward with apply_adjustment / apply_filter / brush_stroke / transform_image.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to an image file on disk" },
                        "data_base64": { "type": "string", "description": "Base64-encoded image bytes (alternative to path)" },
                        "x": { "type": "number", "description": "X position in document space (default 0)" },
                        "y": { "type": "number", "description": "Y position in document space (default 0)" },
                        "name": { "type": "string" },
                        "layer_id": { "type": "string" }
                    }
                }
            },
            {
                "name": "create_raster_layer",
                "description": "Create a blank raster (pixel) layer of a given size — a transparent canvas to paint on, or filled with a solid color. Edit with brush_stroke, gradient_fill, apply_filter, etc.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "width": { "type": "integer", "description": "Width in pixels" },
                        "height": { "type": "integer", "description": "Height in pixels" },
                        "x": { "type": "number", "description": "X position in document space (default 0)" },
                        "y": { "type": "number", "description": "Y position in document space (default 0)" },
                        "fill": { "description": "Optional fill color: hex string (#rrggbb / #rrggbbaa) or [r,g,b,a] (0-255). Default: transparent." },
                        "name": { "type": "string" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["width","height"]
                }
            },
            {
                "name": "apply_adjustment",
                "description": "Apply a non-spatial color adjustment to a raster node (Photoshop Image > Adjustments). `adjustment` selects the operation; `params` carries its arguments; optional `selection` confines the edit to a region.\n\nadjustment ∈ {brightness_contrast(brightness,contrast -1..1), levels(in_black,in_white,gamma,out_black,out_white), curves(points:[[x,y],...] 0..1), exposure(stops), hue_saturation(hue deg,saturation -1..1,lightness -1..1), color_balance(shadows/midtones/highlights:[r,g,b] -1..1), vibrance(amount -1..1), desaturate, black_and_white(weights:[r,g,b]), invert, posterize(levels), threshold(level 0..1), photo_filter(color:[r,g,b] 0..1,density 0..1,preserve_luminosity), channel_mixer(red/green/blue:[r,g,b]), gradient_map(stops:[{pos,color}]), selective_color(target:[r,g,b],adjust:[r,g,b],range), shadows_highlights(shadows 0..1,highlights 0..1), gamma(gamma), auto_contrast, auto_levels}.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Raster node id or name" },
                        "adjustment": { "type": "string", "description": "Adjustment name (see description)" },
                        "params": { "type": "object", "description": "Adjustment-specific parameters" },
                        "selection": { "type": "object", "description": "Optional selection mask (see make-selection schema in apply_filter)" }
                    },
                    "required": ["node_id","adjustment"]
                }
            },
            {
                "name": "create_adjustment_layer",
                "description": "Create a NON-DESTRUCTIVE adjustment layer — a Photoshop adjustment layer. It carries no pixels; its adjustment is re-applied to the composite of every layer beneath it on each render, and the layer `opacity` controls the adjustment strength. `adjustment` and `params` use the exact same vocabulary as `apply_adjustment`. Move/reorder/hide it like any node; delete it to fully revert.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "adjustment": { "type": "string", "description": "Adjustment name (see apply_adjustment)" },
                        "params": { "type": "object", "description": "Adjustment-specific parameters" },
                        "opacity": { "type": "number", "description": "Adjustment strength 0..1 (default 1)" },
                        "name": { "type": "string" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["adjustment"]
                }
            },
            {
                "name": "apply_filter",
                "description": "Apply a spatial filter to a raster node (Photoshop Filter menu). `filter` selects the operation; `params` carries its arguments; optional `selection` confines the edit.\n\nfilter ∈ {gaussian_blur(radius), box_blur(radius), motion_blur(angle deg,distance px), sharpen(amount), unsharp_mask(radius,amount,threshold 0..255), median(radius), add_noise(amount 0..1,monochrome bool,seed), emboss, find_edges, mosaic(block px), high_pass(radius), surface_blur(radius,threshold), lens_blur(radius), smart_sharpen(amount,radius,threshold), reduce_noise(strength), clarity(amount), vignette(amount,feather), chromatic_aberration(amount)}.\n\nselection (shared by adjustment/filter/brush): {kind:'rect'|'ellipse'|'polygon'|'wand'|'color_range'|'whole', x,y,w,h | points:[[x,y]] | tolerance,color | feather,invert,grow,contract}.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string" },
                        "filter": { "type": "string", "description": "Filter name (see description)" },
                        "params": { "type": "object" },
                        "selection": { "type": "object" }
                    },
                    "required": ["node_id","filter"]
                }
            },
            {
                "name": "brush_stroke",
                "description": "Paint (or erase) a brush stroke along a polyline on a raster node. Points are in the image's local pixel space. mode 'paint' (default) composites `color`; mode 'erase' removes alpha.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string" },
                        "points": { "type": "array", "items": { "type": "array", "items": { "type": "number" } }, "description": "[[x,y],...] in image pixel space" },
                        "color": { "description": "Brush color: hex or [r,g,b,a]" },
                        "radius": { "type": "number", "description": "Brush radius in px (default 10)" },
                        "hardness": { "type": "number", "description": "0 soft .. 1 hard (default 0.8)" },
                        "flow": { "type": "number", "description": "Per-dab build-up 0..1 (default 1)" },
                        "opacity": { "type": "number", "description": "Stroke opacity 0..1 (default 1)" },
                        "mode": { "type": "string", "enum": ["paint","erase"], "description": "Default paint" },
                        "selection": { "type": "object" }
                    },
                    "required": ["node_id","points"]
                }
            },
            {
                "name": "bucket_fill",
                "description": "Flood-fill a contiguous region of a raster node from a seed pixel, by color tolerance (paint bucket).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string" },
                        "x": { "type": "integer" },
                        "y": { "type": "integer" },
                        "color": { "description": "Fill color: hex or [r,g,b,a]" },
                        "tolerance": { "type": "number", "description": "0..1 color tolerance (default 0)" }
                    },
                    "required": ["node_id","x","y","color"]
                }
            },
            {
                "name": "gradient_fill",
                "description": "Fill a raster node with a linear gradient from (x0,y0) to (x1,y1), optionally confined to a selection.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string" },
                        "x0": { "type": "number" }, "y0": { "type": "number" },
                        "x1": { "type": "number" }, "y1": { "type": "number" },
                        "color0": { "description": "Start color: hex or [r,g,b,a]" },
                        "color1": { "description": "End color: hex or [r,g,b,a]" },
                        "selection": { "type": "object" }
                    },
                    "required": ["node_id","x0","y0","x1","y1","color0","color1"]
                }
            },
            {
                "name": "transform_image",
                "description": "Geometric op on a raster node's pixels. op ∈ {crop(x,y,width,height), resize(width,height,filter:'nearest'|'bilinear'|'lanczos3'), resize_canvas(width,height,offset_x,offset_y), rotate90, rotate180, rotate270, flip_h, flip_v, rotate(angle deg)}.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string" },
                        "op": { "type": "string", "description": "Operation name (see description)" },
                        "params": { "type": "object" }
                    },
                    "required": ["node_id","op"]
                }
            },
            {
                "name": "set_layer_mask",
                "description": "Attach a non-destructive layer mask to a raster node, built from a selection spec (rect/ellipse/polygon/wand/color_range, with feather/invert/grow/contract). The mask gates the node's compositing without altering its pixels.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string" },
                        "selection": { "type": "object", "description": "Selection spec defining the mask" }
                    },
                    "required": ["node_id","selection"]
                }
            },
            {
                "name": "clear_layer_mask",
                "description": "Remove the layer mask from a raster node (fully reveal).",
                "inputSchema": {
                    "type": "object",
                    "properties": { "node_id": { "type": "string" } },
                    "required": ["node_id"]
                }
            },
            {
                "name": "remove_background",
                "description": "Detect the subject of a raster layer with a local on-device matting model (U²-Net-p via ONNX Runtime; one-time ~5 MB download, then offline) and apply the result as a non-destructive foreground layer mask — the background is hidden, not erased, and the edit is undoable. Intersects with any existing mask.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "node_id": { "type": "string", "description": "Raster node id or name" } },
                    "required": ["node_id"]
                }
            },
            {
                "name": "get_raster_info",
                "description": "Read-only: report a raster node's dimensions, whether it has a layer mask, its source file, and a 16-bucket luma histogram.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "node_id": { "type": "string" } },
                    "required": ["node_id"]
                }
            },
            {
                "name": "retouch",
                "description": "Photoshop retouching tools on a raster node. op ∈ {healing_brush(cx,cy,radius,src_dx,src_dy — frequency-separation seamless clone), spot_healing(cx,cy,radius — auto-heal a blemish from its surroundings), content_aware_fill(requires `selection` — inpaint the selected region from surrounding pixels), red_eye(cx,cy,radius), dust_and_scratches(radius,threshold)}.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string" },
                        "op": { "type": "string", "description": "Retouch operation (see description)" },
                        "params": { "type": "object" },
                        "selection": { "type": "object", "description": "Required for content_aware_fill" }
                    },
                    "required": ["node_id","op"]
                }
            },
            {
                "name": "liquify",
                "description": "Liquify / distortion on a raster node. op ∈ {push(cx,cy,dx,dy,radius,strength — forward warp), twirl(cx,cy,radius,angle), pucker(cx,cy,radius,amount), bloat(cx,cy,radius,amount), pinch(amount), spherize(amount), ripple(amplitude,wavelength), perspective(corners:[[x,y]×4] TL,TR,BR,BL)}.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string" },
                        "op": { "type": "string", "description": "Liquify/distort operation (see description)" },
                        "params": { "type": "object" },
                        "selection": { "type": "object" }
                    },
                    "required": ["node_id","op"]
                }
            },
            {
                "name": "update_node",
                "description": "Update properties of an existing node by ID. Text nodes also accept: content, font_family, font_size, font_weight, text_align.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string" },
                        "name": { "type": "string" },
                        "fill": { "type": "object", "description": "Fill — solid: {\"type\":\"solid\",\"color\":\"#rrggbb\"} | none: {\"type\":\"none\"} | linear: {\"type\":\"gradient\",\"gradient_type\":\"linear\",\"colors\":[\"#hex1\",\"#hex2\"],\"coords\":[x0,y0,x1,y1]} | radial: {\"type\":\"gradient\",\"gradient_type\":\"radial\",\"colors\":[\"#hex1\",\"#hex2\"],\"coords\":[cx,cy,r]} | fluid: {\"type\":\"fluid_gradient\",\"points\":[{\"x\":100,\"y\":50,\"color\":\"#ff0000\"},...],\"power\":2.0} | mesh: {\"type\":\"mesh_gradient\",\"rows\":2,\"cols\":2,\"vertices\":[{\"x\":0,\"y\":0,\"color\":\"#ff0000\"},...]}" },
                        "stroke": { "type": "object", "description": "Stroke outline. Fields: color (#RRGGBB), width (number), enabled (bool), opacity (0-1), line_cap (butt|round|square), line_join (miter|round|bevel), align (center|inside|outside), dash_array ([dash,gap,...] up to 6 values), dash_offset (number). Example: {\"color\":\"#000000\",\"width\":2,\"enabled\":true,\"dash_array\":[8,4]}" },
                        "opacity": { "type": "number", "minimum": 0, "maximum": 1 },
                        "visible": { "type": "boolean" },
                        "locked": { "type": "boolean", "description": "Lock the node so it cannot be selected or moved in the canvas" },
                        "blend_mode": { "type": "string" },
                        "tags": { "type": "array", "items": { "type": "string" } },
                        "content": { "type": "string", "description": "New text content (text nodes only)" },
                        "font_family": { "type": "string", "description": "Font family (text nodes only)" },
                        "font_size": { "type": "number", "description": "Font size in document units (text nodes only)" },
                        "font_weight": { "type": "integer", "description": "Font weight 100–900 (text nodes only)" },
                        "text_align": { "type": "string", "enum": ["left","center","right"], "description": "Text alignment (text nodes only)" }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "duplicate_nodes",
                "description": "Deep-clone one or more nodes, creating N offset copies. Groups are duplicated with all their descendants — every node in the subtree gets a fresh ID. All copies land in one undoable batch. Returns the IDs of the new root nodes.\n\nUse cases: repeating elements (stars, petals, grid cells), creating variations, building patterns without re-specifying styles.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 1,
                            "description": "IDs of the nodes to duplicate"
                        },
                        "count": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 100,
                            "default": 1,
                            "description": "Number of copies to create per source node. Copy N is offset by N × {offset}."
                        },
                        "offset": {
                            "type": "object",
                            "description": "Position shift applied per copy. Copy 1 shifts by 1×offset, copy 2 by 2×offset, etc. Default: {x: 10, y: 10}.",
                            "properties": {
                                "x": { "type": "number" },
                                "y": { "type": "number" }
                            },
                            "required": ["x", "y"]
                        },
                        "layer_id": {
                            "type": "string",
                            "description": "Target layer for the copies. Defaults to the source node's own layer."
                        }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "delete_nodes",
                "description": "Delete one or more nodes by ID",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "reorder_node",
                "description": "Change the z-order (stacking position) of a node within its layer. Use send_to_back / bring_to_front for absolute positioning, send_backward / bring_forward to step one place, or move_above / move_below with a relative_id to position relative to another node.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "ID of the node to reorder" },
                        "operation": {
                            "type": "string",
                            "enum": ["send_to_back","bring_to_front","send_backward","bring_forward","move_above","move_below"],
                            "description": "send_to_back = lowest z; bring_to_front = highest z; move_above/move_below require relative_id"
                        },
                        "relative_id": { "type": "string", "description": "Required for move_above / move_below — the reference node" }
                    },
                    "required": ["node_id","operation"]
                }
            },
            {
                "name": "group_nodes",
                "description": "Group two or more nodes into a single group node. All nodes must belong to the same layer. The group is inserted at the z-position of the bottom-most child.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "minItems": 2, "description": "IDs of nodes to group" },
                        "name": { "type": "string", "description": "Name for the new group (default: 'Group')" },
                        "layer_id": { "type": "string", "description": "Optional layer override" }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "ungroup_nodes",
                "description": "Dissolve a group node, returning its children to the layer at the group's former z-position.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "group_id": { "type": "string", "description": "ID of the group node to dissolve" }
                    },
                    "required": ["group_id"]
                }
            },
            {
                "name": "boolean_operation",
                "description": "Combine two path nodes using a boolean operation. The result inherits fill and stroke from the target node and is placed at the target's z-position. By default both originals are removed.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target_id": { "type": "string", "description": "Base shape — result inherits its style" },
                        "tool_id": { "type": "string", "description": "Cutting/combining shape (relevant for subtract: tool is subtracted FROM target)" },
                        "operation": {
                            "type": "string",
                            "enum": ["union","subtract","intersect","exclude"],
                            "description": "union = merge shapes; subtract = cut tool from target; intersect = keep overlap; exclude = remove overlap"
                        },
                        "keep_originals": { "type": "boolean", "description": "Keep original nodes alongside the result (default: false)" }
                    },
                    "required": ["target_id","tool_id","operation"]
                }
            },
            {
                "name": "add_anchor_points",
                "description": "Insert a new anchor point at the midpoint of every segment in the selected path node(s). Each pass doubles the anchor count. Non-path nodes are silently skipped.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "IDs of path nodes to subdivide" },
                        "passes":   { "type": "integer", "description": "Number of subdivision passes (default 1, max 8)" }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "delete_anchor_point",
                "description": "Remove specific anchor points from a path node by their zero-based BezPath element indices. The path is rebuilt with the specified elements removed. Use inspect_node to discover anchor count, or the Direct Select tool in the GUI to visually identify anchor indices. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Path node UUID or name" },
                        "anchor_indices": { "type": "array", "items": { "type": "integer" }, "description": "Zero-based indices of BezPath elements to remove" }
                    },
                    "required": ["node_id", "anchor_indices"]
                }
            },
            {
                "name": "zig_zag_path",
                "description": "Replace each segment of a path with a zig-zag (sharp corners) or smooth wave (bezier curves) pattern. Configurable amplitude and ridge count per segment. Useful for decorative borders, electrical symbols, water/wave effects, and organic textures. Destructive — modifies the path data directly. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Path node IDs to apply zig-zag to" },
                        "size": { "type": "number", "description": "Peak-to-peak amplitude in document units (default: 10)" },
                        "ridges_per_segment": { "type": "integer", "description": "Number of peaks per original path segment (default: 4, min: 1)" },
                        "smooth": { "type": "boolean", "description": "Use smooth bezier waves instead of sharp zigzag corners (default: false)" }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "pucker_bloat",
                "description": "Distort path nodes by displacing all anchor and control points radially from a center point.\n\nPositive strength = bloat (expand outward, like inflating). Negative strength = pucker (contract inward, like pulling toward center). Strength of 0.5 expands each point 50% further from center; -0.5 pulls each point 50% closer.\n\nCenter defaults to the path's centroid. Useful for organic deformations, icon styling, and decorative effects. Destructive — modifies path data. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Path node IDs to distort" },
                        "strength": { "type": "number", "description": "Distortion strength: positive = bloat, negative = pucker (default: 0.5)" },
                        "center_x": { "type": "number", "description": "X coordinate of distortion center (default: path centroid)" },
                        "center_y": { "type": "number", "description": "Y coordinate of distortion center (default: path centroid)" }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "create_parametric_shape",
                "description": "Create a closed path from a parametric mathematical equation. Five shape types:\n- `lissajous`: x = A·sin(a·t + δ), y = B·sin(b·t) — elegant figure-8 and knot curves\n- `superellipse`: |x/a|^n + |y/b|^n = 1 — from astroid (n=0.5) through ellipse (n=2) to squircle (n=4)\n- `rose`: r = cos(k·θ) — flower-like petals (odd k → k petals, even k → 2k petals)\n- `hypotrochoid`: rolling circle inside a larger circle — spirograph patterns\n- `epicycloid`: rolling circle outside a larger circle — epicycloid petals and curves",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "cx": { "type": "number", "description": "Center X" },
                        "cy": { "type": "number", "description": "Center Y" },
                        "shape_type": { "type": "string", "enum": ["lissajous", "superellipse", "rose", "hypotrochoid", "epicycloid"], "description": "Which parametric curve to generate" },
                        "radius": { "type": "number", "description": "Overall scale / outer radius (default: 80)" },
                        "ratio_x": { "type": "number", "description": "X semi-axis ratio (Lissajous/Superellipse, default: 1.0)" },
                        "ratio_y": { "type": "number", "description": "Y semi-axis ratio (Lissajous/Superellipse, default: 1.0)" },
                        "freq_a": { "type": "number", "description": "Lissajous: x-frequency a (default: 3)" },
                        "freq_b": { "type": "number", "description": "Lissajous: y-frequency b (default: 2)" },
                        "delta_deg": { "type": "number", "description": "Lissajous: phase offset δ in degrees (default: 90)" },
                        "exponent": { "type": "number", "description": "Superellipse: exponent n (default: 2.5; 2=ellipse, >2=squircle, <2=astroid-like)" },
                        "petals": { "type": "number", "description": "Rose: petal factor k (default: 5; odd k → k petals, even k → 2k petals)" },
                        "inner_ratio": { "type": "number", "description": "Hypotrochoid/Epicycloid: rolling circle radius as fraction of outer radius (default: 0.4)" },
                        "pen_ratio": { "type": "number", "description": "Hypotrochoid/Epicycloid: pen distance as fraction of rolling radius (default: 1.0)" },
                        "points": { "type": "integer", "description": "Sample points for the polyline path (default: 360, max: 4096)" },
                        "fill": { "type": "object" },
                        "stroke": { "type": "object" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["cx", "cy", "shape_type"]
                }
            },
            {
                "name": "create_heart",
                "description": "Create a heart shape using smooth cubic bezier curves. Defaults to red fill if no style specified. The cy coordinate is the bottom tip of the heart.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "cx": { "type": "number", "description": "Center X" },
                        "cy": { "type": "number", "description": "Bottom tip Y" },
                        "size": { "type": "number", "description": "Heart width (default: 60)" },
                        "fill": { "type": "object" },
                        "stroke": { "type": "object" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["cx", "cy"]
                }
            },
            {
                "name": "create_gear",
                "description": "Create a gear/cog shape with configurable tooth count, inner/outer radius, and center hole. Useful for mechanical icons, settings symbols, and technical illustrations.\n\nThe gear is a compound path with even-odd fill rule for the center hole.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "cx": { "type": "number", "description": "Center X" },
                        "cy": { "type": "number", "description": "Center Y" },
                        "outer_radius": { "type": "number", "description": "Tip of teeth radius (default: 50)" },
                        "inner_radius": { "type": "number", "description": "Base of teeth radius (default: 35)" },
                        "hole_radius": { "type": "number", "description": "Center hole radius (default: 10, 0 = no hole)" },
                        "teeth": { "type": "integer", "description": "Number of teeth (default: 12)" },
                        "fill": { "type": "object" },
                        "stroke": { "type": "object" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["cx", "cy"]
                }
            },
            {
                "name": "create_qr_code",
                "description": "Generate a native, resolution-independent QR code as a group of vectors (a compound path of all dark modules, plus an optional background). Encodes a URL/text and lets you style the modules while staying scannable — the three finder 'eyes' are always rendered recognizably so stylized codes still scan.\n\nStyles: square (classic), rounded (rounded squares), dot (circles), connected ('blob' — neighbouring modules merge with smooth joins). Dark modules take any fill (solid or gradient). Keep good dark-on-light contrast and use a higher error-correction level (q/h) when heavily stylized.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "data": { "type": "string", "description": "Content to encode (URL or text)" },
                        "module_shape": { "type": "string", "enum": ["square", "rounded", "dot", "connected"], "description": "Module style (default: square)" },
                        "ecc": { "type": "string", "enum": ["l", "m", "q", "h"], "description": "Error-correction level (default: m). Higher = more robust but denser." },
                        "radius": { "type": "number", "description": "Corner radius as a fraction of one module, 0..0.5 (default 0.4). Used by rounded/connected." },
                        "size": { "type": "number", "description": "Total artwork size in document units incl. quiet zone (default 200)" },
                        "quiet_zone": { "type": "integer", "description": "Quiet-zone margin in modules (default 4; keep >= 2 to stay scannable)" },
                        "fill": { "type": "object", "description": "Fill for the dark modules — solid or gradient (default solid black)" },
                        "background": { "type": "string", "description": "Background behind the code: a #rrggbb hex (default white) or 'none' for transparent" },
                        "x": { "type": "number", "description": "Top-left X (default 0)" },
                        "y": { "type": "number", "description": "Top-left Y (default 0)" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["data"]
                }
            },
            {
                "name": "tag_nodes",
                "description": "Batch add or remove tags on nodes. Tags are arbitrary strings used for querying with find_nodes.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Node IDs. Empty = use selection." },
                        "add": { "type": "array", "items": { "type": "string" }, "description": "Tags to add" },
                        "remove": { "type": "array", "items": { "type": "string" }, "description": "Tags to remove" }
                    }
                }
            },
            {
                "name": "sample_color_at",
                "description": "Sample the fill and stroke color of the topmost visible node at a canvas coordinate. Returns the node ID, fill color hex, stroke color hex, and opacity. Like an eyedropper for the MCP side.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "number", "description": "Canvas X coordinate" },
                        "y": { "type": "number", "description": "Canvas Y coordinate" }
                    },
                    "required": ["x", "y"]
                }
            },
            {
                "name": "set_active_layer",
                "description": "Set the active layer. New nodes created without an explicit layer_id will be placed on the active layer.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "layer_id": { "type": "string", "description": "Layer UUID or name" }
                    },
                    "required": ["layer_id"]
                }
            },
            {
                "name": "delete_layer",
                "description": "Delete a layer. By default, nodes are moved to the first remaining layer. Set delete_nodes=true to also remove all nodes. Cannot delete the last layer.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "layer_id": { "type": "string", "description": "Layer UUID or name" },
                        "delete_nodes": { "type": "boolean", "description": "Also delete all nodes on the layer (default: false — moves them)" }
                    },
                    "required": ["layer_id"]
                }
            },
            {
                "name": "move_to_layer",
                "description": "Move nodes to a different layer. Nodes are appended to the top of the target layer's z-order. Undoable.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Node IDs. Empty = use selection." },
                        "target_layer": { "type": "string", "description": "Target layer UUID or name" }
                    },
                    "required": ["target_layer"]
                }
            },
            {
                "name": "add_dimension_line",
                "description": "Add a technical dimension annotation between two points. Creates a grouped set of elements: extension lines, dimension line with arrowheads, and a distance text label.\n\nUseful for technical illustrations, architectural drawings, and precision documentation.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x1": { "type": "number", "description": "Start X" },
                        "y1": { "type": "number", "description": "Start Y" },
                        "x2": { "type": "number", "description": "End X" },
                        "y2": { "type": "number", "description": "End Y" },
                        "offset": { "type": "number", "description": "Distance of dimension line from measured points (default: 20)" },
                        "font_size": { "type": "number", "description": "Label font size (default: 12)" },
                        "color": { "type": "string", "description": "Color hex (default: #666666)" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["x1", "y1", "x2", "y2"]
                }
            },
            {
                "name": "reorder_layers",
                "description": "Change the stacking order of layers. Provide the complete layer order as an array of layer UUIDs (bottom to top). All existing layers must be included.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "layer_order": { "type": "array", "items": { "type": "string" }, "description": "New layer order (bottom to top)" }
                    },
                    "required": ["layer_order"]
                }
            },
            {
                "name": "set_selection",
                "description": "Set the active selection to specific node IDs. Replaces current selection unless additive=true. Empty node_ids clears selection.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Node IDs to select" },
                        "additive": { "type": "boolean", "description": "Add to existing selection (default: false = replace)" }
                    }
                }
            },
            {
                "name": "get_selection",
                "description": "Return the current selection — list of selected node IDs with name, kind, visibility, and lock state. Read-only.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "flatten_group",
                "description": "Recursively ungroup all nested groups into flat nodes on the parent layer. Unlike ungroup_nodes (single level), this flattens the entire group hierarchy. Useful for simplifying complex imported SVGs.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Group node IDs. Empty = use selection." }
                    }
                }
            },
            {
                "name": "center_on_canvas",
                "description": "Center selected nodes on the canvas without scaling. Translates all nodes so their combined bounding box is centered. Supports horizontal-only or vertical-only centering.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Node IDs. Empty = use selection." },
                        "horizontal": { "type": "boolean", "description": "Center horizontally (default: true)" },
                        "vertical": { "type": "boolean", "description": "Center vertically (default: true)" }
                    }
                }
            },
            {
                "name": "remove_fill",
                "description": "Remove the fill from selected nodes (set to none/transparent).",
                "inputSchema": { "type": "object", "properties": { "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Node IDs. Empty = use selection." } } }
            },
            {
                "name": "remove_stroke",
                "description": "Remove the stroke from selected nodes (set to none).",
                "inputSchema": { "type": "object", "properties": { "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Node IDs. Empty = use selection." } } }
            },
            {
                "name": "fit_to_canvas",
                "description": "Scale and center artwork to fit within the canvas bounds. Applies a uniform scale (never scales up) and centers the result. Useful after importing SVGs or when artwork extends beyond the artboard.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "padding": { "type": "number", "description": "Padding around edges (default: 10)" },
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Nodes to fit. Empty = all." }
                    }
                }
            },
            {
                "name": "create_scatter_plot",
                "description": "Create a scatter plot from X/Y data points. Points are auto-scaled to fit the plot area. Each point rendered as a filled circle.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "number", "description": "Plot area left X" },
                        "y": { "type": "number", "description": "Plot area bottom Y" },
                        "width": { "type": "number", "description": "Plot width (default: 300)" },
                        "height": { "type": "number", "description": "Plot height (default: 300)" },
                        "points": { "type": "array", "items": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 }, "description": "Data points as [x, y] pairs" },
                        "dot_radius": { "type": "number", "description": "Dot radius (default: 4)" },
                        "color": { "type": "string", "description": "Dot color hex (default: #4E79A7)" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["x", "y", "points"]
                }
            },
            {
                "name": "scatter_copies",
                "description": "Randomly scatter copies of a node within a rectangular area. Each copy gets a random position, optional random rotation and scale variation. Deterministic seed for reproducibility.\n\nUseful for confetti, stars, foliage, particle effects, and random textures.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Source node to copy" },
                        "count": { "type": "integer", "description": "Number of copies (default: 20)" },
                        "x": { "type": "number", "description": "Area left X" },
                        "y": { "type": "number", "description": "Area top Y" },
                        "width": { "type": "number", "description": "Area width" },
                        "height": { "type": "number", "description": "Area height" },
                        "rotation_range": { "type": "number", "description": "Random rotation range in degrees (default: 0)" },
                        "scale_range": { "type": "number", "description": "Scale variation range (default: 0)" },
                        "seed": { "type": "integer", "description": "Random seed (default: 42)" }
                    },
                    "required": ["node_id", "x", "y", "width", "height"]
                }
            },
            {
                "name": "create_line_chart",
                "description": "Create a line chart from one or more data series. Lines can be smooth (Catmull-Rom) or straight. Supports area fill under lines. Multiple series overlaid on the same axes.\n\nData is auto-scaled to fit the chart area. Y axis grows upward from the baseline.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "number", "description": "Left X" },
                        "y": { "type": "number", "description": "Baseline Y (bottom)" },
                        "width": { "type": "number", "description": "Chart width (default: 300)" },
                        "height": { "type": "number", "description": "Chart height (default: 200)" },
                        "series": { "type": "array", "items": { "type": "array", "items": { "type": "number" } }, "description": "Data series — each is an array of values" },
                        "colors": { "type": "array", "items": { "type": "string" }, "description": "Line colors hex" },
                        "stroke_width": { "type": "number", "description": "Line width (default: 2)" },
                        "smooth": { "type": "boolean", "description": "Smooth with Catmull-Rom (default: true)" },
                        "fill_area": { "type": "boolean", "description": "Fill area under lines (default: false)" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["x", "y", "series"]
                }
            },
            {
                "name": "create_bar_chart",
                "description": "Create a bar chart from data values. Bars are proportional to their values. Supports vertical (default) and horizontal orientation, configurable gap, colors, and labels. Bars are grouped.\n\nFor vertical charts, y is the baseline (bottom) and bars grow upward. For horizontal, x is the left edge and bars grow rightward.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "number", "description": "Left X (vertical) or baseline X (horizontal)" },
                        "y": { "type": "number", "description": "Bottom Y (vertical) or top Y (horizontal)" },
                        "width": { "type": "number", "description": "Chart width (default: 300)" },
                        "height": { "type": "number", "description": "Chart height (default: 200)" },
                        "values": { "type": "array", "items": { "type": "number" }, "description": "Data values" },
                        "colors": { "type": "array", "items": { "type": "string" }, "description": "Bar colors hex (cycles)" },
                        "labels": { "type": "array", "items": { "type": "string" }, "description": "Bar labels" },
                        "gap": { "type": "number", "description": "Gap between bars as fraction of bar width (default: 0.2)" },
                        "horizontal": { "type": "boolean", "description": "Horizontal bars (default: false)" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["x", "y", "values"]
                }
            },
            {
                "name": "create_stacked_bar_chart",
                "description": "Create a stacked bar or column chart from multiple data series. Each series is stacked on top of the previous one within each position. Useful for showing part-to-whole relationships across categories. Default Tableau-10 color palette.\n\nFor vertical charts (default), x/y is the bottom-left corner and bars grow upward. For horizontal, bars grow rightward.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "number", "description": "Left X" },
                        "y": { "type": "number", "description": "Bottom Y (vertical) or top Y (horizontal)" },
                        "width": { "type": "number", "description": "Chart width (default: 300)" },
                        "height": { "type": "number", "description": "Chart height (default: 200)" },
                        "series": {
                            "type": "array",
                            "items": { "type": "array", "items": { "type": "number" } },
                            "description": "Data series. Each series is one dataset. All series must have the same length (one value per stack position)."
                        },
                        "colors": { "type": "array", "items": { "type": "string" }, "description": "Series colors as hex (one per series, cycles)" },
                        "labels": { "type": "array", "items": { "type": "string" }, "description": "Labels for each stack position (column/bar)" },
                        "series_names": { "type": "array", "items": { "type": "string" }, "description": "Series names for node labeling" },
                        "gap": { "type": "number", "description": "Gap between stacks as fraction of bar width (default: 0.2)" },
                        "horizontal": { "type": "boolean", "description": "Horizontal bars (default: false = vertical columns)" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["x", "y", "series"]
                }
            },
            {
                "name": "create_pie_chart",
                "description": "Create a pie chart from data values. Each slice is proportional to its value. Supports solid pie and donut style (with inner_radius). Slices are grouped. Default Tableau-10 color palette.\n\nUseful for data visualization, infographics, and dashboards.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "cx": { "type": "number", "description": "Center X" },
                        "cy": { "type": "number", "description": "Center Y" },
                        "radius": { "type": "number", "description": "Outer radius (default: 80)" },
                        "values": { "type": "array", "items": { "type": "number" }, "description": "Data values — slice sizes are proportional" },
                        "colors": { "type": "array", "items": { "type": "string" }, "description": "Slice colors as hex (cycles if fewer than values)" },
                        "labels": { "type": "array", "items": { "type": "string" }, "description": "Slice labels (optional)" },
                        "inner_radius": { "type": "number", "description": "Inner radius for donut chart (default: 0 = solid pie)" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["cx", "cy", "values"]
                }
            },
            {
                "name": "create_radar_chart",
                "description": "Create a radar (spider) chart from multi-dimensional data. Each axis represents one dimension; each series is drawn as a polygon scaled to its values per axis. Supports filled semi-transparent overlays, configurable grid rings, and multiple overlapping series. Default Tableau-10 color palette.\n\nUseful for comparing profiles (skills, stats, attributes) across multiple subjects.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "cx": { "type": "number", "description": "Center X" },
                        "cy": { "type": "number", "description": "Center Y" },
                        "radius": { "type": "number", "description": "Outer radius (default: 100)" },
                        "series": {
                            "type": "array",
                            "items": { "type": "array", "items": { "type": "number" } },
                            "description": "Data series. Each series is an array of values, one per axis. All series must have the same length (≥ 3)."
                        },
                        "labels": { "type": "array", "items": { "type": "string" }, "description": "Axis labels, one per axis (optional)" },
                        "series_names": { "type": "array", "items": { "type": "string" }, "description": "Series names for node labeling (optional)" },
                        "colors": { "type": "array", "items": { "type": "string" }, "description": "Series fill/stroke colors as hex (cycles if fewer than series)" },
                        "stroke_width": { "type": "number", "description": "Stroke width for series polygons (default: 1.5)" },
                        "grid_rings": { "type": "number", "description": "Number of concentric grid rings (default: 4)" },
                        "fill_area": { "type": "boolean", "description": "Fill series polygons with semi-transparent color (default: true)" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["cx", "cy", "series"]
                }
            },
            {
                "name": "create_truchet_tiling",
                "description": "Generate a Truchet tiling — a grid of algorithmically arranged tiles where each tile is one of two orientations, chosen randomly from a seed. Creates organic, labyrinthine, or kaleidoscopic patterns depending on the tile style.\n\nStyles:\n- \"arcs\" (default): two quarter-circle arcs per tile — classic Truchet pattern\n- \"diagonals\": a straight diagonal line per tile — creates maze-like cross-hatch patterns\n- \"triangles\": a filled triangle per tile — creates woven/checkerboard-like patterns\n\nAll tiles are grouped into a single node.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x":            { "type": "number", "description": "Top-left X of the tiling area" },
                        "y":            { "type": "number", "description": "Top-left Y of the tiling area" },
                        "width":        { "type": "number", "description": "Width of the tiling area (default: 200)" },
                        "height":       { "type": "number", "description": "Height of the tiling area (default: 200)" },
                        "tile_size":    { "type": "number", "description": "Side length of each tile in px (default: 40, min: 4)" },
                        "style":        { "type": "string", "enum": ["arcs", "diagonals", "triangles"], "description": "Tile pattern style (default: arcs)" },
                        "seed":         { "type": "number", "description": "Random seed for reproducible patterns (default: 42)" },
                        "color":        { "type": "string", "description": "Stroke/fill color for tiles as hex (default: #1a1a2e)" },
                        "background":   { "type": "string", "description": "Background fill color as hex; if absent no background is added" },
                        "stroke_width": { "type": "number", "description": "Stroke width for arc/diagonal tiles (default: 2.0)" },
                        "layer_id":     { "type": "string" }
                    },
                    "required": ["x", "y"]
                }
            },
            {
                "name": "point_on_path",
                "description": "Sample one or more points along a path at specified fractions (0.0 = start, 1.0 = end). Returns the (x, y) coordinates and tangent angle at each position.\n\nUseful for:\n- Positioning elements at precise locations along curves\n- Computing tangent directions for text-on-path or object alignment\n- Measuring intermediate distances along a path",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Path node UUID or name" },
                        "t": { "type": "array", "items": { "type": "number" }, "description": "Position fractions along the path (0.0–1.0). Single value or array." }
                    },
                    "required": ["node_id", "t"]
                }
            },
            {
                "name": "create_speech_bubble",
                "description": "Create a speech bubble shape — a rounded rectangle with a triangular tail pointing to a specified location. Defaults to white fill with black stroke. Tail position is configurable.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "cx": { "type": "number", "description": "Bubble center X" },
                        "cy": { "type": "number", "description": "Bubble center Y" },
                        "width": { "type": "number", "description": "Bubble width (default: 120)" },
                        "height": { "type": "number", "description": "Bubble height (default: 60)" },
                        "corner_radius": { "type": "number", "description": "Corner radius (default: 15)" },
                        "tail_x": { "type": "number", "description": "Tail tip X (default: below-left of center)" },
                        "tail_y": { "type": "number", "description": "Tail tip Y" },
                        "tail_width": { "type": "number", "description": "Tail base width (default: 20)" },
                        "fill": { "type": "object" },
                        "stroke": { "type": "object" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["cx", "cy"]
                }
            },
            {
                "name": "set_visibility",
                "description": "Show or hide nodes. Omit `visible` to toggle current state. Hidden nodes are not rendered but remain in the document.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Node IDs. Empty = use selection." },
                        "visible": { "type": "boolean", "description": "Set visible. Omit to toggle." }
                    }
                }
            },
            {
                "name": "set_locked",
                "description": "Lock or unlock nodes. Locked nodes cannot be selected or modified in the GUI. Omit `locked` to toggle current state.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Node IDs. Empty = use selection." },
                        "locked": { "type": "boolean", "description": "Set locked. Omit to toggle." }
                    }
                }
            },
            {
                "name": "select_all",
                "description": "Select all nodes in the document, or all nodes on a specific layer.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "layer_id": { "type": "string", "description": "Only select nodes on this layer (UUID or name). Omit for all layers." }
                    }
                }
            },
            {
                "name": "deselect_all",
                "description": "Clear the selection (deselect all nodes).",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "set_blend_mode",
                "description": "Set blend mode on multiple nodes at once. All 16 blend modes supported: normal, multiply, screen, overlay, darken, lighten, color_dodge, color_burn, hard_light, soft_light, difference, exclusion, hue, saturation, color, luminosity.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Node IDs. Empty = use selection." },
                        "blend_mode": { "type": "string", "enum": ["normal","multiply","screen","overlay","darken","lighten","color_dodge","color_burn","hard_light","soft_light","difference","exclusion","hue","saturation","color","luminosity","linear_dodge","linear_burn","subtract","divide","vivid_light","linear_light","pin_light","hard_mix","darker_color","lighter_color"], "description": "Blend mode" }
                    },
                    "required": ["blend_mode"]
                }
            },
            {
                "name": "set_opacity",
                "description": "Set opacity on multiple nodes at once. More efficient than calling update_node individually for each.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Node IDs. Empty = use selection." },
                        "opacity": { "type": "number", "description": "Opacity 0.0–1.0" }
                    },
                    "required": ["opacity"]
                }
            },
            {
                "name": "randomize_colors",
                "description": "Assign random colors to selected nodes from a palette. If no palette provided, generates random vibrant colors. Useful for color exploration, generative art, and rapid prototyping.\n\nDifferent from recolor_artwork (which maps existing colors to nearest palette match). randomize_colors assigns completely random picks from the palette.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Node IDs. Empty = use selection." },
                        "palette": { "type": "array", "items": { "type": "string" }, "description": "Color palette as hex strings. Empty = auto-generate." },
                        "seed": { "type": "integer", "description": "Random seed (default: 42)" },
                        "fill": { "type": "boolean", "description": "Randomize fills (default: true)" },
                        "stroke": { "type": "boolean", "description": "Randomize strokes (default: false)" }
                    }
                }
            },
            {
                "name": "swap_fill_stroke",
                "description": "Swap the fill and stroke colors on selected nodes. The fill color becomes the stroke color and vice versa. Works with paths and text. Solid fills only — gradient fills become stroke black.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Node IDs. Empty = use selection." }
                    }
                }
            },
            {
                "name": "flip_nodes",
                "description": "Flip/mirror nodes horizontally or vertically around their bounding box center. Paths are flipped geometrically; text and groups are flipped via transform scale.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Node IDs. Empty = use selection." },
                        "axis": { "type": "string", "enum": ["horizontal", "vertical"], "description": "Flip axis" }
                    },
                    "required": ["axis"]
                }
            },
            {
                "name": "create_cross",
                "description": "Create a cross/plus shape. A 12-point polygon with configurable size, arm thickness, rotation, and style. Set rotation to 45° for an X shape.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "cx": { "type": "number", "description": "Center X" },
                        "cy": { "type": "number", "description": "Center Y" },
                        "size": { "type": "number", "description": "Total size (default: 60)" },
                        "thickness": { "type": "number", "description": "Arm thickness (default: 20)" },
                        "rotation": { "type": "number", "description": "Rotation in degrees (default: 0, use 45 for X)" },
                        "fill": { "type": "object" },
                        "stroke": { "type": "object" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["cx", "cy"]
                }
            },
            {
                "name": "measure_path",
                "description": "Measure a path's total arc length, anchor count, segment count, bounding box, and open/closed status. Read-only — does not modify the document.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Path node UUID or name" }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "measure_distance",
                "description": "Measure the distance between two points or two nodes. Returns distance, delta X/Y, and angle.\n\nEach target can be an [x, y] coordinate pair or a node UUID/name (uses the node's bounding box center).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "from": { "description": "Start: [x, y] array or node ID string" },
                        "to": { "description": "End: [x, y] array or node ID string" }
                    },
                    "required": ["from", "to"]
                }
            },
            {
                "name": "create_arrow_shape",
                "description": "Create a block arrow shape (chevron) with configurable dimensions and direction. The arrow has a triangular head and rectangular shaft.\n\nUseful for flowcharts, infographics, directional indicators, and UI elements.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "number", "description": "Arrow tip X coordinate" },
                        "y": { "type": "number", "description": "Arrow tip Y coordinate" },
                        "length": { "type": "number", "description": "Total arrow length (default: 100)" },
                        "head_width": { "type": "number", "description": "Arrow head width (default: 40)" },
                        "head_depth": { "type": "number", "description": "Head depth as fraction of length (default: 0.4)" },
                        "shaft_width": { "type": "number", "description": "Shaft width (default: 16)" },
                        "direction": { "type": "number", "description": "Direction in degrees, 0 = right (default: 0)" },
                        "fill": { "type": "object" },
                        "stroke": { "type": "object" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["x", "y"]
                }
            },
            {
                "name": "create_donut",
                "description": "Create a donut (ring/annulus) shape with configurable inner and outer radius. Supports full rings and partial arc segments (e.g., a pie chart slice with a hole).\n\nFull donuts use compound path with even-odd fill rule. Partial donuts create a closed wedge-shaped ring segment.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "cx": { "type": "number", "description": "Center X" },
                        "cy": { "type": "number", "description": "Center Y" },
                        "outer_radius": { "type": "number", "description": "Outer radius (default: 50)" },
                        "inner_radius": { "type": "number", "description": "Inner radius / hole size (default: 25)" },
                        "start_angle": { "type": "number", "description": "Start angle in degrees for partial arcs (default: 0)" },
                        "end_angle": { "type": "number", "description": "End angle in degrees (default: 360 = full ring)" },
                        "fill": { "type": "object", "description": "Fill style" },
                        "stroke": { "type": "object", "description": "Stroke style" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["cx", "cy"]
                }
            },
            {
                "name": "create_sunburst",
                "description": "Create a radial sunburst pattern — alternating filled wedges radiating from a center point. Classic retro/vintage effect.\n\nWedges are created as a single compound path with smooth arc edges. Configurable ray count, inner/outer radius, and color.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "cx": { "type": "number", "description": "Center X" },
                        "cy": { "type": "number", "description": "Center Y" },
                        "inner_radius": { "type": "number", "description": "Inner radius (default: 20). Set to 0 for no hole." },
                        "outer_radius": { "type": "number", "description": "Outer radius (default: 100)" },
                        "rays": { "type": "integer", "description": "Number of rays — half are filled (default: 24)" },
                        "color": { "type": "string", "description": "Wedge fill color hex (default: #FFD700 gold)" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["cx", "cy"]
                }
            },
            {
                "name": "create_wave_pattern",
                "description": "Generate a decorative wave/sine pattern as a compound stroke path. Creates multiple parallel sine waves with configurable wavelength, amplitude, and line count.\n\nUseful for water effects, hair/fur textures, topographic maps, decorative borders, and abstract backgrounds.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "number", "description": "Left edge X" },
                        "y": { "type": "number", "description": "Top edge Y" },
                        "width": { "type": "number", "description": "Pattern width" },
                        "height": { "type": "number", "description": "Pattern height" },
                        "lines": { "type": "integer", "description": "Number of wave lines (default: 8)" },
                        "wavelength": { "type": "number", "description": "Wavelength in document units (default: 40)" },
                        "amplitude": { "type": "number", "description": "Wave amplitude (default: 10)" },
                        "stroke": { "type": "object", "description": "Stroke style" },
                        "layer_id": { "type": "string" }
                    },
                    "required": ["x", "y", "width", "height"]
                }
            },
            {
                "name": "hatch_fill",
                "description": "Fill a path shape with parallel hatching lines clipped to the path boundary. Supports single-direction hatching or cross-hatching (two angles).\n\nUseful for engraving style, technical drawing shading, woodcut effects, and decorative fills. Lines are created as a separate stroke-only path on the same layer.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Path node IDs to fill with hatching" },
                        "spacing": { "type": "number", "description": "Spacing between lines (default: 5)" },
                        "angle": { "type": "number", "description": "Angle of hatch lines in degrees (default: 45)" },
                        "cross_angle": { "type": "number", "description": "Second angle for cross-hatching. Omit for single-direction." },
                        "stroke_width": { "type": "number", "description": "Line width (default: 1)" },
                        "color": { "type": "string", "description": "Line color hex (default: uses path fill color)" }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "stipple_fill",
                "description": "Fill a path shape with randomly placed dots (stipple effect). Uses rejection sampling to place dots inside the path boundary.\n\nThe original path is preserved — dots are added as a separate path on the same layer. Useful for halftone textures, pointillism, sand/grain effects, and decorative fills.\n\nDot color defaults to the path's solid fill color. Deterministic seed ensures reproducible results.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Path node IDs to fill with stipple dots" },
                        "count": { "type": "integer", "description": "Number of dots (default: 200)" },
                        "dot_radius": { "type": "number", "description": "Dot radius in document units (default: 1.5)" },
                        "color": { "type": "string", "description": "Dot color hex (default: uses path fill color)" },
                        "seed": { "type": "integer", "description": "Random seed for reproducibility (default: 42)" }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "add_drop_shadow",
                "description": "Add a drop shadow behind one or more nodes. Creates a duplicate of each node, offset and recolored to the shadow color, placed behind the original.\n\nThe shadow copy has its fill replaced with the shadow color and stroke removed. For groups, child colors are preserved as a solid-color silhouette. Works with paths, text, and groups.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Node IDs to add shadows to" },
                        "offset_x": { "type": "number", "description": "Shadow X offset (default: 5)" },
                        "offset_y": { "type": "number", "description": "Shadow Y offset (default: 5)" },
                        "color": { "type": "string", "description": "Shadow color hex (default: #000000)" },
                        "opacity": { "type": "number", "description": "Shadow opacity 0–1 (default: 0.4)" }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "transform_copies",
                "description": "Create N copies of a node with cumulative transform offsets. Each copy has the previous copy's transform plus the specified translation, rotation, and scale increments.\n\nPerfect for:\n- Radial patterns: rotate=30°, copies=11 → 12-spoke pattern\n- Step-and-repeat: translate_x=50, copies=9 → 10-column grid\n- Spiral scaling: rotate=20°, scale=0.9, copies=15 → shrinking spiral\n- Fade trails: opacity_step=0.8 → each copy 80% of previous opacity\n\nAll copies are placed on the same layer as the source.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Source node UUID or name to copy" },
                        "copies": { "type": "integer", "description": "Number of copies (default: 5)" },
                        "translate_x": { "type": "number", "description": "X offset per copy in document units (default: 0)" },
                        "translate_y": { "type": "number", "description": "Y offset per copy in document units (default: 0)" },
                        "rotate": { "type": "number", "description": "Rotation per copy in degrees (default: 0)" },
                        "scale": { "type": "number", "description": "Scale factor per copy (default: 1.0). 0.9 = shrink 10% each." },
                        "opacity_step": { "type": "number", "description": "Opacity multiplier per copy (default: 1.0). 0.8 = fade 20% each." }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "round_corners",
                "description": "Round sharp corners of path nodes by replacing each corner with a smooth quadratic bezier arc. The radius is automatically clamped so adjacent fillets never overlap.\n\nDifferent from smooth_path (Chaikin): round_corners inserts precise arc fillets at corners while preserving straight segments. Useful for UI element shapes, rounded rectangles, and softening angular artwork. Destructive — modifies path data. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Path node IDs to round" },
                        "radius": { "type": "number", "description": "Corner radius in document units (default: 10)" }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "warp_envelope",
                "description": "Apply an envelope warp distortion to path nodes using named presets. The path is deformed according to a mathematical envelope function.\n\nPresets:\n- arc: bend along a circular arc\n- bulge: expand from center outward\n- wave: sinusoidal wave deformation\n- flag: wave that increases from left to right\n- squeeze: compress horizontally in the middle\n- inflate: expand everything from center (softer than bulge)\n- fisheye: fisheye lens distortion\n\nFor best results, add_anchor_points first for smoother warping on low-polygon paths. Destructive — modifies path data. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Path node IDs to warp" },
                        "warp_type": { "type": "string", "enum": ["arc", "arc_lower", "arc_upper", "arch", "bulge", "wave", "flag", "squeeze", "inflate", "fisheye", "shell_lower", "shell_upper", "fish", "rise", "twist"], "description": "Warp preset name" },
                        "bend": { "type": "number", "description": "Primary bend amount, roughly -1 to 1 (default: 0.5). Negative reverses direction." },
                        "distort_h": { "type": "number", "description": "Horizontal distortion, roughly -1 to 1 (default: 0). Only affects some presets." },
                        "distort_v": { "type": "number", "description": "Vertical distortion, roughly -1 to 1 (default: 0). Only affects some presets." }
                    },
                    "required": ["node_ids", "warp_type"]
                }
            },
            {
                "name": "crystallize_path",
                "description": "Add sharp outward spike detail to path segments, creating star-like, crystal, or frost-like edges. Each segment is replaced with triangular spikes pointing outward from the path.\n\nConfigurable spike height (size) and number of spikes per segment (count). Destructive — modifies path data. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Path node IDs to crystallize" },
                        "size": { "type": "number", "description": "Height of each spike in document units (default: 10)" },
                        "count": { "type": "integer", "description": "Number of spikes per original segment (default: 3, min: 1)" }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "scallop_path",
                "description": "Replace each path segment with smooth inward-curving scallop arcs. Creates decorative scalloped edges, cloud-like shapes, and ornamental borders.\n\nPositive depth curves inward (toward the interior); negative depth curves outward. Multiple arcs per segment create finer scalloping. Destructive — modifies path data. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Path node IDs to scallop" },
                        "depth": { "type": "number", "description": "Depth of each scallop arc in document units (default: 10). Positive = inward." },
                        "count": { "type": "integer", "description": "Number of scallop arcs per original segment (default: 1, min: 1)" }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "blend_objects",
                "description": "Generate intermediate path nodes that interpolate between two paths in both shape (geometry) and fill color. Both source paths must have the same number of BezPath elements — use add_anchor_points to equalize if needed.\n\nThree step-count modes:\n- `steps` (default): fixed number of intermediate steps (default: 5)\n- `smooth_color: true`: auto-compute steps so each step changes color by ≤ 1/255 (Smooth Color mode)\n- `spacing`: steps = ceil(center_distance / spacing) — Specified Distance mode\n\nEach intermediate node has: geometry (linear interp), fill color (linear interp, solid fills only), opacity (interpolated), and position (translation interpolated).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id_a": { "type": "string", "description": "First (start) path node UUID or name" },
                        "node_id_b": { "type": "string", "description": "Second (end) path node UUID or name" },
                        "steps": { "type": "integer", "description": "Number of intermediate steps to generate (default: 5, min: 1). Ignored when smooth_color or spacing is set." },
                        "smooth_color": { "type": "boolean", "description": "Auto-compute steps so each step changes color by ≤ 1/255. When true, steps is ignored." },
                        "spacing": { "type": "number", "description": "Specified Distance mode: space blend steps by this many pixels. Steps = ceil(dist / spacing). When set, overrides steps and smooth_color." }
                    },
                    "required": ["node_id_a", "node_id_b"]
                }
            },
            {
                "name": "twirl_path",
                "description": "Rotate path points around a center with a spiral falloff — points near the center rotate more, creating a twirl/vortex effect. Useful for decorative spirals, logo flourishes, and abstract patterns.\n\nThe rotation angle decreases linearly from full at the center to zero at the outermost point. Add anchor points first for smoother results on paths with few segments. Destructive — modifies path data. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Path node IDs to twirl" },
                        "angle": { "type": "number", "description": "Rotation angle in degrees (positive = counter-clockwise). Default: 90" },
                        "center_x": { "type": "number", "description": "X coordinate of twirl center (default: path centroid)" },
                        "center_y": { "type": "number", "description": "Y coordinate of twirl center (default: path centroid)" }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "proportional_move_anchor",
                "description": "Move a path's anchor point(s) and pull nearby anchors along a falloff — Blender-style proportional editing on real vector anchors (topology is preserved; only positions change). The primary anchors move by the full (dx, dy); every other anchor within `spread` follows by a weighted fraction shaped by the falloff `curve`. Coordinates and `spread` are in the path's local space, matching the anchor positions from inspect_node. Destructive to geometry; single undoable step.\n\nUse anchor element indices from inspect_node / get_node. Great for organically reshaping, tapering, or bulging a run of points without dragging each one by hand.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Path node ID (UUID or name) to edit" },
                        "anchor_indices": { "type": "array", "items": { "type": "integer" }, "description": "Element indices of the primary anchor(s) to move directly (weight 1.0), from inspect_node/get_node" },
                        "dx": { "type": "number", "description": "Primary displacement in the path's local X (path units)" },
                        "dy": { "type": "number", "description": "Primary displacement in the path's local Y (path units)" },
                        "spread": { "type": "number", "description": "Falloff radius in path units — neighbours within this distance follow proportionally. Default: 120" },
                        "curve": { "type": "number", "description": "Falloff curve exponent: 1 = linear, >1 = sharp, <1 = soft/broad. Default: 2" }
                    },
                    "required": ["node_id", "anchor_indices", "dx", "dy"]
                }
            },
            {
                "name": "roughen_path",
                "description": "Displace path anchor and control points by random amounts to create a hand-drawn, organic, or grunge effect. Configurable maximum displacement (size), optional subdivision for extra detail, and deterministic seed for reproducible results.\n\nUse detail > 0 to add intermediate points before roughening — this creates finer texture on long straight segments. Destructive — modifies path data. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Path node IDs to roughen" },
                        "size": { "type": "number", "description": "Maximum displacement in document units (default: 5)" },
                        "detail": { "type": "integer", "description": "Subdivision passes before roughening — adds points for finer texture (default: 0)" },
                        "seed": { "type": "integer", "description": "Random seed for reproducible results (default: 42)" }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "clean_up",
                "description": "Remove degenerate content: stray points (paths with no drawing segments), unpainted objects (no visible fill or stroke), and empty text nodes. Use dry_run:true to preview without deleting.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "remove_stray_points": { "type": "boolean", "description": "Remove paths with no drawing segments (default true)" },
                        "remove_unpainted":    { "type": "boolean", "description": "Remove paths with no visible fill and no visible stroke (default true)" },
                        "remove_empty_text":   { "type": "boolean", "description": "Remove text nodes with empty or whitespace-only content (default true)" },
                        "dry_run":             { "type": "boolean", "description": "Preview what would be removed without deleting (default false)" }
                    }
                }
            },
            {
                "name": "join_paths",
                "description": "Close or join path nodes. With 1 node_id: appends ClosePath to every open subpath in the node (i.e. closes the path). With 2 node_ids: merges both paths into one by connecting their nearest open endpoints with a straight line segment; the result replaces the first node and the second node is deleted. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "1 or 2 path node IDs. 1 = close open subpaths; 2 = join the two paths into one.", "minItems": 1, "maxItems": 2 }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "pathfinder_crop",
                "description": "Clip all selected path nodes to the boundary of the frontmost selected node (highest z-order). Each back node is replaced by the intersection of its path with the frontmost path; the frontmost node is then removed. Useful for masking artwork to a crop shape without creating a clipping mask. All node transforms are baked into path coordinates before the operation. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Two or more path node IDs. The frontmost (highest z-order) is the crop boundary.", "minItems": 2 }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "pathfinder_minus_back",
                "description": "Subtract all back nodes from the frontmost selected path node (Illustrator's Minus Back). The frontmost node (highest z-order) has each back node's shape subtracted from it in sequence; the back nodes are removed. The frontmost node's fill/stroke style is preserved. All node transforms are baked before the operation. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Two or more path node IDs. All nodes except the frontmost are subtracted from the frontmost.", "minItems": 2 }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "pathfinder_minus_front",
                "description": "Subtract the frontmost selected path from every back node (Illustrator's Minus Front). The frontmost node (highest z-order) punches a hole in each back node; each back node is updated with back_path − front_path. The frontmost node is removed. Each back node's fill/stroke style is preserved. All transforms baked before the operation. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Two or more path node IDs. The frontmost is the cutter; all others have the front subtracted from them.", "minItems": 2 }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "pathfinder_trim",
                "description": "Remove hidden portions of each selected path node by subtracting all paths above it in z-order (Illustrator's Trim). Nodes are processed back-to-front; each node's path becomes its_path − union(all_paths_above). Strokes are disabled on all result nodes; fills are preserved. No nodes are removed. All transforms are baked before the operation. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Two or more path node IDs to trim.", "minItems": 2 }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "pathfinder_outline",
                "description": "Convert selected filled path nodes to stroked outlines (Illustrator's Outline). For each node: the solid fill color is moved to the stroke and the fill is set to none. Gradient fills fall back to black. Existing stroke width is preserved (or defaults to 1 pt). Path data is unchanged. Non-path nodes are silently skipped. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "One or more path node IDs to convert to outlines." }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "divide_objects_below",
                "description": "Use a selected path as a cutting edge to divide all path nodes beneath it in z-order. Each overlapping node below is split into two face nodes (inside the cutter, outside the cutter). Non-overlapping nodes are untouched. The cutter is always removed. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "UUID of the cutting path node (must be a path; will be removed after cutting)" }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "pathfinder_divide",
                "description": "Divide two overlapping path nodes at every edge where they intersect, producing up to three distinct colored face nodes: the region only in the back shape, the overlapping region, and the region only in the front shape. Both originals are removed and replaced by the face nodes. Transforms are baked before the operation. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "minItems": 2, "maxItems": 2, "description": "Exactly two path node IDs: [back_node_id, front_node_id]" },
                        "layer_id": { "type": "string", "description": "Layer for result nodes (default: back node's layer)" }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "pathfinder_merge",
                "description": "Trim all selected path nodes of hidden areas, then merge (union) any nodes that share the same solid fill color into a single combined shape (Illustrator's Merge). Like Trim, each node has the regions covered by nodes above it subtracted; unlike Trim, nodes with matching solid fill colors are then unioned together. Non-solid fills remain separate. Original nodes are replaced by the merged result nodes. Strokes are disabled on all results. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "minItems": 2, "description": "Two or more path node IDs (z-order resolved automatically)" },
                        "layer_id": { "type": "string", "description": "Layer for result nodes (default: backmost source node's layer)" }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "reverse_path_direction",
                "description": "Reverse the winding direction of one or more path nodes. For open paths this flips the travel direction; for closed paths it toggles the fill rule winding (relevant for self-intersecting shapes, brushes, and type-on-path). Non-path nodes are silently skipped.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "IDs of path nodes to reverse" }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "average_anchor_points",
                "description": "Reposition all on-curve anchor points in each selected path node to their average position on the chosen axis. 'horizontal' equalises X-coordinates, 'vertical' equalises Y-coordinates, 'both' (default) moves all anchors to the centroid. Bézier control handles shift with their owning anchor so local curve shape is preserved. Non-path nodes are silently skipped.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "IDs of path nodes to average" },
                        "axis":     { "type": "string", "enum": ["horizontal", "vertical", "both"], "description": "Which axis to average (default: both)" }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "get_node",
                "description": "Get full details of a node by ID or name",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string" },
                        "name": { "type": "string" }
                    }
                }
            },
            {
                "name": "find_nodes",
                "description": "Query nodes by tag, name, type, layer, visibility, or world-space region. All filters are optional and combine with AND. Empty call returns all nodes up to limit. Results are unordered. 'count' = nodes returned; check 'truncated' if limit was reached.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "tags":            { "type": "array",   "items": {"type":"string"}, "description": "Node must have ALL these tags" },
                        "tags_any":        { "type": "array",   "items": {"type":"string"}, "description": "Node must have ANY of these tags" },
                        "name_contains":   { "type": "string",  "description": "Case-insensitive substring match on node name" },
                        "node_type":       { "type": "string",  "enum": ["path","group","text"], "description": "Filter by node type" },
                        "layer_id":        { "type": "string",  "description": "Restrict to this layer UUID" },
                        "visible_only":    { "type": "boolean", "description": "Exclude invisible nodes (default: false)" },
                        "in_region": {
                            "type": "object",
                            "description": "World AABB filter. Path nodes whose transformed bounding box intersects this rect are included. Groups/text always pass.",
                            "properties": {
                                "x": {"type":"number"}, "y": {"type":"number"},
                                "width": {"type":"number"}, "height": {"type":"number"}
                            },
                            "required": ["x","y","width","height"]
                        },
                        "include_details": { "type": "boolean", "description": "Return full node JSON (default: false = minimal {id,name,type,tags,layer_id,visible})" },
                        "limit":           { "type": "integer", "description": "Max results (default: 200)", "default": 200 }
                    }
                }
            },
            {
                "name": "select_same",
                "description": "Select all document nodes that share a specific attribute value with the reference node. Updates the active selection. Useful for selecting all objects with the same fill color, stroke weight, opacity, etc. For color/weight/opacity comparisons a configurable tolerance is applied (default 0.01).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id":     { "type": "string",  "description": "UUID of the reference node whose attribute value is matched" },
                        "attribute":   { "type": "string",  "enum": ["fill_color", "stroke_color", "stroke_weight", "opacity", "blend_mode", "object_type"], "description": "Which attribute to match" },
                        "tolerance":   { "type": "number",  "description": "Allowed difference for numeric/color comparisons (default 0.01)" },
                        "include_self":{ "type": "boolean", "description": "Include the reference node itself in results (default true)" }
                    },
                    "required": ["node_id", "attribute"]
                }
            },
            {
                "name": "apply_transform",
                "description": "Apply a geometric transform to nodes",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" } },
                        "operation": { "type": "string", "enum": ["translate","rotate","scale","matrix","reflect_horizontal","reflect_vertical","shear"] },
                        "translate": { "type": "object" },
                        "rotate": { "type": "object" },
                        "scale": { "type": "object" },
                        "matrix": { "type": "array", "items": { "type": "number" }, "minItems": 6, "maxItems": 6 },
                        "shear": { "type": "object", "properties": { "shear_x": { "type": "number", "description": "Horizontal shear factor (x' = x + shear_x * y)" }, "shear_y": { "type": "number", "description": "Vertical shear factor (y' = shear_y * x + y)" }, "origin_x": { "type": "number" }, "origin_y": { "type": "number" } }, "required": ["shear_x"] }
                    },
                    "required": ["operation"]
                }
            },
            {
                "name": "get_document_info",
                "description": "Get a compact summary of the document: canvas dimensions, layer list (name, visibility, node count, template status), node counts by kind (path/text/group), unique font names, and unique solid fill colors. Faster than get_document_state for overview queries.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "get_document_state",
                "description": "Get the full document tree: layers, nodes, styles, transforms",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "include_path_data": { "type": "boolean" },
                        "layer_id": { "type": "string" }
                    }
                }
            },
            {
                "name": "save_document",
                "description": "Save the current in-memory document in Photonic's native .photon format, including its undo/history tree. Pass path to save-as and make it the current MCP document path; omit path to save to that current path. Parent directories are created automatically. Returns the absolute path and bytes written.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Optional destination path for save-as. Required when the document has no current path." }
                    },
                    "required": []
                }
            },
            {
                "name": "create_layer",
                "description": "Create a new layer in the document",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "position": { "type": "integer", "description": "Position in layer stack (0 = top/front; 1 = just below top; omit to add at top)" }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "collect_in_new_layer",
                "description": "Move a set of nodes into a newly created layer as a single undoable step. Group children are automatically resolved to their top-level ancestor.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 1,
                            "description": "IDs of nodes to collect (group children resolve to their top-level ancestor)"
                        },
                        "name": { "type": "string", "description": "Name for the new layer (default: \"Collected Layer\")" },
                        "position": { "type": "integer", "description": "Position in layer stack (0 = top/front; 1 = just below top; omit to add at top)" }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "release_to_layers",
                "description": "Move each node into its own newly created layer — the inverse of collect_in_new_layer. One layer is created per node; group children are resolved to their top-level ancestor before release. Layer names default to 'Layer 1', 'Layer 2', … but can be customised with name_prefix. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 1,
                            "description": "IDs of nodes to release. Each top-level node goes into its own new layer."
                        },
                        "name_prefix": {
                            "type": "string",
                            "description": "Prefix for new layer names. Layers are named '<prefix> 1', '<prefix> 2', … Default: 'Layer'."
                        }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "merge_layers",
                "description": "Merge two or more layers into one. All nodes from source layers are moved into the target layer (the first layer among those selected in document stack order). Empty source layers are then removed. Optional target_name renames the surviving layer. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "layer_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 2,
                            "description": "IDs of the layers to merge. The bottom-most layer in document order becomes the target; all others are merged into it and removed."
                        },
                        "target_name": {
                            "type": "string",
                            "description": "Optional new name for the surviving layer. Defaults to its existing name."
                        }
                    },
                    "required": ["layer_ids"]
                }
            },
            {
                "name": "flatten_artwork",
                "description": "Merge all layers in the document into one. The bottom-most layer becomes the target; all other layers are dissolved into it and removed. No-ops on a single-layer document. Optional target_name renames the surviving layer. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target_name": {
                            "type": "string",
                            "description": "Optional new name for the surviving layer. Defaults to the bottom-most layer's existing name."
                        }
                    }
                }
            },
            {
                "name": "update_layer",
                "description": "Update mutable metadata on a layer: rename it, change visibility, lock/unlock, set a color tag, mark as a template layer, or set the layer's opacity and blend mode. Only the fields you supply are changed; omitted fields keep their current values. Layer opacity and blend mode composite the whole layer as a unit against the layers beneath (like Photoshop/Illustrator). Template layers are locked reference layers used for tracing over (dimmed in the GUI). Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "layer_id": { "type": "string", "description": "UUID of the layer to update." },
                        "name": { "type": "string", "description": "New name for the layer." },
                        "visible": { "type": "boolean", "description": "Show or hide the layer." },
                        "locked": { "type": "boolean", "description": "Lock or unlock the layer." },
                        "is_template": { "type": "boolean", "description": "Mark as a template layer (locked, dimmed reference for tracing over artwork). Setting true also locks the layer." },
                        "print": { "type": "boolean", "description": "Include the layer in export/print output. false = visible on canvas but excluded from exports." },
                        "opacity": { "type": "number", "description": "Layer opacity 0.0–1.0; the layer composites as a unit at this opacity." },
                        "blend_mode": { "type": "string", "description": "Layer blend mode: normal, multiply, screen, overlay, darken, lighten, color_dodge, color_burn, hard_light, soft_light, difference, exclusion, hue, saturation, color, luminosity.", "enum": ["normal","multiply","screen","overlay","darken","lighten","color_dodge","color_burn","hard_light","soft_light","difference","exclusion","hue","saturation","color","luminosity","linear_dodge","linear_burn","subtract","divide","vivid_light","linear_light","pin_light","hard_mix","darker_color","lighter_color"] },
                        "color": {
                            "description": "Color tag as [r,g,b,a] floats 0.0–1.0. Pass null to clear.",
                            "oneOf": [
                                { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
                                { "type": "null" }
                            ]
                        }
                    },
                    "required": ["layer_id"]
                }
            },
            {
                "name": "align_nodes",
                "description": "Align or distribute multiple nodes by their bounding boxes. Alignment snaps each node's edge or center to a reference (selection bounds, canvas, or a key object). Distribution evenly spaces nodes along an axis — by default the two extreme nodes stay fixed; supply `spacing` to use an exact pixel gap instead. Groups are not supported (no bounds).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 2,
                            "description": "IDs of the nodes to align or distribute"
                        },
                        "operation": {
                            "type": "string",
                            "enum": [
                                "left", "center_horizontal", "right",
                                "top", "center_vertical", "bottom",
                                "distribute_horizontal", "distribute_vertical"
                            ],
                            "description": "left/center_horizontal/right — snap to horizontal reference edge or center. top/center_vertical/bottom — snap to vertical reference edge or center. distribute_horizontal/distribute_vertical — evenly space gaps between nodes along the axis (or use exact `spacing`)."
                        },
                        "anchor": {
                            "type": "string",
                            "enum": ["selection", "canvas", "key_object"],
                            "description": "Reference for alignment. selection (default) = combined bounding box of all specified nodes. canvas = document dimensions. key_object = use the bounding box of the node given in key_object_id as the fixed reference; the key object itself is not moved."
                        },
                        "key_object_id": {
                            "type": "string",
                            "description": "When anchor is key_object, the ID of the node to use as the fixed alignment reference. Must be one of the node_ids."
                        },
                        "spacing": {
                            "type": "number",
                            "description": "Exact pixel gap between adjacent node edges when using distribute_horizontal or distribute_vertical. The first node (leftmost / topmost) stays fixed; subsequent nodes are placed at prev_edge + spacing. Omit for equal-spacing mode (default)."
                        }
                    },
                    "required": ["node_ids", "operation"]
                }
            },
            {
                "name": "screenshot",
                "description": "Capture the current canvas as a PNG for visual inspection",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "undo",
                "description": "Undo the last operation(s)",
                "inputSchema": {
                    "type": "object",
                    "properties": { "steps": { "type": "integer", "default": 1 } }
                }
            },
            {
                "name": "redo",
                "description": "Redo previously undone operation(s)",
                "inputSchema": {
                    "type": "object",
                    "properties": { "steps": { "type": "integer", "default": 1 } }
                }
            },
            {
                "name": "style_transfer",
                "description": "Copy the visual style (fill, stroke, opacity, blend_mode) from one source node onto any number of target nodes in a single undoable step.\n\nUse cases: applying a reference palette to many shapes at once, making a set of icons consistent, pasting a complex gradient or stroke style without re-specifying it per node.\n\nfill and stroke only transfer when both source and target are path nodes. opacity and blend_mode transfer to all node types. Use the `properties` filter to copy a subset (e.g. fill only, or opacity+blend_mode only).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "source_id": {
                            "type": "string",
                            "description": "ID of the node whose style to copy"
                        },
                        "target_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 1,
                            "description": "IDs of the nodes that will receive the style"
                        },
                        "properties": {
                            "type": "array",
                            "items": {
                                "type": "string",
                                "enum": ["fill", "stroke", "opacity", "blend_mode"]
                            },
                            "description": "Which style properties to copy. Omit or pass an empty array to copy all four. Example: [\"fill\", \"stroke\"] copies only colour and outline, leaving opacity and blend_mode untouched."
                        }
                    },
                    "required": ["source_id", "target_ids"]
                }
            },
            {
                "name": "measure_nodes",
                "description": "Return the world-space bounding box and center of each node after applying its transform. Also returns the combined bounding box of all specified nodes. When exactly two nodes are provided, includes pairwise center-to-center distance and angle (0° = right, 90° = down).\n\nUse this whenever you need to know WHERE something actually is on canvas — e.g. before placing a new element next to an existing one, checking alignment, or computing spacing. Groups and text nodes return null bounds; use path nodes or pass children individually.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 1,
                            "description": "IDs of the nodes to measure"
                        }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "create_array",
                "description": "Repeat a node in a structured pattern — grid or radial. The source node stays in place; new copies are created around it in a single undoable step. Great for tile patterns, mandalas, icon grids, clock faces, and any repeating motif.\n\nGrid mode: source is cell (0,0); `rows × cols - 1` copies fill the remaining cells. Copies are translated by (col × col_stride, row × row_stride).\n\nRadial mode: source is instance 0; `count - 1` copies are placed at evenly-spaced angles around (center_x, center_y). Each copy is the source rotated around that centre by its angle, so the visual count (source + copies) = count.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id":   { "type": "string", "description": "ID of the source node to repeat" },
                        "mode":      { "type": "string", "enum": ["grid", "radial"], "description": "Layout mode" },
                        "rows":      { "type": "integer", "description": "(grid) Number of rows — source is row 0. Default 2." },
                        "cols":      { "type": "integer", "description": "(grid) Number of columns — source is col 0. Default 2." },
                        "col_stride":{ "type": "number",  "description": "(grid) Horizontal distance between column centres in px. Default 100." },
                        "row_stride":{ "type": "number",  "description": "(grid) Vertical distance between row centres in px. Default 100." },
                        "count":     { "type": "integer", "description": "(radial) Total instances including source (min 2, default 6). Creates count-1 new copies." },
                        "center_x":  { "type": "number",  "description": "(radial) X of rotation centre. Default 0." },
                        "center_y":  { "type": "number",  "description": "(radial) Y of rotation centre. Default 0." },
                        "start_angle_degrees": { "type": "number", "description": "(radial) Clockwise angle in degrees for the first copy relative to the source. Default 0 (evenly distributed)." },
                        "group_result": { "type": "boolean", "description": "Wrap source + all copies into a new group node. Default false." },
                        "layer_id":  { "type": "string",  "description": "Target layer UUID. Defaults to source node's layer." },
                        "name_prefix":{ "type": "string", "description": "Name prefix for copies, e.g. 'Petal' → 'Petal 1', 'Petal 2'. Defaults to the source node's name." }
                    },
                    "required": ["node_id", "mode"]
                }
            },
            {
                "name": "export_raster",
                "description": "Export the current canvas as a raster image (PNG, JPEG, WebP, GIF, or TIFF). Returns the image data as a base64-encoded string, OR — when `path` is given — writes the image to disk and returns only a small result (path + dimensions), keeping full-resolution PNGs off the socket.\n\nPNG is lossless with optional transparency. JPEG is lossy with configurable quality (1–100) and always has a white background. WebP is lossy with transparency support and configurable quality. TIFF is lossless with full RGBA support, suitable for print workflows. Use this to obtain a file-ready raster export without the GUI file menu.\n\nOptionally specify width/height to resize the output. If omitted, the capture uses the current canvas dimensions.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "format": {
                            "type": "string",
                            "enum": ["png", "jpeg", "webp", "gif", "tiff"],
                            "description": "Output format (default: png)"
                        },
                        "width": {
                            "type": "integer",
                            "description": "Output width in pixels. Omit to use current canvas width."
                        },
                        "height": {
                            "type": "integer",
                            "description": "Output height in pixels. Omit to use current canvas height."
                        },
                        "quality": {
                            "type": "integer",
                            "description": "JPEG/WebP quality 1–100 (default: 90 for JPEG, 80 for WebP). Ignored for PNG."
                        },
                        "path": {
                            "type": "string",
                            "description": "Write the encoded image to this filesystem path (parent dirs created) and return a small result (path + width/height) instead of base64. Omit to return base64 inline. Use this for full-resolution PNGs, whose base64 is too large for the MCP socket."
                        }
                    }
                }
            },
            {
                "name": "export_artboards",
                "description": "Export one or more artboards to raster images — one image per artboard. Returns base64 by default, OR — when `path` is given — writes each image to disk and returns only a small result (path + dimensions per artboard), keeping full-resolution PNGs off the socket. Unlike export_raster (which snapshots the live editor viewport), each artboard's rectangle is rendered off-screen at its own aspect ratio, so results are exact regardless of the current zoom/scroll. Select artboards by (precedence): all=true → every artboard; range=[start,end] → 1-based inclusive index range; artboards=[...] → a list of UUIDs, exact names, or 1-based index strings; otherwise the active artboard. Get ids/names/indices from list_artboards. Set bleed=true to include the document print bleed around each artboard (for print upload). Max 24 artboards per call; each side capped at 8192 px.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "artboards": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Specific artboards to export, each a UUID, an exact name, or a 1-based index string (\"1\", \"2\", …). Exported in the given order."
                        },
                        "range": {
                            "type": "array",
                            "items": { "type": "integer" },
                            "minItems": 2,
                            "maxItems": 2,
                            "description": "Inclusive 1-based index range [start, end] (e.g. [2, 5] exports artboards 2–5)."
                        },
                        "all": { "type": "boolean", "description": "Export every artboard in document order. Overrides range/artboards." },
                        "format": {
                            "type": "string",
                            "enum": ["png", "jpeg", "webp", "gif", "tiff"],
                            "description": "Output format (default: png)."
                        },
                        "scale": {
                            "type": "number",
                            "description": "Output pixels per document unit (default 1.0). A 400×300 artboard at scale 2 exports 800×600 px. Clamped to 0.05–8.0."
                        },
                        "transparent": {
                            "type": "boolean",
                            "description": "Transparent background instead of the artboard fill (default false). Ignored by JPEG (always opaque)."
                        },
                        "quality": {
                            "type": "integer",
                            "description": "JPEG/WebP quality 1–100 (default: 90 for JPEG, 80 for WebP). Ignored for PNG."
                        },
                        "bleed": {
                            "type": "boolean",
                            "description": "Expand each artboard's rectangle by the document print bleed (bleed_mm, at the export scale/DPI) on all four sides, so the output includes the bleed area for print upload. Output dimensions become artboard px + 2×bleed px. Default false (trim only)."
                        },
                        "path": {
                            "type": "string",
                            "description": "Write each artboard's image to disk (parent dirs created) and return a small result (path + dimensions) instead of base64. With multiple artboards, include a {name} or {index} placeholder (or an extension, before which -{index} is inserted); a single artboard is written to `path` verbatim. Omit to return base64 inline."
                        }
                    }
                }
            },
            {
                "name": "duplicate_layer",
                "description": "Duplicate a layer with all its nodes. Creates a copy of the layer and deep-clones every node with new IDs. Single undoable batch.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "layer_id": { "type": "string", "description": "Layer UUID or name to duplicate" },
                        "name": { "type": "string", "description": "Name for the copy (default: '<original> Copy')" }
                    },
                    "required": ["layer_id"]
                }
            },
            {
                "name": "resize_canvas",
                "description": "Resize the document canvas (artboard) to new dimensions. Does not scale existing artwork — only changes the canvas boundary. Undoable.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "width": { "type": "number", "description": "New canvas width" },
                        "height": { "type": "number", "description": "New canvas height" }
                    },
                    "required": ["width", "height"]
                }
            },
            {
                "name": "add_export_profile",
                "description": "Save a named export configuration to the document. Profiles store format and quality settings so you can re-export with consistent settings using run_export_profile. If a profile with the same name exists it is replaced.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name":         { "type": "string",  "description": "Unique profile name." },
                        "format":       { "type": "string",  "enum": ["svg", "png", "jpeg", "webp"], "description": "Target export format." },
                        "width":        { "type": "integer", "description": "Raster-only: output pixel width." },
                        "height":       { "type": "integer", "description": "Raster-only: output pixel height." },
                        "semantic_ids": { "type": "boolean", "description": "SVG-only: emit semantic id attributes (default: true)." },
                        "precision":    { "type": "integer", "minimum": 1, "maximum": 6, "description": "SVG-only: coordinate decimal precision (default: 4)." }
                    },
                    "required": ["name", "format"]
                }
            },
            {
                "name": "list_export_profiles",
                "description": "List all named export profiles stored in the document.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "remove_export_profile",
                "description": "Delete a named export profile from the document.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the profile to remove." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "run_export_profile",
                "description": "Execute a named export profile and return the export data. For SVG profiles, returns the SVG markup. For raster profiles, returns base64-encoded image data.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the profile to run." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "export_svg",
                "description": "Export the entire document as an SVG string. Returns the raw SVG markup that can be saved as a .svg file or pasted directly into any SVG-aware tool.\n\nThe output starts with <!-- photonic-svg-v1 --> for pipeline stability. By default, every node and layer element receives an id attribute derived from its name (slugified, deduplicated), making the SVG immediately usable in CSS, JavaScript, and developer handoff.\n\nUse this to:\n- Verify exactly what the canvas looks like as markup after a sequence of drawing operations\n- Get export-ready SVG without using the GUI file menu\n- Inspect gradient definitions, path data, transforms, and layer structure as emitted XML\n\nThe returned SVG reflects all visible layers in draw order with correct transforms. Hidden layers and nodes with visible=false are omitted.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "inner_only": {
                            "type": "boolean",
                            "description": "Return only the inner SVG body without the outer <svg> wrapper (default: false)"
                        },
                        "semantic_ids": {
                            "type": "boolean",
                            "description": "Emit slugified node/layer names as id attributes (default: true). Set to false to suppress id attributes on all elements."
                        },
                        "precision": {
                            "type": "integer",
                            "description": "Decimal places for SVG dimension and viewBox values, clamped 1–6 (default: 4). Use 2 for smaller output, 6 for maximum fidelity."
                        }
                    }
                }
            },
            {
                "name": "export_pdf",
                "description": "Export the document as a vector PDF, at its physical size for the document DPI. Returns the PDF bytes as base64 in `data_base64`; also writes to `path` when given.\n\nVector paths with solid colours, transforms and nesting (gradients approximate to their first stop). With `color_mode: \"cmyk\"` the output is a print-ready PDF/X-1a:2001 file: DeviceCMYK via an ICC profile, embedded GTS_PDFX OutputIntent, PDF 1.3, and MediaBox/TrimBox/BleedBox from the document bleed. `outline_text` converts every glyph to a vector path (zero font dependencies); `marks` renders trim + registration marks.\n\nPER-ARTBOARD / MULTI-PAGE: set `all`, `range`, or `artboards` to emit one page per artboard, each clipped to its rectangle + bleed with its own page boxes and marks — a single multi-page PDF by default, or one file per artboard with `separate_files: true` (needs a `path` template). This is the native card-batch path; without any selector the whole canvas is one page. Note: documents that use layer opacity/blend still emit transparency (not yet flattened for strict X-1a).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "background": {
                            "type": "string",
                            "description": "Optional page background colour, e.g. \"#ffffff\". Omit for an unpainted (white-in-viewers) page."
                        },
                        "path": {
                            "type": "string",
                            "description": "Also write the exported PDF to this filesystem path. The base64 payload is still returned."
                        },
                        "outline_text": {
                            "type": "boolean",
                            "description": "Convert text nodes to vector outlines (zero font dependencies). Default false."
                        },
                        "marks": {
                            "type": "boolean",
                            "description": "Render trim and registration marks in the bleed/slug area. Default false."
                        },
                        "color_mode": {
                            "type": "string",
                            "enum": ["rgb", "cmyk"],
                            "description": "Export colour model. \"rgb\" (default) preserves on-screen colours. \"cmyk\" separates to DeviceCMYK via the ICC profile and emits a PDF/X-1a file. When omitted, the document's own color_mode is used."
                        },
                        "profile": {
                            "type": "string",
                            "description": "Path to a CMYK ICC profile for colour conversion and the OutputIntent (defaults to the bundled Coated FOGRA39 when color_mode is cmyk)."
                        },
                        "artboards": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Per-artboard export: specific artboards, each a UUID, an exact name, or a 1-based index string (\"1\", \"2\"). One clipped page per artboard, in this order. Get ids/names from list_artboards."
                        },
                        "all": {
                            "type": "boolean",
                            "description": "Export every artboard, one page each (overrides range/artboards)."
                        },
                        "range": {
                            "type": "array",
                            "items": { "type": "integer" },
                            "minItems": 2,
                            "maxItems": 2,
                            "description": "Export a 1-based inclusive artboard index range [start, end], one page each."
                        },
                        "separate_files": {
                            "type": "boolean",
                            "description": "With a multi-artboard selection, write one file PER artboard instead of a single multi-page PDF. Requires `path` with a {name} or {index} placeholder (or a filename, before whose extension -{index} is inserted)."
                        }
                    }
                }
            },
            {
                "name": "export_selection_as_svg",
                "description": "Export specific nodes (or the current selection) as a clean, minimal SVG. Each node's name is slugified into the SVG id attribute, making the output immediately pasteable into HTML or React. Identical gradient/pattern paints are deduplicated into a single shared <defs> entry. Coordinates and path data are rounded to `precision` decimals (default 4) so exports stay compact. Use `normalize: \"square\"` to frame the icon in a uniform centered square viewBox (so a set of icons all export at the same aspect ratio and scale). Optionally wrap the output in a React functional component.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Node IDs to export. If omitted or empty, uses the current document selection."
                        },
                        "as_react_component": {
                            "type": "boolean",
                            "description": "Wrap the SVG in a TypeScript React functional component (default: false)"
                        },
                        "component_name": {
                            "type": "string",
                            "description": "Component name when as_react_component is true (default: 'SvgIcon')"
                        },
                        "precision": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 6,
                            "description": "Decimal places for coordinates and path data (default: 4). Prevents 15-decimal path bloat."
                        },
                        "normalize": {
                            "type": "string",
                            "enum": ["tight", "square"],
                            "description": "viewBox framing. 'tight' (default) = union bounding box; 'square' = uniform centered square so a set of icons frames identically."
                        },
                        "pad": {
                            "type": "number",
                            "description": "Padding as a fraction of the square side when normalize='square' (e.g. 0.1 = 10%). Default 0."
                        }
                    }
                }
            },
            {
                "name": "export_icon_set",
                "description": "Batch-export a set of icons to normalized .svg files in a single call — the canonical icon-pipeline workflow. Given a list of {name, node_ids} entries (or, if omitted, every top-level group in the document named by its group name), each icon is exported with a uniform square viewBox (so the whole set renders at a consistent scale) and compact rounded path data. If `out_dir` is given the .svg files are written to disk (one per icon, filenames slugified + de-duplicated); otherwise each icon's SVG string is returned inline. Replaces the drop-out-to-a-Python-script post-pass for uniform icon sizing.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "icons": {
                            "type": "array",
                            "description": "Icons to export. Omit to export every top-level group in the document.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "name": { "type": "string", "description": "Output file name (a .svg extension is added if absent)." },
                                    "node_ids": {
                                        "type": "array",
                                        "items": { "type": "string" },
                                        "description": "Node IDs comprising this icon (a single group id is fine)."
                                    }
                                },
                                "required": ["name", "node_ids"]
                            }
                        },
                        "out_dir": {
                            "type": "string",
                            "description": "Directory to write the .svg files into. If omitted, SVGs are returned inline instead of written to disk."
                        },
                        "precision": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 6,
                            "description": "Decimal places for coordinates and path data (default: 4)."
                        },
                        "normalize": {
                            "type": "string",
                            "enum": ["tight", "square"],
                            "description": "viewBox framing (default: 'square' for uniform icon sizing)."
                        },
                        "pad": {
                            "type": "number",
                            "description": "Padding fraction for square normalization (default: 0.1)."
                        }
                    }
                }
            },
            {
                "name": "preview_selection",
                "description": "Render the selection (or given nodes) at target display sizes over light AND dark backgrounds, returned as a single contact-sheet PNG (base64 in data.data_base64). Icons live or die at their real size and against their real surface — this closes that loop without round-tripping through an app. Rows = backgrounds, columns = sizes; each icon is centered in a square cell (aspect ratio preserved). Only the selected nodes are drawn (others hidden), so neighbors don't bleed in.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Nodes to preview. If omitted or empty, uses the current selection."
                        },
                        "sizes": {
                            "type": "array",
                            "items": { "type": "integer", "minimum": 1, "maximum": 1024 },
                            "description": "Target pixel sizes (default: [24, 32, 48])."
                        },
                        "backgrounds": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Background swatch hex colors to composite over (default: [\"#ffffff\", \"#0b0b12\"])."
                        },
                        "pad": {
                            "type": "number",
                            "description": "Padding as a fraction of the icon's square size (default: 0.15)."
                        }
                    }
                }
            },
            {
                "name": "set_node_size",
                "description": "Resize a node to exact pixel dimensions in a single undoable step — no manual scale-factor arithmetic required.\n\nInternally this tool:\n1. Computes the node's current world-space bounding box (equivalent to `measure_nodes`)\n2. Derives the x/y scale factors needed to reach the requested dimensions\n3. Composes those scales onto the existing node transform, anchored at the chosen corner or edge\n\nThis eliminates the common two-round-trip pattern of `measure_nodes` → compute → `apply_transform`.\n\nTips:\n- Omit `height` and pass only `width` (with `maintain_aspect_ratio: true`) to scale proportionally\n- Use `anchor: \"center\"` when you want the shape to grow or shrink symmetrically\n- Use `anchor: \"top_left\"` (the default) when you want the position to stay fixed\n- Works on any path node; groups and text nodes with no geometry return an error\n\nReturns the previous and new dimensions and the scale factors applied.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": {
                            "type": "string",
                            "description": "ID of the node to resize"
                        },
                        "width": {
                            "type": "number",
                            "description": "Target width in pixels (must be > 0). Omit to derive from height when maintain_aspect_ratio is true."
                        },
                        "height": {
                            "type": "number",
                            "description": "Target height in pixels (must be > 0). Omit to derive from width when maintain_aspect_ratio is true."
                        },
                        "maintain_aspect_ratio": {
                            "type": "boolean",
                            "description": "When true and both dimensions given: fit inside the requested box without distortion (uses the smaller scale factor). When true and only one dimension given: scale the other axis proportionally. Default: false."
                        },
                        "anchor": {
                            "type": "string",
                            "enum": ["top_left","top_center","top_right","left_center","center","right_center","bottom_left","bottom_center","bottom_right"],
                            "description": "The point on the bounding box that stays fixed while the rest of the shape scales. Default: \"top_left\"."
                        }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "find_replace_style",
                "description": "Search every node for a matching fill or stroke color and replace those colors — and optionally node-level opacity — in a single undoable batch.\n\nThis is the 'Find & Replace' for color. Instead of calling get_document_state → iterating nodes → calling update_node for each match (N round-trips, N undo steps), a single find_replace_style call handles the entire document atomically.\n\nTypical use cases:\n- Brand refresh: swap old brand color for new across the whole file in one call\n- Design audit: dry_run=true to see every node using a given color before committing\n- Bulk opacity change: set all red fills to 50% opacity\n- Near-match cleanup: use color_tolerance=0.05 to catch slightly off-brand colors\n\nGradient support: matching checks solid fills AND individual stop colors inside linear, radial, fluid, and mesh gradients. Only matching stops are replaced; others are untouched.\n\nRequires at least one search criterion (fill_color or stroke_color) and at least one replacement (new_fill_color, new_stroke_color, or new_opacity). Returns the list of changed nodes and exactly what changed on each.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "fill_color": {
                            "type": "string",
                            "description": "Hex color to search for in fills — solid color or any gradient stop. e.g. '#FF0000'"
                        },
                        "stroke_color": {
                            "type": "string",
                            "description": "Hex color to search for in enabled strokes. e.g. '#000000'"
                        },
                        "color_tolerance": {
                            "type": "number",
                            "minimum": 0,
                            "maximum": 1,
                            "description": "Match threshold. 0.0 = exact match (default). 0.05 = visually near-identical. 1.0 = any color. Normalized Euclidean distance in linear RGB."
                        },
                        "new_fill_color": {
                            "type": "string",
                            "description": "Replace every matched fill color (solid or gradient stop) with this hex color."
                        },
                        "new_stroke_color": {
                            "type": "string",
                            "description": "Replace every matched stroke color with this hex color."
                        },
                        "new_opacity": {
                            "type": "number",
                            "minimum": 0,
                            "maximum": 1,
                            "description": "Set node-level opacity to this value for every matched node."
                        },
                        "layer_id": {
                            "type": "string",
                            "description": "Restrict the search to nodes on this layer UUID. Omit to search the entire document."
                        },
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Restrict the search to these specific node IDs. Useful for scoped updates without touching the rest of the file."
                        },
                        "dry_run": {
                            "type": "boolean",
                            "description": "When true, return what would change without mutating the document. Use before large batch operations to confirm scope. Default: false."
                        }
                    }
                }
            },
            {
                "name": "find_replace_text",
                "description": "Search and replace text content across text nodes. Supports plain-string and regular-expression matching with optional case sensitivity. Use dry_run: true to preview matches without applying changes. Returns a list of changed nodes with their old and new content.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "find": {
                            "type": "string",
                            "description": "Text to search for. Plain string by default; treated as a regex when regex: true."
                        },
                        "replace": {
                            "type": "string",
                            "description": "Replacement string. When regex: true, capture group back-references ($1, $2, …) are supported."
                        },
                        "regex": {
                            "type": "boolean",
                            "description": "Treat find as a regular expression. Default: false."
                        },
                        "case_sensitive": {
                            "type": "boolean",
                            "description": "Case-sensitive match. Default: true."
                        },
                        "dry_run": {
                            "type": "boolean",
                            "description": "Preview matches without applying changes. Default: false."
                        },
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Scope to specific text node UUIDs. Omit to search all text nodes in the document."
                        }
                    },
                    "required": ["find", "replace"]
                }
            },
            {
                "name": "layout_nodes",
                "description": "Rearrange a set of existing nodes using a spatial layout algorithm — no manual coordinate math required.\n\nFour layouts are available:\n- `grid` — pack nodes into a uniform grid. Columns default to ceil(sqrt(N)); cell size defaults to the widest × tallest node. Nodes are centred inside their cell.\n- `circle` — distribute nodes evenly around a circle at a given centre and radius.\n- `stack_horizontal` — place nodes left-to-right with a gap, with optional cross-axis alignment (top / centre / bottom).\n- `stack_vertical` — place nodes top-to-bottom with a gap, with optional cross-axis alignment (left / centre / right).\n\nAll layout origins default to the current top-left corner of the combined selection so the group stays in place unless you explicitly move it.\n\nThis complements `create_array` (which duplicates one node) and `align_nodes` (which distributes along a single axis). Use `layout_nodes` whenever you have N *existing* nodes that need 2-D spatial organisation.\n\nReturns the number of nodes moved. The operation is a single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "IDs of the nodes to rearrange. Order determines placement (left-to-right, top-to-bottom for grid/stack; clockwise from start_angle for circle)."
                        },
                        "layout": {
                            "type": "string",
                            "enum": ["grid", "circle", "stack_horizontal", "stack_vertical"],
                            "description": "Layout algorithm to apply."
                        },
                        "x": {
                            "type": "number",
                            "description": "X coordinate of the layout origin. Defaults to the left edge of the current selection."
                        },
                        "y": {
                            "type": "number",
                            "description": "Y coordinate of the layout origin. Defaults to the top edge of the current selection."
                        },
                        "columns": {
                            "type": "integer",
                            "description": "(grid) Number of columns. Defaults to ceil(sqrt(N))."
                        },
                        "gap_x": {
                            "type": "number",
                            "description": "(grid) Horizontal gap between cells in pixels. Default: 20."
                        },
                        "gap_y": {
                            "type": "number",
                            "description": "(grid) Vertical gap between cells in pixels. Default: 20."
                        },
                        "cell_width": {
                            "type": "number",
                            "description": "(grid) Fixed cell width in pixels. Defaults to the widest node's width."
                        },
                        "cell_height": {
                            "type": "number",
                            "description": "(grid) Fixed cell height in pixels. Defaults to the tallest node's height."
                        },
                        "cx": {
                            "type": "number",
                            "description": "(circle) X of the circle centre. Defaults to the combined bounding-box centre."
                        },
                        "cy": {
                            "type": "number",
                            "description": "(circle) Y of the circle centre. Defaults to the combined bounding-box centre."
                        },
                        "radius": {
                            "type": "number",
                            "description": "(circle) Radius in pixels. Default: 200."
                        },
                        "start_angle": {
                            "type": "number",
                            "description": "(circle) Angle in degrees for the first node, measured clockwise from the positive X axis. Default: 0 (rightmost point)."
                        },
                        "gap": {
                            "type": "number",
                            "description": "(stack_horizontal / stack_vertical) Gap between successive nodes in pixels. Default: 20."
                        },
                        "align": {
                            "type": "string",
                            "enum": ["start", "center", "end"],
                            "description": "(stack_horizontal / stack_vertical) Cross-axis alignment. For stack_horizontal: top/centre/bottom. For stack_vertical: left/centre/right. Default: start."
                        }
                    },
                    "required": ["node_ids", "layout"]
                }
            },
            {
                "name": "inspect_node",
                "description": "Return computed geometry and structure metrics for a single node — values that go beyond what get_node provides.\n\n**Path nodes:** world-space and local bounding box, perimeter length, enclosed area, centroid (world-space center of bounding box), and anchor-point count.\n\n**Group nodes:** direct child count, total descendant count, sum of all anchor points across descendant paths, and sorted lists of unique solid fill and stroke colors (hex strings) used anywhere in the group hierarchy.\n\n**Text nodes:** line count (split by newlines), character count, font family, font size, and font weight.\n\nAll node types include a `world_bounds` object with `x`, `y`, `width`, and `height` in document (world) space. Returns an error if no node matches the provided ID or name.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": "string",
                            "description": "Node ID (UUID string) or node name. UUID is matched first; falls back to name search if parsing fails."
                        }
                    },
                    "required": ["id"]
                }
            },
            {
                "name": "add_annotation",
                "description": "Attach a non-printing text comment to a node or to the document as a whole.\n\nAnnotations are stored in the `.photonic` file but are completely invisible in all export formats (SVG, PNG, ICO). They are not part of the undo/redo history.\n\nUse cases:\n- AI agents recording *why* a design decision was made: \"Chose this radius because the brief said 'approachable'.\"\n- Human reviewers leaving redline feedback: \"This stroke weight should match the header.\"\n- Cross-session notes that survive save/reload.\n\nReturns the new `annotation_id` UUID.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "The comment or design note (required, non-empty)."
                        },
                        "node_id": {
                            "type": "string",
                            "description": "UUID of the node to annotate. Omit to create a document-level annotation."
                        },
                        "author": {
                            "type": "string",
                            "description": "Optional author identity, e.g. \"claude\" or \"design-reviewer\"."
                        }
                    },
                    "required": ["text"]
                }
            },
            {
                "name": "list_annotations",
                "description": "Return all annotations on the document, optionally filtered by node or resolved status.\n\nBy default only unresolved annotations are returned. Pass `include_resolved: true` to see the full history. Results are sorted by creation time (oldest first).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": {
                            "type": "string",
                            "description": "Filter to annotations attached to this specific node UUID. Omit to list all annotations."
                        },
                        "include_resolved": {
                            "type": "boolean",
                            "description": "When true, include annotations that have already been resolved. Default: false."
                        }
                    }
                }
            },
            {
                "name": "resolve_annotation",
                "description": "Mark an annotation as resolved. The annotation is retained in the file (for audit purposes) but is excluded from future `list_annotations` calls unless `include_resolved: true` is passed.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "annotation_id": {
                            "type": "string",
                            "description": "UUID of the annotation to resolve."
                        }
                    },
                    "required": ["annotation_id"]
                }
            },
            {
                "name": "list_audit_log",
                "description": "Return the most recent MCP tool calls recorded since the server started.\n\nEach entry includes: `id` (sequential), `timestamp` (ISO 8601), `tool_name`, `args` (full arguments), `result_summary` (first 200 chars of result text), `duration_ms`, and `is_error`.\n\nUseful for multi-agent accountability: see exactly what was called, by whom (if the calling agent passes an `author` in its args), and with what parameters.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of entries to return, newest first. Default: 50, maximum: 1000."
                        }
                    }
                }
            },
            {
                "name": "export_audit_log",
                "description": "Export the complete in-memory MCP audit log as a JSON array (oldest first). Includes every tool call recorded since the server started, up to 1000 entries.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "copy_nodes_to_clipboard",
                "description": "Copy one or more nodes (and all their descendants) into the session clipboard ring.\n\nThe clipboard ring holds up to 20 entries. Copying always pushes to index 0 (most recent); older entries shift down. The clipboard is session-scoped — it is not persisted when Photonic closes.\n\nUseful for AI workflows where you want to save a node or group for later reuse within the same session without modifying the document. Combine with `paste_from_history` to place saved nodes anywhere.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "IDs of the nodes to copy. Groups include all descendants automatically."
                        },
                        "label": {
                            "type": "string",
                            "description": "Optional human-readable label for this clipboard entry. Defaults to \"N node(s)\"."
                        }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "get_clipboard_history",
                "description": "Return a summary of all entries currently in the clipboard ring.\n\nEach entry shows its index (0 = most recent), id, label, the number of root nodes copied, and the timestamp. Use the index with `paste_from_history` to paste a specific entry.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "paste_from_history",
                "description": "Paste nodes from a clipboard history entry into the document.\n\nAll pasted nodes receive fresh UUIDs — the original clipboard snapshot is preserved and can be pasted again. The paste is a single undoable step.\n\nAn optional pixel offset shifts the pasted nodes relative to their original positions; useful when pasting multiple times to avoid exact overlap.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "index": {
                            "type": "integer",
                            "description": "Zero-based index into the clipboard ring (0 = most recently copied).",
                            "minimum": 0
                        },
                        "offset_x": {
                            "type": "number",
                            "description": "Horizontal offset in pixels applied to pasted nodes. Default: 0."
                        },
                        "offset_y": {
                            "type": "number",
                            "description": "Vertical offset in pixels applied to pasted nodes. Default: 0."
                        },
                        "layer_id": {
                            "type": "string",
                            "description": "Target layer UUID. Defaults to the document's active layer."
                        }
                    },
                    "required": ["index"]
                }
            },
            {
                "name": "export_design_tokens",
                "description": "Extract the document's design vocabulary — unique solid fill colors, stroke colors, font families, font sizes, and stroke widths — and return them as structured design tokens.\n\nUseful for generating a CSS variable sheet, a Tailwind theme extension, a Style Dictionary token file, or raw JSON for any downstream tooling. Only solid-color fills are tokenised; gradient fills are skipped (they don't map to a single value).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "format": {
                            "type": "string",
                            "enum": ["json", "css", "tailwind", "style-dictionary"],
                            "description": "Output format (default: json). css → :root { --color-1: … }, tailwind → theme.extend block, style-dictionary → W3C Design Token format with $type annotations."
                        }
                    }
                }
            },
            {
                "name": "import_design_tokens",
                "description": "Counterpart to export_design_tokens: register named color swatches from a design-tokens payload so brand colors can be referenced by name (via apply_color_swatch) instead of hard-coded into every fill/gradient. Accepts CSS custom properties (:root { --brand-primary: #2f56cf }), flat or nested JSON, and Style Dictionary ({ \"value\": \"#hex\" }) — auto-detected. Hex values are normalized; existing swatches with the same name are updated. When the brand palette shifts, re-importing re-themes everything referencing those swatches.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "content": {
                            "type": "string",
                            "description": "Inline tokens text (CSS, JSON, or style-dictionary). Provide this OR `path`."
                        },
                        "path": {
                            "type": "string",
                            "description": "Path to a tokens file to read. Provide this OR `content`."
                        },
                        "format": {
                            "type": "string",
                            "enum": ["auto", "css", "json"],
                            "description": "Parse hint (default: auto — CSS if it doesn't start with { or [, else JSON)."
                        },
                        "prefix": {
                            "type": "string",
                            "description": "Optional prefix prepended to every imported swatch name (e.g. 'brand/')."
                        },
                        "clear_existing": {
                            "type": "boolean",
                            "description": "Clear all existing swatches before importing (default: false)."
                        }
                    }
                }
            },
            {
                "name": "auto_name_nodes",
                "description": "Rename nodes with descriptive, human-readable names derived from their content and geometry.\n\n- Text nodes → first 24 chars of content: \"text: hello world\"\n- Group nodes → child count: \"group (3 items)\"\n- Path nodes → fill colour + bounding-box shape: \"blue medium square\", \"red large wide bar\", \"gradient shape\"\n\nBy default only renames nodes with generic auto-generated names (e.g. 'rectangle', 'path', 'group'). Pass overwrite:true to rename all targeted nodes. Use dry_run:true to preview proposed names without applying them.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "scope": {
                            "type": "string",
                            "enum": ["selection", "document"],
                            "description": "Which nodes to rename: 'selection' (active selection only) or 'document' (all nodes). Default: document."
                        },
                        "overwrite": {
                            "type": "boolean",
                            "description": "If true, rename nodes even if they already have non-generic names. Default: false."
                        },
                        "dry_run": {
                            "type": "boolean",
                            "description": "If true, return proposed renames without applying them. Default: false."
                        }
                    }
                }
            },
            {
                "name": "get_css_preview",
                "description": "Return the CSS equivalent of a node's visual properties for developer handoff. Shows background/color, outline (stroke), opacity, mix-blend-mode, transform, and — for text nodes — font-family, font-size, font-weight, and text-align. Width and height are derived from the node's world bounding box. Read-only — does not modify the document.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": "string",
                            "description": "Node UUID or name. If omitted, the first node in document order is used."
                        }
                    }
                }
            },
            {
                "name": "check_style_continuity",
                "description": "Analyse style consistency across the document or a node subset. Flags outliers — nodes whose fill color, stroke width, opacity, or font family deviate from the dominant values used by the rest of the selection. Returns a structured report; makes no changes to the document.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "UUIDs of nodes to analyse. Omit or pass empty array to analyse the entire document."
                        },
                        "checks": {
                            "type": "array",
                            "items": { "type": "string", "enum": ["fill", "stroke", "opacity", "font"] },
                            "description": "Which property groups to check. Defaults to all four when omitted."
                        },
                        "outlier_threshold": {
                            "type": "integer",
                            "default": 2,
                            "description": "Minimum occurrences for a value to be considered 'dominant'. Nodes whose value appears fewer than this many times are flagged."
                        }
                    }
                }
            },
            {
                "name": "list_checkpoints",
                "description": "List all saved document checkpoints, including their IDs, names, and creation times. Use these IDs with diff_checkpoints or restore_checkpoint.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "restore_checkpoint",
                "description": "Restore the document to a saved checkpoint by ID, clearing undo/redo history. This mutates the document.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "checkpoint_id": {
                            "type": "string",
                            "description": "UUID of the checkpoint to restore"
                        }
                    },
                    "required": ["checkpoint_id"]
                }
            },
            {
                "name": "diff_checkpoints",
                "description": "Compare two checkpoint snapshots and return a structured JSON diff of added, removed, and modified nodes and layers. Use list_checkpoints first to get checkpoint IDs.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "from_id": {
                            "type": "string",
                            "description": "UUID of the baseline (older) checkpoint"
                        },
                        "to_id": {
                            "type": "string",
                            "description": "UUID of the target (newer) checkpoint"
                        }
                    },
                    "required": ["from_id", "to_id"]
                }
            },
            {
                "name": "simplify_path",
                "description": "Reduce the anchor-point count of a path using Ramer-Douglas-Peucker simplification. Bézier curves are first sampled to line segments, then redundant points are removed. Supports dry_run to preview the reduction without applying. The result is a polygonal path with fewer vertices.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": {
                            "type": "string",
                            "description": "UUID of the path node to simplify"
                        },
                        "tolerance": {
                            "type": "number",
                            "description": "RDP tolerance in document coordinates. Larger values remove more points. Typical: 0.5–5.0 for screen work, 0.1–1.0 for precise technical illustration."
                        },
                        "dry_run": {
                            "type": "boolean",
                            "description": "If true, return before/after point counts without modifying the document. Default false."
                        }
                    },
                    "required": ["node_id", "tolerance"]
                }
            },
            {
                "name": "smooth_path",
                "description": "Smooth jagged or polygonal paths using Chaikin's corner-cutting algorithm. Converts sharp LineTo segments into smooth cubic Bézier curves. Applies to the specified node IDs or the current selection. factor (0–0.5) controls rounding strength; 0.25 is the classic value. iterations (1–8) controls how many passes are applied.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "UUIDs of path nodes to smooth. If empty, uses the current selection."
                        },
                        "factor": {
                            "type": "number",
                            "description": "Smoothing strength [0, 0.5]. 0.25 is the classic Chaikin value; higher values produce rounder curves. Default 0.25."
                        },
                        "iterations": {
                            "type": "integer",
                            "description": "Number of smoothing passes (1–8). More passes = smoother result. Default 2."
                        }
                    }
                }
            },
            {
                "name": "snap_to_pixel",
                "description": "Round the position (translation) of one or more nodes to the nearest integer coordinates. Useful for pixel-perfect screen design. Bundled as a single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 1,
                            "description": "IDs of nodes to snap to integer pixel coordinates"
                        }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "distribute_no_overlap",
                "description": "Push nodes apart until none of their bounding boxes overlap, using iterative pairwise repulsion. Nodes are nudged along the axis with the smallest overlap at each step.\n\nUseful for:\n- Spreading a pile of overlapping objects\n- Auto-spacing labels, icons, or stickers\n- Resolving collisions after a bulk paste or array creation\n\nNode positions are updated in a single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "IDs of nodes to distribute. Uses current selection if empty."
                        },
                        "padding":         { "type": "number", "description": "Minimum gap between bounding boxes in px (default: 4)" },
                        "max_iterations":  { "type": "number", "description": "Maximum resolution iterations (default: 100, max: 500)" }
                    },
                    "required": []
                }
            },
            {
                "name": "noise_deform",
                "description": "Apply smooth sinusoidal displacement to all anchor and control points in the selected path nodes, producing organic wave-like deformation. Uses two-octave sinusoidal noise — unlike roughen_path (random per-point jitter), noise_deform produces flowing, rhythmic distortion.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "UUIDs or names of path nodes to deform."
                        },
                        "amplitude": { "type": "number", "description": "Maximum displacement in document units (default: 8.0)." },
                        "frequency": { "type": "number", "description": "Spatial frequency in cycles/px — higher = tighter waves (default: 0.05)." },
                        "seed":      { "type": "number", "description": "Phase offset seed to shift the wave pattern (default: 0.0)." },
                        "axis":      { "type": "string", "enum": ["both", "x", "y"], "description": "Which axis to deform (default: \"both\")." }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "mirror_copy",
                "description": "Duplicate each selected node and flip the copy across its bounding-box center, producing a mirrored twin. The original is unchanged; the new copy is added to the same layer and can be repositioned freely. Uses current selection when node_ids is empty.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "UUIDs or names of nodes to mirror. Uses current selection if empty."
                        },
                        "axis": { "type": "string", "enum": ["horizontal", "vertical"], "description": "\"horizontal\" flips left-right (default); \"vertical\" flips top-bottom." }
                    },
                    "required": []
                }
            },
            {
                "name": "rotate_copies",
                "description": "Create N evenly-spaced rotational copies of a node around a center point, producing a radial symmetry arrangement. The original node is counted in the total — count=6 means the original plus 5 copies at 60° increments. Optionally wraps all copies in a Group. Useful for mandalas, snowflakes, icons, and any N-fold symmetric composition.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "UUID or name of the source node." },
                        "count":   { "type": "integer", "minimum": 2, "description": "Total number of copies including the original (e.g. 6 = original + 5 copies at 60° steps)." },
                        "cx":      { "type": "number", "description": "X of rotation center in document units. Defaults to the node's bounding-box center." },
                        "cy":      { "type": "number", "description": "Y of rotation center in document units. Defaults to the node's bounding-box center." },
                        "group":   { "type": "boolean", "description": "When true, wrap all copies (including the original) in a new Group node. Default: false." }
                    },
                    "required": ["node_id", "count"]
                }
            },
            {
                "name": "copy_appearance",
                "description": "Copy fill, stroke, and/or opacity from one source node to one or more target nodes (eyedropper-style). Each attribute can be toggled independently. Targets that are not path nodes will still have their opacity updated if copy_opacity is true. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "source_id":    { "type": "string", "description": "UUID or name of the node to copy appearance from." },
                        "target_ids":   { "type": "array", "items": { "type": "string" }, "description": "UUIDs or names of nodes to apply the appearance to." },
                        "copy_fill":    { "type": "boolean", "description": "Copy fill. Default: true." },
                        "copy_stroke":  { "type": "boolean", "description": "Copy stroke. Default: true." },
                        "copy_opacity": { "type": "boolean", "description": "Copy opacity. Default: true." }
                    },
                    "required": ["source_id", "target_ids"]
                }
            },
            {
                "name": "set_node_prompt",
                "description": "Record an AI prompt on a node's prompt history for creative provenance tracking. Each entry is chronological — the full history shows which prompts shaped the node's appearance. This enables 'intent-preserving edit' workflows where an agent understands why a node looks the way it does.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "UUID or name of the node to annotate." },
                        "prompt":  { "type": "string", "description": "The prompt text to record." },
                        "mode":    { "type": "string", "enum": ["append", "prepend", "replace"], "description": "\"append\" (default) adds to end; \"prepend\" adds to start; \"replace\" clears history first." }
                    },
                    "required": ["node_id", "prompt"]
                }
            },
            {
                "name": "get_node_prompts",
                "description": "Return the full prompt history for a node — the chronological list of AI prompts that created or modified it. Returns empty history message if none recorded.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "UUID or name of the node." }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "reverse_node_order",
                "description": "Reverse the front-to-back stacking order of children within each selected group node. The topmost child becomes the bottommost and vice versa. Useful for flipping blend results or layered artwork. Single undoable step. Uses current selection if node_ids is empty.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "UUIDs or names of group nodes. Uses current selection if empty."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "pin_object_guides",
                "description": "Create persistent ruler guides at the edges and/or center of selected nodes. Guides remain visible across editing sessions and serve as precision alignment references. Deduplicates — existing guides within 0.5 px are not duplicated.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "UUIDs or names of nodes. Uses current selection if empty."
                        },
                        "edges": {
                            "type": "string",
                            "description": "Which edges to pin: \"all\" (default), \"center\" (center_h + center_v), \"edges\" (top+bottom+left+right), or comma-separated from: top, bottom, left, right, center_h, center_v."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "distribute_on_path",
                "description": "Place evenly-spaced copies of one or more nodes along a guide path. Each source node is cloned at arc-length-equidistant positions along the first subpath of the path node. Optionally rotates each copy to align with the path's tangent direction.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path_node_id": { "type": "string", "description": "ID of the path node to use as the distribution guide" },
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 1,
                            "description": "IDs of source nodes to clone and distribute. Nodes are cycled if count > node_ids.length."
                        },
                        "count": { "type": "integer", "minimum": 1, "description": "Number of copies to place. Defaults to the number of source nodes." },
                        "align_to_path": { "type": "boolean", "description": "Rotate each copy to face along the path's tangent direction. Default: false." },
                        "layer_id": { "type": "string", "description": "Target layer for the copies. Defaults to the guide path's layer." }
                    },
                    "required": ["path_node_id", "node_ids"]
                }
            },
            {
                "name": "recolor_artwork",
                "description": "Map every unique solid fill in the selected nodes to the nearest color in a target palette (Euclidean RGB distance). Useful for applying brand palettes or reducing color count. Gradient fills are skipped. Single undoable batch step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "IDs of nodes to recolor. If empty, all path nodes in the document are processed."
                        },
                        "palette": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 1,
                            "description": "Target palette as hex strings, e.g. [\"#FF0000\",\"#00FF00\",\"#0000FF\"]. Each node's fill is replaced with the closest palette color."
                        }
                    },
                    "required": ["palette"]
                }
            },
            {
                "name": "adjust_colors",
                "description": "Shift RGB(A) channel values across selected path nodes. Each delta is added to the corresponding channel and clamped to [0, 1]. Works on solid fills, gradient stops, fluid/mesh gradient points, and stroke colors. If node_ids is omitted, all path nodes in the document are adjusted. Single undo step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "UUIDs of path nodes to adjust. Omit to adjust all path nodes in the document."
                        },
                        "delta_r": { "type": "number", "description": "Red channel delta (−1.0 to 1.0). Default 0." },
                        "delta_g": { "type": "number", "description": "Green channel delta (−1.0 to 1.0). Default 0." },
                        "delta_b": { "type": "number", "description": "Blue channel delta (−1.0 to 1.0). Default 0." },
                        "delta_a": { "type": "number", "description": "Alpha channel delta (−1.0 to 1.0). Default 0." }
                    },
                    "required": []
                }
            },
            {
                "name": "invert_colors",
                "description": "Invert all color values (fill and stroke) on selected path nodes. Each RGB channel becomes (1 − value); alpha is preserved. Works on solid fills, linear/radial gradient stops, fluid gradient points, and mesh gradient vertices. If node_ids is omitted, all path nodes in the document are inverted. Single undo step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "UUIDs of path nodes to invert. Omit to invert all path nodes in the document."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "convert_to_grayscale",
                "description": "Convert all color values (fill and stroke) on selected path nodes to grayscale using the ITU-R BT.601 luminance formula (0.299R + 0.587G + 0.114B). Works on solid fills, linear/radial gradient stops, fluid gradient points, and mesh gradient vertices. Alpha is preserved. If node_ids is omitted, all path nodes in the document are converted. Single undo step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "UUIDs of path nodes to convert. Omit to convert all path nodes in the document."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "outline_stroke",
                "description": "Convert the stroke on each selected path node into a new filled closed path that traces the stroke outline (center-aligned). The new node inherits the stroke color and opacity as its solid fill; its stroke is disabled. The original node's stroke is disabled. Useful for turning hairline strokes into editable geometry for boolean operations, export, or further styling. Dash patterns are ignored — the solid stroke shape is always outlined. Single undoable step per call.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "UUIDs of path nodes to outline. Each must be a path node with an enabled stroke."
                        },
                        "keep_original": {
                            "type": "boolean",
                            "description": "Unused — reserved for future use. The original node is always retained with its stroke disabled. Default: false."
                        }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "offset_path",
                "description": "Create a parallel copy of one or more paths inset or outset by a fixed distance. Positive distance expands the path outward (outset); negative distance contracts it inward (inset). By default a new offset node is added above the original (create_copy: true); set create_copy to false to replace the original in place. Corner style is configurable. Non-path nodes are silently skipped. Single undoable step per call.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "UUIDs of path nodes to offset"
                        },
                        "distance": {
                            "type": "number",
                            "description": "Offset distance in document units. Positive = outset (expand outward), negative = inset (contract inward)."
                        },
                        "join_style": {
                            "type": "string",
                            "enum": ["miter", "round", "bevel"],
                            "description": "Corner join style for the offset path. Default: miter."
                        },
                        "create_copy": {
                            "type": "boolean",
                            "description": "If true (default), add the offset result as a new node above the original. If false, replace the original node with the offset result."
                        }
                    },
                    "required": ["node_ids", "distance"]
                }
            },
            {
                "name": "split_into_grid",
                "description": "Divide a path node's bounding box into a rows×cols grid of separate rectangle nodes, each inheriting the source node's fill, stroke, opacity, and blend mode. Optional horizontal (gutter_x) and vertical (gutter_y) gutters are subtracted from the total area before dividing. The source node is deleted by default; set keep_original to true to retain it. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": {
                            "type": "string",
                            "description": "UUID of the source path node whose bounding box defines the grid area."
                        },
                        "rows": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Number of rows in the grid (≥ 1)."
                        },
                        "cols": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Number of columns in the grid (≥ 1)."
                        },
                        "gutter_x": {
                            "type": "number",
                            "description": "Horizontal gutter width in document units between columns. Default: 0."
                        },
                        "gutter_y": {
                            "type": "number",
                            "description": "Vertical gutter height in document units between rows. Default: 0."
                        },
                        "keep_original": {
                            "type": "boolean",
                            "description": "When true, keep the source node after splitting. Default: false (source is deleted)."
                        },
                        "layer_id": {
                            "type": "string",
                            "description": "UUID of the layer to place new nodes in. Defaults to the source node's layer."
                        }
                    },
                    "required": ["node_id", "rows", "cols"]
                }
            },
            {
                "name": "make_compound_path",
                "description": "Combine two or more path nodes into a single compound path. Overlapping subpaths create holes via the even-odd fill rule (like Illustrator's Object > Compound Path > Make). The bottommost selected node's fill, stroke, and position are kept; all other source nodes are removed. All transforms are baked before merging. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 2,
                            "description": "IDs of the path nodes to combine. Must be at least 2 top-level path nodes."
                        },
                        "name": { "type": "string", "description": "Optional name for the resulting compound path node." }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "make_live_boolean",
                "description": "Group two or more path nodes into a NON-DESTRUCTIVE live boolean. Unlike boolean_operation / make_compound_path, the operands stay individually editable: the group renders and exports as the single resolved path (styled by the bottom child), and editing any operand re-computes the boolean live. Operators: union, intersect, subtract, exclude, divide. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 2,
                            "description": "IDs of the path nodes to combine (at least 2 top-level path nodes)."
                        },
                        "operation": {
                            "type": "string",
                            "enum": ["union", "intersect", "subtract", "exclude", "divide"],
                            "description": "The boolean operator to apply across the operands."
                        },
                        "name": { "type": "string", "description": "Optional name for the live-boolean group." }
                    },
                    "required": ["node_ids", "operation"]
                }
            },
            {
                "name": "release_compound_path",
                "description": "Release a compound path back into its individual subpaths (Illustrator's Object > Compound Path > Release). Each subpath becomes a separate path node sharing the compound path's fill and stroke. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "ID of the compound path node to release." }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "blend_colors",
                "description": "Distribute fill colors linearly across 2 or more path nodes. The first and last nodes keep their existing solid fill colors; all intermediate nodes receive interpolated colors at evenly spaced positions between them. Optionally sort the nodes along an axis before blending: 'horizontal' (left→right by bounding-box center X), 'vertical' (top→bottom by center Y), or 'depth' (bottom→top by z-order). Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 2,
                            "description": "Ordered UUIDs of path nodes to blend. At minimum 2 required; 3+ produces visible interpolation on intermediate nodes."
                        },
                        "direction": {
                            "type": "string",
                            "enum": ["horizontal", "vertical", "depth"],
                            "description": "Optional sort axis. 'horizontal' sorts by bounding-box center X, 'vertical' by center Y, 'depth' by z-order. Omit to use the supplied order as-is."
                        }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "color_guide",
                "description": "Generate a color harmony palette from a base color using classic harmony rules. Supply a hex color directly or omit base_color to use the solid fill of the first selected node. Returns an array of colors including the base. Read-only — does not modify the document.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "base_color": {
                            "type": "string",
                            "description": "Hex color string (#RRGGBB or #RRGGBBAA). If omitted, uses the solid fill of the first selected node."
                        },
                        "rule": {
                            "type": "string",
                            "enum": ["complementary", "analogous", "triadic", "split_complementary", "tetradic", "monochromatic"],
                            "description": "Color harmony rule. Default: 'complementary'."
                        }
                    }
                }
            },
            {
                "name": "scissors_cut",
                "description": "Cut a path node at the point on it nearest to the specified canvas coordinates, splitting it into two open path nodes. The original node is removed; both halves inherit the original's fill, stroke, transform, opacity, and blend mode. Useful for splitting paths at intersections or at specific positions along an edge.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": {
                            "type": "string",
                            "description": "UUID of the path node to cut."
                        },
                        "canvas_x": {
                            "type": "number",
                            "description": "X coordinate in document (canvas) space of the desired cut point."
                        },
                        "canvas_y": {
                            "type": "number",
                            "description": "Y coordinate in document (canvas) space of the desired cut point."
                        }
                    },
                    "required": ["node_id", "canvas_x", "canvas_y"]
                }
            },
            {
                "name": "add_construction_line",
                "description": "Add an angled construction line — an infinite non-printing reference line through a specified point at any angle. Unlike ruler guides (horizontal/vertical only), construction lines can be at any angle. Stored in the document's guide list and stripped from all exports. Visible when guides are shown (Ctrl+;).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "number", "description": "X coordinate (document units) of the line's origin point." },
                        "y": { "type": "number", "description": "Y coordinate (document units) of the line's origin point." },
                        "angle_degrees": { "type": "number", "description": "Angle in degrees. 0° = horizontal, 90° = vertical, 45° = diagonal." },
                        "color": { "type": "string", "description": "Optional hex color (e.g. '#FF8800'). Default: orange." }
                    },
                    "required": ["x", "y", "angle_degrees"]
                }
            },
            {
                "name": "add_guide",
                "description": "Add a ruler guide (horizontal or vertical reference line) at a precise document-unit position. Guides are visible in the editor and stripped from all export formats.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "orientation": {
                            "type": "string",
                            "enum": ["horizontal", "vertical"],
                            "description": "Guide orientation. 'horizontal' creates a fixed-Y line; 'vertical' creates a fixed-X line."
                        },
                        "position": {
                            "type": "number",
                            "description": "Position in document units. Y coordinate for horizontal guides; X coordinate for vertical guides."
                        },
                        "color": {
                            "type": "array",
                            "items": { "type": "number" },
                            "minItems": 4,
                            "maxItems": 4,
                            "description": "Optional RGBA color override as [R, G, B, A] in [0, 1] range. Omit to use the default cyan."
                        }
                    },
                    "required": ["orientation", "position"]
                }
            },
            {
                "name": "remove_guide",
                "description": "Remove a specific ruler guide by its UUID. Returns an error if the guide is locked.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "guide_id": {
                            "type": "string",
                            "description": "UUID of the guide to remove. Obtain from list_guides."
                        }
                    },
                    "required": ["guide_id"]
                }
            },
            {
                "name": "list_guides",
                "description": "List all ruler guides in the document with their orientation, position, lock state, and optional color. Read-only.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "clear_guides",
                "description": "Remove all unlocked ruler guides from the document. Locked guides are preserved.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "magic_wand_select",
                "description": "Click at a canvas coordinate to select the topmost node at that point, then expand the selection to all nodes sharing the specified attribute (fill color, stroke color, stroke weight, opacity, blend mode, or object type). Equivalent to the Magic Wand tool in vector editors.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "canvas_x": {
                            "type": "number",
                            "description": "X coordinate in document (canvas) space to click."
                        },
                        "canvas_y": {
                            "type": "number",
                            "description": "Y coordinate in document (canvas) space to click."
                        },
                        "attribute": {
                            "type": "string",
                            "enum": ["fill_color", "stroke_color", "stroke_weight", "opacity", "blend_mode", "object_type"],
                            "description": "Which attribute to match. Defaults to fill_color."
                        },
                        "tolerance": {
                            "type": "number",
                            "description": "How close two values must be to count as matching. For colors: Euclidean RGBA distance in [0,1] space. For stroke weight / opacity: absolute difference. Ignored for blend_mode and object_type. Defaults to 0.01."
                        }
                    },
                    "required": ["canvas_x", "canvas_y"]
                }
            },
            {
                "name": "convert_anchor_points",
                "description": "Convert all cubic bezier anchor points in the selected path nodes to smooth joins (handles made collinear through each interior anchor) or corner joins (handles retracted to the anchor, producing straight-line segments). Non-path nodes are skipped. Single undoable step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "IDs of path nodes to convert."
                        },
                        "mode": {
                            "type": "string",
                            "enum": ["smooth", "corner"],
                            "description": "smooth: makes junction handles collinear (smooth bezier curve). corner: retracts cubic handles to their anchors (straight lines / cusps). Default: smooth."
                        }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "lasso_select",
                "description": "Select all visible nodes whose bounding-box centroid (or any corner, in non-centroid mode) lies inside a closed polygon defined by canvas-space coordinates. Equivalent to the Lasso Selection tool. Useful for selecting nodes within an irregular region without needing their IDs.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "points": {
                            "type": "array",
                            "items": {
                                "type": "array",
                                "items": { "type": "number" },
                                "minItems": 2,
                                "maxItems": 2
                            },
                            "minItems": 3,
                            "description": "Polygon boundary in canvas (document) coordinates. Each element is [x, y]. Minimum 3 points. The polygon is automatically closed."
                        },
                        "centroid_mode": {
                            "type": "boolean",
                            "description": "When true (default), select nodes whose bounding-box centroid is inside the polygon. When false, select nodes with any AABB corner inside the polygon."
                        },
                        "additive": {
                            "type": "boolean",
                            "description": "When true, add to the existing selection rather than replacing it. Default false."
                        }
                    },
                    "required": ["points"]
                }
            },
            {
                "name": "get_recent_colors",
                "description": "Return the list of recently used fill and stroke colors for this document, ordered most-recently-used first (up to 20 entries). Useful for quickly re-applying a palette or building color suggestions.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "select_by_kind",
                "description": "Select all nodes of a specified type. kind can be: 'path' (all path/shape nodes), 'text' (all text nodes), 'group' (all group nodes), or 'same_layer' (all nodes on the active layer). Optionally additive to extend the current selection.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "kind": {
                            "type": "string",
                            "enum": ["path", "text", "group", "same_layer"],
                            "description": "Which object type to select. Defaults to 'path'."
                        },
                        "additive": {
                            "type": "boolean",
                            "description": "When true, add to the current selection instead of replacing it. Default false."
                        }
                    }
                }
            },
            {
                "name": "create_freehand_path",
                "description": "Create a freehand polyline path from an ordered list of canvas-space [x, y] points. Equivalent to using the Pencil tool by dragging. The path is open (no auto-close). Optionally specify fill and stroke styles.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "points": {
                            "type": "array",
                            "description": "Ordered canvas-space points [x, y]. Minimum 2 required.",
                            "items": {
                                "type": "array",
                                "items": { "type": "number" },
                                "minItems": 2,
                                "maxItems": 2
                            },
                            "minItems": 2
                        },
                        "fill":   { "type": "object", "description": "Optional fill." },
                        "stroke": { "type": "object", "description": "Optional stroke." },
                        "name":   { "type": "string",  "description": "Node name (default: 'Pencil')." }
                    },
                    "required": ["points"]
                }
            },
            {
                "name": "enter_isolation_mode",
                "description": "Enter Isolation Mode for a group: select all direct children of the group, restricting further edits to those children. Equivalent to double-clicking a group in Illustrator. In the GUI, only children of the group are clickable until Escape is pressed.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "group_id": {
                            "type": "string",
                            "format": "uuid",
                            "description": "The UUID of the group node to isolate."
                        }
                    },
                    "required": ["group_id"]
                }
            },
            {
                "name": "exit_isolation_mode",
                "description": "Exit Isolation Mode: clear the current selection and return to normal editing. Equivalent to pressing Escape in Illustrator's isolation mode.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "select_inside_group",
                "description": "Replace the current selection with the direct children of a group node. Equivalent to Alt+clicking into a group in Illustrator's Group Selection tool. Use to select individual objects inside a group without ungrouping it.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "group_id": {
                            "type": "string",
                            "format": "uuid",
                            "description": "The UUID of the group node whose children should be selected."
                        },
                        "additive": {
                            "type": "boolean",
                            "description": "When true, add the group's children to the existing selection instead of replacing it. Default false."
                        }
                    },
                    "required": ["group_id"]
                }
            },
            {
                "name": "create_paragraph_style",
                "description": "Save a named paragraph style (alignment, line height, letter spacing, font) to the document. Capture from a source text node or specify attributes directly.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Unique style name." },
                        "source_node_id": { "type": "string", "description": "Capture layout from this text node (UUID or name)." },
                        "align": { "type": "string", "enum": ["left","center","right"], "description": "Text alignment." },
                        "line_height": { "type": "number", "description": "Line height multiplier e.g. 1.5." },
                        "letter_spacing": { "type": "number" },
                        "font_size": { "type": "number" },
                        "font_family": { "type": "string" }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "list_paragraph_styles",
                "description": "List all named paragraph styles saved in the document.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "apply_paragraph_style",
                "description": "Apply a named paragraph style to one or more text nodes. Only defined attributes are changed.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "style_name": { "type": "string" },
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Text node UUIDs or names. Uses selection if empty." }
                    },
                    "required": ["style_name"]
                }
            },
            {
                "name": "delete_paragraph_style",
                "description": "Delete a named paragraph style from the document.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "name": { "type": "string" } },
                    "required": ["name"]
                }
            },
            {
                "name": "create_character_style",
                "description": "Save a named character style to the document. Capture from a source text node or specify attributes explicitly. Styles can be applied to any text node with apply_character_style.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Unique style name." },
                        "source_node_id": { "type": "string", "description": "Capture font/color from this text node (UUID or name). Explicit args override captured values." },
                        "font_family": { "type": "string" },
                        "font_size": { "type": "number" },
                        "font_weight": { "type": "integer", "description": "100–900. 400=regular, 700=bold." },
                        "fill_hex": { "type": "string", "description": "Fill color as CSS hex e.g. #FF5733." },
                        "letter_spacing": { "type": "number" },
                        "line_height": { "type": "number", "description": "Multiplier e.g. 1.5 = 150%." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "list_character_styles",
                "description": "List all named character styles saved in the document.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "apply_character_style",
                "description": "Apply a named character style to one or more text nodes. Only attributes defined in the style are changed; unset attributes are left as-is.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "style_name": { "type": "string", "description": "Name of the style to apply." },
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Text node UUIDs or names. Uses current selection if empty."
                        }
                    },
                    "required": ["style_name"]
                }
            },
            {
                "name": "delete_character_style",
                "description": "Delete a named character style from the document.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the style to delete." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "tag_node_for_export",
                "description": "Tag a node for inclusion in batch asset exports (Asset Export Panel equivalent). Set name to an empty string to remove the tag. Supports per-scale raster exports.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "UUID or name of the node to tag." },
                        "name": { "type": "string", "description": "Base asset name (without extension). Pass empty string to remove the tag." },
                        "format": { "type": "string", "enum": ["svg","png","jpeg","jpg","webp"], "description": "Export format. Default: svg." },
                        "scales": {
                            "type": "array",
                            "items": { "type": "number" },
                            "description": "Scale multipliers for raster exports (e.g. [1,2,3] → @1x @2x @3x). Ignored for SVG. Default: [1]."
                        }
                    },
                    "required": ["node_id", "name"]
                }
            },
            {
                "name": "export_tagged_assets",
                "description": "Export all nodes tagged via tag_node_for_export. SVG assets are returned inline; raster assets return metadata (name, node_id, scale) for use with export_raster.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "filter": { "type": "string", "description": "Only export assets whose name contains this string." }
                    },
                    "required": []
                }
            },
            {
                "name": "select_similar",
                "description": "Select all nodes in the document whose visual attributes match those of the reference node(s). Implements Illustrator-style 'Select > Same > …' and Global Edit. match_by accepts a comma-separated list of: fill_color, stroke_color, stroke_width, kind, opacity, tags. Default match_by: fill_color.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Reference node UUIDs or names. Uses current selection if empty."
                        },
                        "match_by": {
                            "type": "string",
                            "description": "Comma-separated match criteria: fill_color, stroke_color, stroke_width, kind, opacity, tags. Default: fill_color."
                        },
                        "tolerance": {
                            "type": "integer",
                            "description": "Color match tolerance 0–255 per channel. Default: 5."
                        },
                        "additive": {
                            "type": "boolean",
                            "description": "When true, add matches to the existing selection instead of replacing it. Default: false."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "get_document_template",
                "description": "Capture the current document as a reusable template — preserving canvas size, layer structure, guides, and export profiles while stripping all node content. Use the returned template_json with apply_document_template to stamp these settings onto a different document.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            },
            {
                "name": "apply_document_template",
                "description": "Apply a previously captured document template to the current document. Canvas size, guides, and export profiles from the template are merged in non-destructively. New layers from the template are added only if no layer with the same name already exists. Existing nodes are never removed.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "template_json": {
                            "type": "string",
                            "description": "Template JSON string as returned by get_document_template."
                        }
                    },
                    "required": ["template_json"]
                }
            },
            {
                "name": "add_color_swatch",
                "description": "Add a named color swatch to the document palette. Swatches can be applied to any node's fill with apply_color_swatch.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Unique swatch name." },
                        "color_hex": { "type": "string", "description": "CSS hex color e.g. #FF5733." }
                    },
                    "required": ["name", "color_hex"]
                }
            },
            {
                "name": "list_color_swatches",
                "description": "List all named color swatches saved in the document palette.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            },
            {
                "name": "apply_color_swatch",
                "description": "Apply a named color swatch to the fill of one or more nodes.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Node IDs (UUID or name) to apply the swatch to."
                        },
                        "swatch_name": { "type": "string", "description": "Name of the swatch to apply." }
                    },
                    "required": ["node_ids", "swatch_name"]
                }
            },
            {
                "name": "update_color_swatch",
                "description": "Rename or change the color of an existing swatch. Optionally propagates the color change to all nodes currently using the old swatch color.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the swatch to update." },
                        "new_name": { "type": "string", "description": "New name (optional, omit to keep current name)." },
                        "new_color_hex": { "type": "string", "description": "New color as CSS hex (optional, omit to keep current color)." },
                        "propagate": { "type": "boolean", "description": "When true (default), update all nodes whose fill matches the old color. Set false to update only the swatch record." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "delete_color_swatch",
                "description": "Remove a named color swatch from the document palette. Does not alter existing node fill colors.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the swatch to delete." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "load_swatch_library",
                "description": "Load a predefined color swatch library into the document. Available libraries: web (16 named HTML colors), material (16 Material Design 500 tones), pastels (12 soft pastel shades), earth_tones (12 warm earthy tones), neon (12 bright neon colors), grayscale (11-step neutral ramp). Skips swatches already present by name. Set clear_existing=true to replace all existing swatches first.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "library": { "type": "string", "enum": ["web", "material", "pastels", "earth_tones", "neon", "grayscale"], "description": "Library name to load." },
                        "clear_existing": { "type": "boolean", "description": "Remove all existing swatches before loading. Default false (append)." }
                    },
                    "required": ["library"]
                }
            },
            {
                "name": "define_graphic_style",
                "description": "Define (or overwrite) a named graphic style — a reusable appearance preset storing fill, stroke, and opacity. Capture style from an existing node by passing node_id, or define it explicitly with fill_hex, stroke_hex, stroke_width, and opacity. Apply later with apply_graphic_style.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Unique style name." },
                        "node_id": { "type": "string", "description": "Capture fill, stroke, and opacity from this node (UUID or name). Omit to use explicit parameters." },
                        "fill_hex": { "type": "string", "description": "Fill color as hex (e.g. '#ff0000'). Used when node_id is not provided." },
                        "stroke_hex": { "type": "string", "description": "Stroke color as hex. Used when node_id is not provided." },
                        "stroke_width": { "type": "number", "description": "Stroke width in px. Used when node_id is not provided." },
                        "opacity": { "type": "number", "description": "Node opacity 0.0–1.0. Default 1.0." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "list_graphic_styles",
                "description": "List all named graphic styles saved in the document.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "apply_graphic_style",
                "description": "Apply a named graphic style (fill, stroke, opacity) to one or more nodes. Undo-safe batch command.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Node UUIDs or names to apply the style to." },
                        "name": { "type": "string", "description": "Name of the graphic style to apply." }
                    },
                    "required": ["node_ids", "name"]
                }
            },
            {
                "name": "delete_graphic_style",
                "description": "Delete a named graphic style from the document. Existing nodes are not affected.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the graphic style to delete." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "define_width_profile",
                "description": "Define (or overwrite) a named variable-width stroke profile. Widths are sampled at even t intervals along the path (t=0 start, t=1 end). When applied, the average width is used for uniform stroke rendering — the profile is stored for future variable-width rendering support.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Unique profile name." },
                        "widths": {
                            "type": "array",
                            "items": { "type": "number", "minimum": 0 },
                            "minItems": 2,
                            "description": "Width values (≥2) in document units, from path start to end. E.g. [1, 4, 1] = thin ends, thick middle."
                        }
                    },
                    "required": ["name", "widths"]
                }
            },
            {
                "name": "list_width_profiles",
                "description": "List all named variable-width stroke profiles in the document.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "apply_width_profile",
                "description": "Apply a named width profile to path nodes — sets stroke.width to the profile average. Undo-safe batch command.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Node UUIDs or names to apply the profile to." },
                        "name": { "type": "string", "description": "Name of the width profile to apply." }
                    },
                    "required": ["node_ids", "name"]
                }
            },
            {
                "name": "delete_width_profile",
                "description": "Delete a named width profile. Does not affect existing node stroke widths.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the profile to delete." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "define_pattern",
                "description": "Define (or overwrite) a named tiled raster pattern in the document pattern registry. Supply the tile via `path` (a file on disk) or `data_base64` (inline image bytes). Patterns are applied to nodes with apply_pattern_fill; the applied fill embeds a clone of the tile so it renders self-contained on canvas, headless, and SVG.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Unique pattern name." },
                        "path": { "type": "string", "description": "Path to a tile image file (PNG/JPEG/WebP/…)." },
                        "data_base64": { "type": "string", "description": "Base64-encoded tile image bytes (alternative to `path`)." },
                        "tile_type": { "type": "string", "enum": ["grid", "brick_by_row", "brick_by_column", "hex"], "description": "Tile layout (default 'grid')." },
                        "scale": { "type": "number", "description": "Uniform pattern scale (default 1.0)." },
                        "rotation_degrees": { "type": "number", "description": "Pattern rotation in degrees (default 0)." },
                        "offset": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "description": "Document-space [x, y] offset of the pattern origin (default [0, 0])." },
                        "spacing": { "type": "number", "description": "Inter-tile gutter in tile pixels; samples as transparent (default 0)." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "list_patterns",
                "description": "List all named patterns in the document pattern registry, with their tile dimensions and transform.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "apply_pattern_fill",
                "description": "Apply a registry pattern as the fill of one or more path nodes (undo-safe batch). A clone of the pattern's tile is embedded on each node, with optional per-application transform overrides. The pattern transform is independent of the node transform — moving the shape scrolls the artwork under the pattern.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Node UUIDs or names to fill with the pattern." },
                        "pattern": { "type": "string", "description": "Name or UUID of the pattern to apply." },
                        "tile_type": { "type": "string", "enum": ["grid", "brick_by_row", "brick_by_column", "hex"], "description": "Override the tile layout for this application." },
                        "scale": { "type": "number", "description": "Override the pattern scale for this application." },
                        "rotation_degrees": { "type": "number", "description": "Override the pattern rotation (degrees) for this application." },
                        "offset": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "description": "Override the pattern [x, y] offset for this application." },
                        "spacing": { "type": "number", "description": "Override the inter-tile spacing for this application." }
                    },
                    "required": ["node_ids", "pattern"]
                }
            },
            {
                "name": "delete_pattern",
                "description": "Delete a named pattern from the registry. Does not affect nodes already filled with it.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the pattern to delete." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "set_constraint",
                "description": "Create a live property constraint binding a node property to an arithmetic expression over other nodes' properties (e.g. 'nodes[\\'logo\\'].x + 20'). Re-evaluated after every edit. Target property must be one of x, y, opacity, font_size. Cycles are rejected.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Target node UUID or name." },
                        "property": { "type": "string", "enum": ["x", "y", "opacity", "font_size"], "description": "Target property to drive." },
                        "expression": { "type": "string", "description": "Arithmetic expression; may reference nodes['<id-or-name>'].<prop>." }
                    },
                    "required": ["node_id", "property", "expression"]
                }
            },
            {
                "name": "list_constraints",
                "description": "List all live property constraints with their current evaluated values.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "remove_constraint",
                "description": "Remove a property constraint by its id.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "constraint_id": { "type": "string", "description": "UUID of the constraint to remove." }
                    },
                    "required": ["constraint_id"]
                }
            },
            {
                "name": "define_symbol",
                "description": "Designate a node as a named symbol master. Any node can be a symbol: paths, groups, text. Instances placed with place_symbol are independent copies that carry a symbol_ref tag identifying their origin symbol.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Node ID (UUID or name) to designate as the symbol master." },
                        "name": { "type": "string", "description": "Unique symbol name." }
                    },
                    "required": ["node_id", "name"]
                }
            },
            {
                "name": "list_symbols",
                "description": "List all named symbols defined in the document, including master node names and IDs.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "place_symbol",
                "description": "Place an instance of a named symbol at the given position. The instance is a clone of the master node with a symbol_ref tag. Edit the master to see design intent; use break_link_to_symbol to detach an instance for independent editing.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "symbol_name": { "type": "string", "description": "Symbol name to instantiate." },
                        "x": { "type": "number", "description": "X position (document units). Default: 0." },
                        "y": { "type": "number", "description": "Y position (document units). Default: 0." }
                    },
                    "required": ["symbol_name"]
                }
            },
            {
                "name": "break_link_to_symbol",
                "description": "Break the link between an instance node and its symbol master, converting it to an independent editable node. The symbol registry is unaffected.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Instance node ID (UUID or name) to detach." }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "delete_symbol",
                "description": "Remove a named symbol from the registry. Existing instances are converted to standalone nodes (symbol_ref cleared). The master node itself is not deleted.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Symbol name to delete." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "spray_symbol_instances",
                "description": "Spray multiple instances of a named symbol scattered around a center point using a golden-angle spiral distribution for natural-looking placement. Like Illustrator's Symbol Sprayer tool. Supports undo per instance.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "symbol_name": { "type": "string", "description": "Name of the symbol to spray." },
                        "count":       { "type": "integer", "description": "Number of instances to place (1–200).", "minimum": 1, "maximum": 200 },
                        "x":           { "type": "number", "description": "Center X coordinate of the spray area." },
                        "y":           { "type": "number", "description": "Center Y coordinate of the spray area." },
                        "spread":      { "type": "number", "description": "Scatter radius in document units. Default: 100." }
                    },
                    "required": ["symbol_name", "count", "x", "y"]
                }
            },
            {
                "name": "load_symbol_library",
                "description": "Load a built-in symbol library into the document. Each library adds a set of named symbols (as hidden off-canvas master nodes) ready to be placed with place_symbol or spray_symbol_instances. Available libraries: 'arrows' (6 directional arrows), 'shapes' (diamond, hexagon, pentagon, star-5pt, cross, checkmark), 'ui' (checkbox-empty, checkbox-checked, radio-empty, close-x, menu-lines, plus-icon). Skips symbols that are already defined. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "library_name": { "type": "string", "enum": ["arrows", "shapes", "ui"], "description": "Name of the built-in library to load." }
                    },
                    "required": ["library_name"]
                }
            },
            {
                "name": "get_canvas_overview",
                "description": "Return a compact spatial map of all visible nodes: bounding box, layer, kind, and fill color for each node, plus the overall canvas bounds. Faster than get_document_state for layout queries. Useful for AI agents to understand spatial composition before placing or adjusting elements.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "include_hidden": { "type": "boolean", "description": "When true, include hidden nodes in the overview. Default: false." }
                    },
                    "required": []
                }
            },
            {
                "name": "save_gradient_swatch",
                "description": "Save the gradient fill of a node as a named gradient swatch. Works with linear, radial, fluid, and mesh gradients. If a swatch with the same name already exists it is updated. Use apply_gradient_swatch to reuse it on other nodes.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Path/text node ID (UUID or name) whose gradient fill to save." },
                        "name": { "type": "string", "description": "Unique name for the swatch." }
                    },
                    "required": ["node_id", "name"]
                }
            },
            {
                "name": "list_gradient_swatches",
                "description": "List all named gradient swatches saved in the document.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "apply_gradient_swatch",
                "description": "Apply a named gradient swatch to one or more path nodes, replacing their current fill. Undo-safe (one step per node).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": { "type": "array", "items": { "type": "string" }, "description": "Path node IDs (UUIDs or names) to apply the swatch to." },
                        "name": { "type": "string", "description": "Name of the gradient swatch to apply." }
                    },
                    "required": ["node_ids", "name"]
                }
            },
            {
                "name": "set_paint",
                "description": "Apply ONE paint to MANY nodes in a single undoable call — each node re-fit to its own bounding box. The paint uses the same object shape as `fill` (solid / linear / radial / fluid / mesh / pattern). For gradients, set `paint.units` to \"bbox\" with 0–1 `coords` to reuse one relative gradient (e.g. left→right blue→purple) across an entire icon set with zero per-node coordinate math — the coords are resolved to each node's bounding box automatically. `target` chooses the fill (default) or stroke slot; strokes accept gradient/pattern paints too (#201), so a whole set of gradient-stroked line icons is one call — no outline_stroke→delete→refill workaround.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Node IDs or names to paint."
                        },
                        "target": {
                            "type": "string",
                            "enum": ["fill", "stroke"],
                            "description": "Which paint slot to set (default: fill)."
                        },
                        "paint": {
                            "type": "object",
                            "description": "The paint (same shape as `fill`). For a bbox-relative gradient: { \"type\": \"gradient\", \"gradient_type\": \"linear\", \"colors\": [\"#2f56cf\",\"#7c3aed\"], \"coords\": [0,0,1,0], \"units\": \"bbox\" }.",
                            "properties": {
                                "type": { "type": "string", "enum": ["none","solid","gradient","fluid_gradient","mesh_gradient","pattern"] },
                                "color": { "type": "string" },
                                "gradient_type": { "type": "string", "enum": ["linear","radial"] },
                                "colors": { "type": "array", "items": { "type": "string" } },
                                "offsets": { "type": "array", "items": { "type": "number" } },
                                "coords": { "type": "array", "items": { "type": "number" } },
                                "units": { "type": "string", "enum": ["user","bbox","objectBoundingBox"], "description": "Coordinate space for `coords`. 'user' (default) = absolute; 'bbox' = 0–1 relative to each node's bounding box." }
                            },
                            "required": ["type"]
                        }
                    },
                    "required": ["node_ids", "paint"]
                }
            },
            {
                "name": "delete_gradient_swatch",
                "description": "Delete a named gradient swatch from the document registry. Does not affect nodes that were already painted with this gradient.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the gradient swatch to delete." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "analyze_composition",
                "description": "Analyze the visual composition of the current document and return advisory findings. Checks balance (quadrant distribution), density, object overlaps, color contrast, palette size, and off-canvas objects. Read-only — does not modify the document.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Optional list of node UUIDs or names to restrict the analysis to. Defaults to all visible nodes."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "detect_rhythms",
                "description": "Detect visual rhythm patterns in the document: evenly-spaced objects (horizontal/vertical), uniform widths, geometric size progressions, and rotational symmetry. Returns structured findings with descriptions and extension suggestions. Read-only.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Optional list of node UUIDs or names to restrict the analysis to. Defaults to all visible leaf nodes."
                        },
                        "min_count": {
                            "type": "integer",
                            "minimum": 2,
                            "description": "Minimum number of nodes required to form a pattern (default: 3)."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "define_action",
                "description": "Define (or overwrite) a named action set — a replayable sequence of MCP tool calls. Use to record multi-step workflows that can be replayed in one call. Node IDs in steps can be substituted at play time.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Unique action name." },
                        "steps": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "tool": { "type": "string", "description": "MCP tool name." },
                                    "args": { "type": "object", "description": "Tool arguments." }
                                },
                                "required": ["tool", "args"]
                            },
                            "minItems": 1,
                            "description": "Ordered list of tool steps."
                        }
                    },
                    "required": ["name", "steps"]
                }
            },
            {
                "name": "list_actions",
                "description": "List all named action sets in the document.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "delete_action",
                "description": "Delete a named action set.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the action to delete." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "play_action",
                "description": "Play a named action set, executing each recorded step in order. Optional substitutions replace node IDs or names from the recording with new values for the current run. Stops at first error.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the action set to play." },
                        "substitutions": {
                            "type": "object",
                            "additionalProperties": { "type": "string" },
                            "description": "Optional map of recorded node UUID/name → current node UUID/name."
                        }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "register_event_trigger",
                "description": "Register a script event trigger: map a document lifecycle event to a named action that executes automatically when the event fires. Valid events: on_open, on_save, on_node_create, on_selection_change. The named action must already exist.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "event": {
                            "type": "string",
                            "enum": ["on_open", "on_save", "on_node_create", "on_selection_change"],
                            "description": "Document lifecycle event to listen for."
                        },
                        "action_name": { "type": "string", "description": "Name of the action set to execute when the event fires." }
                    },
                    "required": ["event", "action_name"]
                }
            },
            {
                "name": "list_event_triggers",
                "description": "List all registered script event triggers in the document. Read-only.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "remove_event_trigger",
                "description": "Remove one or all event triggers for a given event. If action_name is omitted, all triggers for the event are removed.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "event": { "type": "string", "description": "Event name to remove triggers for." },
                        "action_name": { "type": "string", "description": "Optional: only remove the trigger pointing to this action name." }
                    },
                    "required": ["event"]
                }
            },
            {
                "name": "save_workspace",
                "description": "Save the current properties-panel filter query as a named workspace preset. Pass search_query to define which panel sections are visible (e.g. 'text font' shows typography sections). Overwrites any existing workspace with the same name. Stored on document.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name":         { "type": "string", "description": "Name for the workspace (e.g. 'Typography', 'Drawing')." },
                        "search_query": { "type": "string", "description": "Panel search filter to save. Empty string shows all panels." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "load_workspace",
                "description": "Load a saved workspace preset, returning its search_query for the GUI to apply. Read-only (does not mutate document state).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the workspace to load." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "list_workspaces",
                "description": "List all saved workspace presets in the document. Read-only.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "delete_workspace",
                "description": "Delete a named workspace preset from the document.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the workspace to delete." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "measure_distances",
                "description": "Measure edge-to-edge gaps, center-to-center distances, and alignment offsets between two or more nodes. For ≤6 nodes reports all pairs; for larger sets reports consecutive pairs. Read-only. Useful for verifying layout spacing.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 2,
                            "description": "UUIDs or names of at least 2 nodes to measure between."
                        }
                    },
                    "required": ["node_ids"]
                }
            },
            {
                "name": "define_grammar_rule",
                "description": "Define (or update) a named design grammar rule. Rules constrain the document: palette_includes (a specific color must appear), max_colors (palette size limit), min_text_size (minimum font size), required_layer (a named layer must exist), max_node_count (total node limit). Run check_grammar to validate.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Unique rule name (used as key in check results)." },
                        "rule_type": { "type": "string", "enum": ["palette_includes", "max_colors", "min_text_size", "required_layer", "max_node_count"], "description": "Rule type discriminator." },
                        "params": { "type": "object", "description": "Rule parameters: palette_includes={color_hex}, max_colors={count}, min_text_size={px}, required_layer={name or prefix}, max_node_count={count}." }
                    },
                    "required": ["name", "rule_type", "params"]
                }
            },
            {
                "name": "list_grammar_rules",
                "description": "List all named design grammar rules in the document.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "delete_grammar_rule",
                "description": "Delete a named design grammar rule.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the rule to delete." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "check_grammar",
                "description": "Check the document against its grammar rules. Returns per-rule pass/fail with descriptive messages. Read-only.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "rule_names": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Optional subset of rule names to check. Defaults to all rules."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "set_document_bleed",
                "description": "Set the print bleed and/or slug margins for the document. Bleed is the extra artwork bled past the trim edge (typically 3 mm) to prevent white borders after cutting. Slug is the additional area outside bleed reserved for printer marks and file info. Values persist in the .photonic file. Provide only the fields you want to change.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "bleed_mm": { "type": "number", "minimum": 0, "description": "Bleed in millimetres (all four sides). Typical values: 3.0 (EU) or 3.175 (US 0.125 in). Default: unchanged." },
                        "slug_mm": { "type": "number", "minimum": 0, "description": "Slug area in millimetres outside the bleed. Default: unchanged." }
                    },
                    "required": []
                }
            },
            {
                "name": "get_document_bleed",
                "description": "Return the current document bleed and slug values in millimetres. Read-only.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "set_document_color_mode",
                "description": "Set the document color mode to RGB or CMYK. CMYK is required for print-production PDF/X output. The mode persists in the .photonic file and is used as the default color space when exporting PDF without an explicit color_mode override.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "mode": { "type": "string", "enum": ["rgb", "cmyk"], "description": "Color mode. 'rgb' for screen/web; 'cmyk' for print production." }
                    },
                    "required": ["mode"]
                }
            },
            {
                "name": "get_document_color_mode",
                "description": "Return the current document color mode ('rgb' or 'cmyk'). Read-only.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "set_document_dpi",
                "description": "Set the document resolution (DPI) — the honored physical-size property on export: exported PDF/raster physical size = pixel size / dpi × 72 pt. A 1050×600 px document at 300 DPI exports at 252×144 pt (3.5×2 in); the same pixels at 72 DPI export at 1050×600 pt. Presets set this automatically (e.g. 300 for print). Default is 72 (px ≡ pt). Persists in the .photonic file.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "dpi": { "type": "number", "exclusiveMinimum": 0, "description": "Dots per inch. Common print value: 300." }
                    },
                    "required": ["dpi"]
                }
            },
            {
                "name": "get_document_dpi",
                "description": "Return the current document DPI and the physical page size (pt and inches) it implies for export. Read-only.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "set_artboard_margins",
                "description": "Set the artboard safe-area margins (top, right, bottom, left) in document units. Margins define the inner content area; content should stay within these guides. Pass only the fields you want to change.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "top":    { "type": "number", "minimum": 0, "description": "Top margin in document units. Default: unchanged." },
                        "right":  { "type": "number", "minimum": 0, "description": "Right margin in document units. Default: unchanged." },
                        "bottom": { "type": "number", "minimum": 0, "description": "Bottom margin in document units. Default: unchanged." },
                        "left":   { "type": "number", "minimum": 0, "description": "Left margin in document units. Default: unchanged." }
                    },
                    "required": []
                }
            },
            {
                "name": "get_artboard_margins",
                "description": "Return the current artboard safe-area margin values (top, right, bottom, left in document units). Read-only.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "list_artboards",
                "description": "List every artboard (named crop/export rectangle) in the document, with id, name, position (x, y = top-left), size (width, height), and which one is active. Artboards live in the shared document coordinate space; a fresh document has one 'Artboard 1' at the origin. Read-only. Use before update_artboard / remove_artboard / set_active_artboard to get ids. Distinct from the document canvas size (get_document_info) and from artboard safe-area margins (get_artboard_margins).",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "add_artboard",
                "description": "Create a new artboard (a named crop/export rectangle) at the given top-left (x, y) with width×height in document units, and make it the active artboard. Returns the new artboard_id. Use for multi-artboard layouts (e.g. responsive frames, icon sizes, print pages side by side).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name":   { "type": "string", "description": "Optional name. Default: 'Artboard N'." },
                        "x":      { "type": "number", "description": "Top-left X in document units." },
                        "y":      { "type": "number", "description": "Top-left Y in document units." },
                        "width":  { "type": "number", "exclusiveMinimum": 0, "description": "Width in document units (> 0)." },
                        "height": { "type": "number", "exclusiveMinimum": 0, "description": "Height in document units (> 0)." }
                    },
                    "required": ["x", "y", "width", "height"]
                }
            },
            {
                "name": "update_artboard",
                "description": "Edit an existing artboard's name, position (x, y = top-left) and/or size (width, height). Only the fields you pass change; others are left as-is. Repositioning with this tool moves only the artboard frame, not its contents. One undoable step. Get the artboard_id from list_artboards.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "artboard_id": { "type": "string", "description": "UUID of the artboard to edit (from list_artboards)." },
                        "name":   { "type": "string", "description": "New name. Default: unchanged." },
                        "x":      { "type": "number", "description": "New top-left X in document units. Default: unchanged." },
                        "y":      { "type": "number", "description": "New top-left Y in document units. Default: unchanged." },
                        "width":  { "type": "number", "exclusiveMinimum": 0, "description": "New width in document units (> 0). Default: unchanged." },
                        "height": { "type": "number", "exclusiveMinimum": 0, "description": "New height in document units (> 0). Default: unchanged." }
                    },
                    "required": ["artboard_id"]
                }
            },
            {
                "name": "duplicate_artboard",
                "description": "Duplicate an artboard together with all content/nodes inside it, as one undoable step. The copy becomes active. By default it is placed to the right of the original with a 40-unit gutter (offset_x = original width + 40, offset_y = 0). Get the artboard_id from list_artboards.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "artboard_id": { "type": "string", "description": "UUID of the artboard to duplicate (from list_artboards)." },
                        "offset_x": { "type": "number", "description": "Horizontal offset for the copy in document units. Default: original width + 40." },
                        "offset_y": { "type": "number", "description": "Vertical offset for the copy in document units. Default: 0." },
                        "new_name": { "type": "string", "description": "Optional name for the copy. Default: '{original name} copy'." }
                    },
                    "required": ["artboard_id"]
                }
            },
            {
                "name": "move_artboard",
                "description": "Move an artboard together with all content/nodes inside it by (dx, dy), as one undoable step. Unlike update_artboard, which repositions only the frame, this tool also moves the artboard's contents. Get the artboard_id from list_artboards.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "artboard_id": { "type": "string", "description": "UUID of the artboard to move (from list_artboards)." },
                        "dx": { "type": "number", "description": "Horizontal movement in document units." },
                        "dy": { "type": "number", "description": "Vertical movement in document units." }
                    },
                    "required": ["artboard_id", "dx", "dy"]
                }
            },
            {
                "name": "remove_artboard",
                "description": "Delete an artboard by id. Refuses to remove the last remaining artboard (a document must keep at least one). If the removed artboard was active, the first remaining one becomes active. Get the artboard_id from list_artboards.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "artboard_id": { "type": "string", "description": "UUID of the artboard to remove (from list_artboards)." }
                    },
                    "required": ["artboard_id"]
                }
            },
            {
                "name": "set_active_artboard",
                "description": "Make an artboard the active one. The active artboard is the default target for artboard-relative operations and export. Soft view state — not an undo step. Get the artboard_id from list_artboards.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "artboard_id": { "type": "string", "description": "UUID of the artboard to activate (from list_artboards)." }
                    },
                    "required": ["artboard_id"]
                }
            },
            {
                "name": "list_history",
                "description": "Return the most recent edit history entries from the undo stack, newest first. Useful for understanding what an AI agent has done to a document, auditing changes, or deciding which node to revert with `undo_node`. Read-only.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "minimum": 1, "maximum": 200, "description": "Maximum entries to return. Default: 20." }
                    },
                    "required": []
                }
            },
            {
                "name": "jump_to_history",
                "description": "Jump to a specific position in the document's edit history by undoing or redoing the required number of steps. index=0 is the empty-document state; index equal to the current undo_depth means no change. Values beyond the maximum (undo_depth + redo_depth) are clamped. Use list_history to see the current depth and available steps.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "index": { "type": "integer", "minimum": 0, "description": "Target history depth. 0 = undo all; undo_depth() = current state." }
                    },
                    "required": ["index"]
                }
            },
            {
                "name": "fit_to_margins",
                "description": "Scale and position nodes to fill the artboard safe area (artboard bounds minus the set margins). By default preserves aspect ratio (uniform=true) and centers content in the safe area. Requires margins to be set with set_artboard_margins. GUI: 'Fit to Margins' button in the Artboard Margins panel (visible when selection exists and any margin > 0).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Node UUIDs or names to fit. Omit to fit all visible nodes."
                        },
                        "uniform": { "type": "boolean", "description": "Preserve aspect ratio while scaling. Default: true." },
                        "padding": { "type": "number", "description": "Additional inset inside the margin rectangle in document units. Default: 0." }
                    },
                    "required": []
                }
            },
            {
                "name": "add_dimension",
                "description": "Add a dimension annotation showing the distance between two nodes. The annotation is rendered as an arrow line with a distance label in the canvas overlay (visible when guides are shown). Strips from all exports. Use list_dimensions to see all annotations and remove_dimension to delete one.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "from_node_id": { "type": "string", "description": "UUID or name of the first node." },
                        "to_node_id":   { "type": "string", "description": "UUID or name of the second node." },
                        "axis":         { "type": "string", "enum": ["x", "y", "diagonal"], "description": "Measurement axis. 'x' = horizontal only, 'y' = vertical only, 'diagonal' = Euclidean. Default: 'diagonal'." },
                        "label_offset": { "type": "number", "description": "Perpendicular visual offset of the line from the node centers in document units. Default: 20." }
                    },
                    "required": ["from_node_id", "to_node_id"]
                }
            },
            {
                "name": "list_dimensions",
                "description": "List all dimension annotations in the document, including their IDs, node references, axis, and measured distance. Read-only.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "remove_dimension",
                "description": "Remove a dimension annotation by its ID. Use list_dimensions to find the ID.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "UUID of the dimension annotation to remove." }
                    },
                    "required": ["id"]
                }
            },
            {
                "name": "define_spot_color",
                "description": "Define (or update) a named spot color. Spot colors are named inks with optional overprint behavior. Unlike regular color swatches, they carry print-production semantics and can be applied as solid fills.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Unique spot color name (e.g. 'Pantone 485 C')." },
                        "hex": { "type": "string", "description": "Hex color value (e.g. '#FF2400'). Leading # is optional." },
                        "overprint": { "type": "boolean", "description": "When true, ink overprints underlying colors (print production). Default: false." }
                    },
                    "required": ["name", "hex"]
                }
            },
            {
                "name": "list_spot_colors",
                "description": "List all named spot colors defined in the document.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "apply_spot_color",
                "description": "Apply a named spot color as a solid fill to one or more nodes. The node's fill becomes the spot color's hex value.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Node UUIDs or names to apply the spot color to."
                        },
                        "name": { "type": "string", "description": "Name of the spot color to apply." }
                    },
                    "required": ["node_ids", "name"]
                }
            },
            {
                "name": "delete_spot_color",
                "description": "Delete a named spot color from the document. Does not alter existing node fills.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the spot color to delete." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "branch_create",
                "description": "Save the current document state as a named branch. If a branch with the same name already exists it is overwritten. Branches are stored in-memory and do not persist to disk.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Branch name (e.g. 'main', 'experiment-a')." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "branch_list",
                "description": "List all named document branches saved in the current session.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "branch_switch",
                "description": "Restore the document to a previously saved named branch. Clears the undo/redo history. Equivalent to checking out a branch in version control.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the branch to restore." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "branch_delete",
                "description": "Delete a named branch. The live document is not affected.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the branch to delete." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "define_variable",
                "description": "Define (or update) a named document variable. Variables are key-value string pairs that can be bound to text nodes and applied in batch with apply_variables. Useful for data-driven design: names, prices, dates, labels.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Unique variable name." },
                        "value": { "type": "string", "description": "String value." }
                    },
                    "required": ["name", "value"]
                }
            },
            {
                "name": "list_variables",
                "description": "List all named document variables and their current values.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "set_variable_value",
                "description": "Update the value of an existing document variable. Use apply_variables to propagate the change to all bound text nodes.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Variable name to update." },
                        "value": { "type": "string", "description": "New string value." }
                    },
                    "required": ["name", "value"]
                }
            },
            {
                "name": "delete_variable",
                "description": "Delete a named document variable. Does not unbind existing text nodes — their binding name is retained but will no longer update.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Variable name to delete." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "apply_variables",
                "description": "Apply all document variables — replaces the text content of every bound text node with its variable's current value. This is the main dispatch step for data-driven design. Supports undo (single batch command).",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "bind_text_variable",
                "description": "Bind a text node to a document variable. When apply_variables is called, this node's content will be replaced by the variable's current value. The variable must exist (use define_variable first). Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Text node ID (UUID or name)." },
                        "variable_name": { "type": "string", "description": "Variable name to bind to." }
                    },
                    "required": ["node_id", "variable_name"]
                }
            },
            {
                "name": "unbind_text_variable",
                "description": "Remove the variable binding from a text node. The node's current text content is preserved. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Text node ID (UUID or name)." }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "set_text_area",
                "description": "Flow a text node inside a closed path boundary (Area Type). The text reflows to fill the area defined by the given path node. The path node remains a separate visible object; hide it if not needed. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text_node_id": { "type": "string", "description": "Text node ID (UUID or name) to flow inside the area." },
                        "area_path_id": { "type": "string", "description": "Closed path node ID (UUID or name) defining the text boundary." }
                    },
                    "required": ["text_node_id", "area_path_id"]
                }
            },
            {
                "name": "clear_text_area",
                "description": "Remove the area boundary from a text node, reverting it to normal point text. The former area path node is unaffected. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text_node_id": { "type": "string", "description": "Text node ID (UUID or name) with an active area path." }
                    },
                    "required": ["text_node_id"]
                }
            },
            {
                "name": "set_paragraph_options",
                "description": "Set paragraph-level text options on a text node: spacing before paragraphs, spacing after paragraphs, and first-line indent. All fields are optional — pass only those you want to change. Values are in document units. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id":        { "type": "string", "description": "Text node ID or name." },
                        "spacing_before": { "type": "number", "description": "Space before each paragraph in document units. Default: unchanged." },
                        "spacing_after":  { "type": "number", "description": "Space after each paragraph in document units. Default: unchanged." },
                        "indent":         { "type": "number", "description": "First-line indent in document units. Default: unchanged." }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "set_tab_stops",
                "description": "Set explicit tab stop positions on a text node. Stops are specified in document units and are automatically sorted ascending. Replaces all existing tab stops. Use clear_tab_stops to revert to default tab spacing. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Text node ID or name." },
                        "stops":   { "type": "array", "items": { "type": "number" }, "description": "Tab stop positions in document units." }
                    },
                    "required": ["node_id", "stops"]
                }
            },
            {
                "name": "clear_tab_stops",
                "description": "Remove all custom tab stops from a text node, restoring default tab spacing (every 4 em widths). Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Text node ID or name." }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "set_text_decoration",
                "description": "Set the text decoration on a text node: underline, line-through (strikethrough), overline, or none. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id":    { "type": "string", "description": "Text node ID or name." },
                        "decoration": { "type": "string", "enum": ["none", "underline", "line-through", "overline"], "description": "Decoration to apply." }
                    },
                    "required": ["node_id", "decoration"]
                }
            },
            {
                "name": "set_character_metrics",
                "description": "Set advanced node-level character metrics on a text node: baseline shift and super/subscript position. baseline_shift is in document units (positive raises text above the baseline, negative lowers it). script_position is 'normal', 'superscript' (renders smaller and raised), or 'subscript' (renders smaller and lowered). Both fields are optional — pass only those you want to change. Applies to the whole node (per-character ranges are not yet supported). Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id":         { "type": "string", "description": "Text node ID or name." },
                        "baseline_shift":  { "type": "number", "description": "Baseline shift in document units (positive = up). Default: unchanged." },
                        "script_position": { "type": "string", "enum": ["normal", "superscript", "subscript"], "description": "Script position. Default: unchanged." }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "set_opentype_features",
                "description": "Set, add, or remove OpenType feature tags on a text node. Common tags: liga (ligatures), calt (contextual alternates), frac (fractions), smcp (small caps), sups (superscript), subs (subscript), ordn (ordinals), swsh (swashes), dlig (discretionary ligatures). Mode 'set' replaces all features, 'add' appends unique entries, 'remove' removes listed entries. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id":  { "type": "string", "description": "Text node ID or name." },
                        "features": { "type": "array", "items": { "type": "string" }, "description": "OpenType feature tag strings (4-letter codes)." },
                        "mode":     { "type": "string", "enum": ["set", "add", "remove"], "description": "How to apply. Default: set." }
                    },
                    "required": ["node_id", "features"]
                }
            },
            {
                "name": "get_opentype_features",
                "description": "Return the active OpenType feature tags on a text node. Read-only.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Text node ID or name." }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "link_text_frames",
                "description": "Link two text nodes as a threaded text chain so that content overflow from the upstream frame flows into the downstream frame. Both nodes must be text nodes. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "from_id": { "type": "string", "description": "ID or name of the upstream text node (overflow flows out from here)." },
                        "to_id":   { "type": "string", "description": "ID or name of the downstream text node (overflow flows into here)." }
                    },
                    "required": ["from_id", "to_id"]
                }
            },
            {
                "name": "unlink_text_frames",
                "description": "Remove a text node from its thread chain, severing both the previous and next frame links while preserving adjacent nodes. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "ID or name of the text node to remove from its thread chain." }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "set_blend_spine",
                "description": "Assign a path node (child of the group) as the blend spine for a group node. The spine path guides interpolation between objects in a blend group. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "group_id": { "type": "string", "description": "UUID or name of the group node to configure as a blend." },
                        "path_id":  { "type": "string", "description": "UUID or name of the path node to use as the blend spine." }
                    },
                    "required": ["group_id", "path_id"]
                }
            },
            {
                "name": "clear_blend_spine",
                "description": "Remove the blend spine assignment from a group node, reverting it to default straight-line interpolation. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "group_id": { "type": "string", "description": "UUID or name of the group node whose blend spine should be cleared." }
                    },
                    "required": ["group_id"]
                }
            },
            {
                "name": "reverse_blend_spine",
                "description": "Reverse the direction of the blend spine path in a group node. This inverts the order of blend interpolation from start-to-end to end-to-start. The group must have a blend spine assigned. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "group_id": { "type": "string", "description": "UUID or name of the group node whose blend spine should be reversed." }
                    },
                    "required": ["group_id"]
                }
            },
            {
                "name": "expand_blend",
                "description": "Expand a blend group into individual discrete objects. Dissolves the group wrapper and places all child objects as standalone nodes at the parent layer position — equivalent to Illustrator's Object > Blend > Expand. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "group_id": { "type": "string", "description": "UUID or name of the blend group to expand." }
                    },
                    "required": ["group_id"]
                }
            },
            {
                "name": "set_symbol_override",
                "description": "Set per-instance fill and/or stroke color overrides on a symbol instance node. Overrides apply to this instance only; the master symbol is unaffected. Pass fill_hex and/or stroke_hex as '#rrggbb' strings. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id":    { "type": "string", "description": "UUID or name of the symbol instance node." },
                        "fill_hex":   { "type": "string", "description": "Fill color override as '#rrggbb'. Omit to leave unchanged." },
                        "stroke_hex": { "type": "string", "description": "Stroke color override as '#rrggbb'. Omit to leave unchanged." }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "clear_symbol_overrides",
                "description": "Clear all per-instance color overrides on a symbol instance node, reverting it to the master's fill and stroke. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "UUID or name of the symbol instance node to reset." }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "set_text_direction",
                "description": "Set the layout direction of a text node. When vertical is true, characters are stacked top-to-bottom (Vertical Type). When false (default), text flows left-to-right normally. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Text node ID (UUID or name)." },
                        "vertical": { "type": "boolean", "description": "true = vertical top-to-bottom, false = normal horizontal." }
                    },
                    "required": ["node_id", "vertical"]
                }
            },
            {
                "name": "set_font_style",
                "description": "Set the font style (normal, italic, or oblique) on a text node. Italic uses a true italic face if the font provides one; oblique synthesizes slant. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Text node ID (UUID or name)." },
                        "style": { "type": "string", "enum": ["normal", "italic", "oblique"], "description": "Font style to apply." }
                    },
                    "required": ["node_id", "style"]
                }
            },
            {
                "name": "set_font_weight",
                "description": "Set the font weight (100–900) on a text node. Common values: 400 = Regular, 700 = Bold. Clamped to valid range. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Text node ID (UUID or name)." },
                        "weight": { "type": "integer", "minimum": 100, "maximum": 900, "description": "Font weight (100=Thin, 400=Regular, 700=Bold, 900=Black)." }
                    },
                    "required": ["node_id", "weight"]
                }
            },
            {
                "name": "flatten_transparency",
                "description": "Bake node opacity and fill/stroke opacity into color alpha values for print-ready output. After flattening, all processed nodes have opacity=1.0 with colors premultiplied. Group opacity is not baked (children are processed individually). Irreversible — uses a single undoable batch command.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Node UUIDs or names to process. Defaults to all nodes in the document."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "apply_flex_layout",
                "description": "Redistribute the direct children of a Group node in a flex-like arrangement. Children are sorted by their current position along the main axis, then repositioned sequentially with a fixed gap between them. Cross-axis alignment ('start', 'center', 'end') aligns shorter children relative to the tallest/widest. Optional padding offsets the origin. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "group_id": { "type": "string", "description": "Group node ID (UUID or name) whose children will be laid out." },
                        "direction": { "type": "string", "enum": ["row", "column"], "description": "Layout direction. 'row' arranges children left-to-right, 'column' top-to-bottom. Default: 'row'." },
                        "gap": { "type": "number", "description": "Gap in document units between consecutive children. Default: 8.0." },
                        "align": { "type": "string", "enum": ["start", "center", "end"], "description": "Cross-axis alignment. Default: 'center'." },
                        "padding": { "type": "number", "description": "Offset from origin before placing the first child. Default: 0.0." }
                    },
                    "required": ["group_id"]
                }
            },
            {
                "name": "apply_grid_layout",
                "description": "Arrange the direct children of a Group node in a CSS-grid-style layout: left-to-right, top-to-bottom, with uniform column width (max child width) and row height (max child height). `columns` controls how many children appear per row before wrapping. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "group_id": { "type": "string", "description": "Group node ID (UUID or name) whose children will be laid out." },
                        "columns": { "type": "integer", "minimum": 1, "description": "Number of columns per row. Default: 3." },
                        "gap_x": { "type": "number", "description": "Horizontal gap between columns in document units. Default: 8.0." },
                        "gap_y": { "type": "number", "description": "Vertical gap between rows in document units. Default: 8.0." },
                        "padding": { "type": "number", "description": "Offset from origin before placing the first cell. Default: 0.0." }
                    },
                    "required": ["group_id"]
                }
            },
            {
                "name": "apply_stack_layout",
                "description": "Stack all children of a Group node at the same position, creating a Z-stack (like CSS `position: absolute` on all children). Each child is repositioned to align its anchor point with the group's union bounding box. Useful for layered compositions, badge overlays, or icon-over-background patterns. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "group_id": { "type": "string", "description": "Group node ID (UUID or name) whose children will be stacked." },
                        "align_h":  { "type": "string", "enum": ["left", "center", "right"], "description": "Horizontal alignment anchor. Default: center." },
                        "align_v":  { "type": "string", "enum": ["top", "center", "bottom"], "description": "Vertical alignment anchor. Default: center." }
                    },
                    "required": ["group_id"]
                }
            },
            {
                "name": "undo_node",
                "description": "Revert a specific node to its state N edits ago without undoing anything else in the document. Scans the undo history for UpdateNode commands targeting the given node and applies the N-th-most-recent pre-mutation snapshot as a new undoable command — so the revert itself can be undone with a global Ctrl+Z.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "UUID or name of the node to revert." },
                        "steps": { "type": "integer", "minimum": 1, "description": "How many node-specific edits to revert. Default: 1." }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "set_text_path",
                "description": "Place a text node along a path spine (Type on a Path). The text flows along the curve starting at `offset` document units from the path start. The path node remains visible as a separate object; hide it manually if not needed. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text_node_id": { "type": "string", "description": "Text node ID (UUID or name) to place on the path." },
                        "path_node_id": { "type": "string", "description": "Path node ID (UUID or name) to use as the text spine." },
                        "offset": { "type": "number", "description": "Start offset along the path in document units. Default: 0.0." }
                    },
                    "required": ["text_node_id", "path_node_id"]
                }
            },
            {
                "name": "clear_text_path",
                "description": "Remove the path spine from a text node, reverting it to normal positioned text. The former spine path node is unaffected. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text_node_id": { "type": "string", "description": "Text node ID (UUID or name) currently on a path." }
                    },
                    "required": ["text_node_id"]
                }
            },
            {
                "name": "make_clipping_mask",
                "description": "Create a clipping mask on a group node. The topmost child (last in the group's child order) becomes the clip path; all other children are masked to that shape. The clip path node is preserved in the group but rendered only as a mask. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "group_id": {
                            "type": "string",
                            "description": "Group node ID (UUID or name). Must contain at least 2 children."
                        }
                    },
                    "required": ["group_id"]
                }
            },
            {
                "name": "release_clipping_mask",
                "description": "Release the clipping mask from a group node. All children revert to normal visible objects; the former clip path node remains in the group as a regular object. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "group_id": {
                            "type": "string",
                            "description": "Group node ID (UUID or name) that currently has a clipping mask."
                        }
                    },
                    "required": ["group_id"]
                }
            },

            // ── Video domain (10-mcp-tools.md, P2: timeline-EDIT tools) ─────────────
            // Sequence (10 §3.2)
            {
                "name": "create_sequence",
                "description": "Create a new timeline sequence (creates the timeline project itself, undoably, if this is the first video-mode action). Requires at least one SequenceFormat (aspect-ratio variant, CAP-012). Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "frame_rate": { "type": "object", "description": "{\"num\": 30, \"den\": 1} — a rational frame rate, e.g. 30000/1001 for 29.97 fps." },
                        "formats": {
                            "type": "array",
                            "description": "At least one SequenceFormat (aspect-ratio variant).",
                            "items": { "type": "object", "properties": { "name": {"type":"string"}, "width": {"type":"integer"}, "height": {"type":"integer"} }, "required": ["name","width","height"] }
                        }
                    },
                    "required": ["name","frame_rate","formats"]
                }
            },
            {
                "name": "delete_sequence",
                "description": "Delete a sequence. Fails if referenced by a NestedSequence clip elsewhere (cycle/dangling-ref guard). Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "sequence_id": { "type": "string" } },
                    "required": ["sequence_id"]
                }
            },
            {
                "name": "list_sequences",
                "description": "List all sequences in the timeline project.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "set_active_sequence",
                "description": "Set (or clear, if omitted/null) the project's active sequence. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "sequence_id": { "type": ["string","null"] } }
                }
            },
            {
                "name": "set_sequence_format",
                "description": "Add, update, or remove a SequenceFormat (aspect-ratio variant) on a sequence. op=add requires `format`; op=update requires `format_index` and `format`; op=remove requires `format_index` (refused if it's the sequence's last format). Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string" },
                        "op": { "type": "string", "enum": ["add","update","remove"] },
                        "format": { "type": "object", "description": "{\"name\":str,\"width\":int,\"height\":int} — required for add/update.", "properties": { "name": {"type":"string"}, "width": {"type":"integer"}, "height": {"type":"integer"} } },
                        "format_index": { "type": "integer", "description": "Required for update/remove." }
                    },
                    "required": ["sequence_id","op"]
                }
            },
            {
                "name": "set_active_format",
                "description": "Switch a sequence's active aspect-ratio variant by index. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "sequence_id": { "type": "string" }, "format_index": { "type": "integer" } },
                    "required": ["sequence_id","format_index"]
                }
            },
            {
                "name": "set_work_range",
                "description": "Set (or clear, by omitting `range`) a sequence's preview/export in/out work range. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string" },
                        "range": {
                            "type": ["object","null"],
                            "description": "null/omitted clears the work range.",
                            "properties": {
                                "start_ticks": { "type": "integer" }, "start_tc": { "type": "string" }, "start_seconds": { "type": "number" },
                                "end_ticks": { "type": "integer" }, "end_tc": { "type": "string" }, "end_seconds": { "type": "number" }
                            }
                        }
                    },
                    "required": ["sequence_id"]
                }
            },
            {
                "name": "add_marker",
                "description": "Add a marker to a sequence. Give a duration (or end_tc) to create a RANGED marker — the unit export_per_marker exports. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string" },
                        "at_ticks": { "type": "integer" },
                        "at_tc": { "type": "string" },
                        "at_seconds": { "type": "number" },
                        "name": { "type": "string" },
                        "color": { "type": "string", "description": "#rrggbb or #rrggbbaa." },
                        "note": { "type": "string" },
                        "duration_ticks": { "type": "integer", "description": "0 (default) = point marker; > 0 = ranged." },
                        "duration_seconds": { "type": "number" },
                        "end_tc": { "type": "string", "description": "Alternative to a duration: duration = end_tc - at." },
                        "category_id": { "type": "string", "description": "A MarkerCategory id from list_marker_categories." }
                    },
                    "required": ["sequence_id"]
                }
            },
            {
                "name": "remove_marker",
                "description": "Remove a sequence marker by id. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "marker_id": { "type": "string" } },
                    "required": ["marker_id"]
                }
            },
            {
                "name": "list_markers",
                "description": "List a sequence's markers.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "sequence_id": { "type": "string" } },
                    "required": ["sequence_id"]
                }
            },
            {
                "name": "set_marker",
                "description": "Universal marker editor — position, duration (0 = point, > 0 = ranged), name, note, colour, category. Only supplied fields change. Pass clip_id to edit a clip-scoped marker instead of a sequence marker. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "marker_id": { "type": "string" },
                        "clip_id": { "type": "string", "description": "Omit for a sequence marker; supply to edit that clip's own marker." },
                        "at_ticks": { "type": "integer" },
                        "at_tc": { "type": "string" },
                        "at_seconds": { "type": "number" },
                        "duration_ticks": { "type": "integer" },
                        "duration_seconds": { "type": "number" },
                        "end_tc": { "type": "string" },
                        "name": { "type": "string" },
                        "note": { "type": "string" },
                        "color": { "type": "string", "description": "#rrggbb / #rrggbbaa, or \"\" to clear the override and use the category colour." },
                        "category_id": { "type": "string" },
                        "clear_category": { "type": "boolean", "description": "Clear the category reference (category_id is then ignored)." }
                    },
                    "required": ["marker_id"]
                }
            },
            {
                "name": "add_clip_marker",
                "description": "Add a clip-scoped marker. `at` is CLIP-RELATIVE (0 = the clip's first frame) and the marker travels with the clip. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "at_ticks": { "type": "integer" },
                        "at_tc": { "type": "string", "description": "Parsed as a duration into the clip, not a sequence timecode." },
                        "at_seconds": { "type": "number" },
                        "duration_ticks": { "type": "integer" },
                        "duration_seconds": { "type": "number" },
                        "name": { "type": "string" },
                        "note": { "type": "string" },
                        "color": { "type": "string", "description": "#rrggbb or #rrggbbaa." },
                        "category_id": { "type": "string" }
                    },
                    "required": ["clip_id"]
                }
            },
            {
                "name": "remove_clip_marker",
                "description": "Remove a clip-scoped marker. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "clip_id": { "type": "string" }, "marker_id": { "type": "string" } },
                    "required": ["clip_id","marker_id"]
                }
            },
            {
                "name": "list_clip_markers",
                "description": "List a clip's own markers. Each entry carries the clip-relative `at` plus the derived `sequence_tick`.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "clip_id": { "type": "string" } },
                    "required": ["clip_id"]
                }
            },
            {
                "name": "list_marker_categories",
                "description": "List the project's marker categories (id, name, colour, glyph).",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "add_marker_category",
                "description": "Add a project marker category. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "color": { "type": "string", "description": "#rrggbb or #rrggbbaa." },
                        "glyph": { "type": "string", "enum": ["diamond","circle","square","triangle","flag","bar"] }
                    },
                    "required": ["name","color"]
                }
            },
            {
                "name": "seed_marker_categories",
                "description": "Seed the default marker categories (Marker/Cut/Note/Todo/Chapter/Bookmarks) as ONE undo step. No-op if the project already has categories.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "update_marker_category",
                "description": "Rename / recolour / re-glyph a marker category in place; its id and every reference to it are preserved. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "category_id": { "type": "string" },
                        "name": { "type": "string" },
                        "color": { "type": "string", "description": "#rrggbb or #rrggbbaa." },
                        "glyph": { "type": "string", "enum": ["diamond","circle","square","triangle","flag","bar"] }
                    },
                    "required": ["category_id"]
                }
            },
            {
                "name": "remove_marker_category",
                "description": "Remove a marker category, reassigning every marker that referenced it to `reassign_to` (or clearing the reference when omitted) in the same undo step. Markers are never silently remapped. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "category_id": { "type": "string" },
                        "reassign_to": { "type": "string", "description": "Another category id; omit to clear the reference instead." }
                    },
                    "required": ["category_id"]
                }
            },

            // Track (10 §3.3)
            {
                "name": "add_track",
                "description": "Append (or insert at `index`) a video or audio track on a sequence. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string" },
                        "kind": { "type": "string", "enum": ["video","audio"] },
                        "name": { "type": "string" },
                        "index": { "type": "integer", "description": "Insertion index within the track's lane; defaults to the end." }
                    },
                    "required": ["sequence_id","kind"]
                }
            },
            {
                "name": "remove_track",
                "description": "Remove a track (and every clip on it) by id. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "track_id": { "type": "string" } },
                    "required": ["track_id"]
                }
            },
            {
                "name": "set_track_prop",
                "description": "Universal track-property setter — name/enabled/locked/height_px, only supplied fields change. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "track_id": { "type": "string" },
                        "name": { "type": "string" },
                        "enabled": { "type": "boolean" },
                        "locked": { "type": "boolean" },
                        "height_px": { "type": "number" }
                    },
                    "required": ["track_id"]
                }
            },
            {
                "name": "reorder_track",
                "description": "Move a track to a new index within its own lane (video tracks reorder among video tracks, audio among audio). Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "track_id": { "type": "string" }, "new_index": { "type": "integer" } },
                    "required": ["track_id","new_index"]
                }
            },

            // Clip edit ops (10 §3.4)
            {
                "name": "insert_clip",
                "description": "Insert a new clip on a track. Time args follow ticks > tc > seconds precedence (at_tc requires the track's sequence context, resolved automatically). Fails on overlap with an existing clip, non-positive duration, or a NestedSequence cycle. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "track_id": { "type": "string" },
                        "name": { "type": "string" },
                        "start_ticks": { "type": "integer" },
                        "start_tc": { "type": "string", "description": "HH:MM:SS:FF or HH:MM:SS;FF" },
                        "start_seconds": { "type": "number" },
                        "source": { "type": "object", "description": "ClipSource — {\"kind\":\"asset\",\"asset_id\":...} | {\"kind\":\"vector\",\"asset_id\":...} | {\"kind\":\"nested_sequence\",\"sequence_id\":...} | {\"kind\":\"solid_color\",\"color\":\"#rrggbb\"} | {\"kind\":\"adjustment\"}" },
                        "source_in_ticks": { "type": "integer" },
                        "source_in_tc": { "type": "string" },
                        "source_in_seconds": { "type": "number" },
                        "duration_ticks": { "type": "integer", "description": "Always exact ticks — no dual-unit ambiguity." }
                    },
                    "required": ["track_id","source","duration_ticks"]
                }
            },
            {
                "name": "move_clip",
                "description": "Move a clip to a new start position, optionally onto a different track of the same kind (`new_track_id`, cross-track move — the destination must have room, non-overlap enforced). Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "new_start_ticks": { "type": "integer" },
                        "new_start_tc": { "type": "string" },
                        "new_start_seconds": { "type": "number" },
                        "new_track_id": { "type": "string", "description": "Omit for a same-track move. Must be the same TrackKind (video/audio) as the clip's current track." }
                    },
                    "required": ["clip_id"]
                }
            },
            {
                "name": "move_clips",
                "description": "Shift several clips along the timeline by one shared time delta and optional same-kind track_delta (lane index offset), preserving relative spacing, in a single undo step. Linked partners ride along automatically and are never moved twice. The move is refused as a whole if any clip would land on a clip outside the set, before zero, or off the lane list; collisions *within* the set are fine, since the set is displaced as a body.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_ids": { "type": "array", "items": { "type": "string" }, "description": "All must be in the same sequence." },
                        "delta_ticks": { "type": "integer", "description": "Signed time offset applied to every clip. Negative moves earlier." },
                        "track_delta": { "type": "integer", "description": "Same-kind lane-index offset (0 = stay on track). Video and audio lists are separate." }
                    },
                    "required": ["clip_ids","delta_ticks"]
                }
            },
            {
                "name": "trim_clip",
                "description": "Trim a clip's in or out edge to an exact position. edge=in adjusts start+source_in and shortens/lengthens duration, keeping the out point fixed; edge=out changes only duration. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "edge": { "type": "string", "enum": ["in","out"] },
                        "new_ticks": { "type": "integer" },
                        "new_tc": { "type": "string" },
                        "new_seconds": { "type": "number" }
                    },
                    "required": ["clip_id","edge"]
                }
            },
            {
                "name": "split_clip",
                "description": "Split a clip into two at an exact tick, strictly inside the clip's span. Returns the new (right-hand) clip's id. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "at_ticks": { "type": "integer" },
                        "at_tc": { "type": "string" },
                        "at_seconds": { "type": "number" }
                    },
                    "required": ["clip_id"]
                }
            },
            {
                "name": "remove_clip",
                "description": "Remove a clip. ripple=true also shifts every later clip on the track left by the removed clip's duration (one undo step). Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "ripple": { "type": "boolean", "description": "Default false." }
                    },
                    "required": ["clip_id"]
                }
            },
            {
                "name": "roll_edit",
                "description": "Roll the shared edge between two adjacent clips on the same track — one clip's out point and the other's in point move together by delta_ticks, total span unchanged. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id_a": { "type": "string" },
                        "clip_id_b": { "type": "string" },
                        "delta_ticks": { "type": "integer", "description": "Positive = later." }
                    },
                    "required": ["clip_id_a","clip_id_b","delta_ticks"]
                }
            },
            {
                "name": "slip_clip",
                "description": "Shift a clip's source in/out (source_in) by delta_ticks without moving it on the timeline. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "clip_id": { "type": "string" }, "delta_ticks": { "type": "integer" } },
                    "required": ["clip_id","delta_ticks"]
                }
            },
            {
                "name": "slide_clip",
                "description": "Slide a clip over its neighbors by delta_ticks — the clip moves, its immediate neighbors trim to absorb the change, total span unchanged. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "clip_id": { "type": "string" }, "delta_ticks": { "type": "integer" } },
                    "required": ["clip_id","delta_ticks"]
                }
            },
            {
                "name": "ripple_edit",
                "description": "Ripple-trim one edge of a clip by delta_ticks and shift every later clip on the track to close/open the resulting gap, as ONE undo step. edge=in trims the in-point (source_in advances, speed-scaled); edge=out trims the out-point. Distinct from remove_clip's ripple flag (which deletes the clip).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "edge": { "type": "string", "enum": ["in","out"] },
                        "delta_ticks": { "type": "integer" }
                    },
                    "required": ["clip_id","edge","delta_ticks"]
                }
            },

            // 3/4-point editing (16 §2, CAP-019 MCP parity)
            {
                "name": "insert_edit",
                "description": "Insert edit (3-point, Premiere ','): open a gap of source's duration at `at` on track_id — splitting any clip straddling `at` and rippling every clip at/after `at` on that track right — then drop source into the gap. The track's content grows by the source duration, as ONE undo step. Time/source args mirror insert_clip (at_* plays start_*'s role). Returns the new clip's id.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "track_id": { "type": "string" },
                        "name": { "type": "string" },
                        "at_ticks": { "type": "integer" },
                        "at_tc": { "type": "string", "description": "HH:MM:SS:FF or HH:MM:SS;FF" },
                        "at_seconds": { "type": "number" },
                        "source": { "type": "object", "description": "ClipSource — {\"kind\":\"asset\",\"asset_id\":...} | {\"kind\":\"vector\",\"asset_id\":...} | {\"kind\":\"nested_sequence\",\"sequence_id\":...} | {\"kind\":\"solid_color\",\"color\":\"#rrggbb\"} | {\"kind\":\"adjustment\"}" },
                        "source_in_ticks": { "type": "integer" },
                        "source_in_tc": { "type": "string" },
                        "source_in_seconds": { "type": "number" },
                        "duration_ticks": { "type": "integer", "description": "Always exact ticks — no dual-unit ambiguity." }
                    },
                    "required": ["track_id","source","duration_ticks"]
                }
            },
            {
                "name": "overwrite_edit",
                "description": "Overwrite edit (Premiere '.'): drop source at `at` on track_id, replacing whatever it covers — trimming partially-covered clips, removing fully-covered ones, splitting a clip that spans the region — with NO ripple, as ONE undo step. Timeline duration is unchanged unless source extends past the old end. Same args shape as insert_edit. Returns the new clip's id.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "track_id": { "type": "string" },
                        "name": { "type": "string" },
                        "at_ticks": { "type": "integer" },
                        "at_tc": { "type": "string", "description": "HH:MM:SS:FF or HH:MM:SS;FF" },
                        "at_seconds": { "type": "number" },
                        "source": { "type": "object", "description": "ClipSource — {\"kind\":\"asset\",\"asset_id\":...} | {\"kind\":\"vector\",\"asset_id\":...} | {\"kind\":\"nested_sequence\",\"sequence_id\":...} | {\"kind\":\"solid_color\",\"color\":\"#rrggbb\"} | {\"kind\":\"adjustment\"}" },
                        "source_in_ticks": { "type": "integer" },
                        "source_in_tc": { "type": "string" },
                        "source_in_seconds": { "type": "number" },
                        "duration_ticks": { "type": "integer", "description": "Always exact ticks — no dual-unit ambiguity." }
                    },
                    "required": ["track_id","source","duration_ticks"]
                }
            },
            {
                "name": "lift_edit",
                "description": "Lift edit (Premiere ';'): remove clip content in `range` on track_id, leaving a gap (no ripple). Timeline duration is unchanged. ONE undo step; a no-op (no history entry) if nothing overlaps the range.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "track_id": { "type": "string" },
                        "range": { "type": "object", "description": "{\"start_ticks\"|\"start_tc\"|\"start_seconds\":...,\"end_ticks\"|\"end_tc\"|\"end_seconds\":...} — both bounds required, ticks > tc > seconds precedence per bound." }
                    },
                    "required": ["track_id","range"]
                }
            },
            {
                "name": "extract_edit",
                "description": "Extract edit (Premiere '\\''): remove clip content in `range` on track_id AND ripple everything after it left to close the gap (generalizes remove_clip's ripple flag to an arbitrary range). The track's content shrinks by the range width, as ONE undo step; a no-op (no history entry) if nothing overlaps the range.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "track_id": { "type": "string" },
                        "range": { "type": "object", "description": "{\"start_ticks\"|\"start_tc\"|\"start_seconds\":...,\"end_ticks\"|\"end_tc\"|\"end_seconds\":...} — both bounds required, ticks > tc > seconds precedence per bound." }
                    },
                    "required": ["track_id","range"]
                }
            },

            // NLE parity round-2 (17-nle-parity-round2.md, G21 CAP-019 MCP parity)
            {
                "name": "replace_clip_source",
                "description": "Replace With Clip (Premiere, G-5): swap a clip's source in place — start/duration/effects/transitions/grade untouched. A shorter new source is held to the slot (sampled from new_source_in for the slot's length by the engine). Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "new_source": { "type": "object", "description": "ClipSource — {\"kind\":\"asset\",\"asset_id\":...} | {\"kind\":\"vector\",\"asset_id\":...} | {\"kind\":\"nested_sequence\",\"sequence_id\":...} | {\"kind\":\"solid_color\",\"color\":\"#rrggbb\"} | {\"kind\":\"adjustment\"}" },
                        "new_source_in_ticks": { "type": "integer" },
                        "new_source_in_tc": { "type": "string" },
                        "new_source_in_seconds": { "type": "number", "description": "Offset into the new source to sample from. Omit to keep the clip's existing source_in. Precedence: ticks > tc > seconds." }
                    },
                    "required": ["clip_id","new_source"]
                }
            },
            {
                "name": "add_edit_all_tracks",
                "description": "Add Edit to All Tracks (Premiere Ctrl+Shift+K, G-1): split every unlocked track's clip that `at` sits strictly inside, across the whole sequence, as ONE undo step. Returns the number split and their new (right-hand) clip ids; a no-op (no history entry) if nothing is under `at`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string" },
                        "at_ticks": { "type": "integer" },
                        "at_tc": { "type": "string" },
                        "at_seconds": { "type": "number" }
                    },
                    "required": ["sequence_id"]
                }
            },
            {
                "name": "insert_space",
                "description": "Insert Space (26 §9 K-A3): open empty timeline of `amount` (default 1s) at `at` on every unlocked track — later clips shift right. Sequence markers at/after the point shift with the space. One undo step. Locked tracks are skipped.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string" },
                        "at_ticks": { "type": "integer", "description": "Sequence-relative insert point. Precedence: at_ticks > at_tc > at_seconds." },
                        "at_tc": { "type": "string" },
                        "at_seconds": { "type": "number" },
                        "amount_ticks": { "type": "integer", "description": "Gap width in ticks. Prefer over amount_seconds." },
                        "amount_seconds": { "type": "number", "description": "Gap width in seconds (default 1.0 if neither amount_* supplied)." }
                    },
                    "required": ["sequence_id"]
                }
            },
            {
                "name": "remove_space",
                "description": "Remove Space (26 §9 K-A3): close up to `amount` of pure gap at `at` across unlocked tracks (later clips shift left). Refuses when a clip covers `at` or the shared free gap is shorter than amount. One undo step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string" },
                        "at_ticks": { "type": "integer" },
                        "at_tc": { "type": "string" },
                        "at_seconds": { "type": "number" },
                        "amount_ticks": { "type": "integer" },
                        "amount_seconds": { "type": "number", "description": "Default 1.0s when amount_* omitted." }
                    },
                    "required": ["sequence_id"]
                }
            },
            {
                "name": "remove_all_spaces_after",
                "description": "Remove All Spaces After (26 §9 K-A3): pack every unlocked track from `at` onward so later clips are contiguous (no internal gaps). One undo step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string" },
                        "at_ticks": { "type": "integer" },
                        "at_tc": { "type": "string" },
                        "at_seconds": { "type": "number" }
                    },
                    "required": ["sequence_id"]
                }
            },
            {
                "name": "remove_clips_after",
                "description": "Remove All Clips After (26 §9 K-A3): delete every clip on every unlocked track whose start is at or after `at`. One undo step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string" },
                        "at_ticks": { "type": "integer" },
                        "at_tc": { "type": "string" },
                        "at_seconds": { "type": "number" }
                    },
                    "required": ["sequence_id"]
                }
            },
            {
                "name": "close_gap",
                "description": "Close Gap (G-1): close the gap containing `at` — on just track_id when supplied, or on every unlocked track in the sequence when omitted — as ONE undo step either way. A no-op (no history entry) when there is nothing to close.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string" },
                        "track_id": { "type": "string", "description": "Restrict to one track. Omit to close the gap at `at` on every unlocked track in the sequence." },
                        "at_ticks": { "type": "integer" },
                        "at_tc": { "type": "string" },
                        "at_seconds": { "type": "number" }
                    },
                    "required": ["sequence_id"]
                }
            },
            {
                "name": "match_frame",
                "description": "Match Frame (Premiere F, G-3): from clip_id, compute the source-media tick that lines up with timeline position `at` (which must fall within the clip's span). Read-only — no mutation, no undo step. Returns the matching source tick and the clip's asset id (null for generator/adjustment/text clips) so the caller can feed replace_clip_source/insert_edit/overwrite_edit.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "at_ticks": { "type": "integer" },
                        "at_tc": { "type": "string" },
                        "at_seconds": { "type": "number" }
                    },
                    "required": ["clip_id"]
                }
            },
            {
                "name": "insert_adjustment_clip",
                "description": "Adjustment-layer clip (G-7): create a no-media ClipSource::Adjustment clip spanning [start, start+duration) on track_id — its effect stack/grade composites over every lower track beneath its span (engine side). Returns the new clip's id. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "track_id": { "type": "string" },
                        "start_ticks": { "type": "integer" },
                        "start_tc": { "type": "string" },
                        "start_seconds": { "type": "number" },
                        "duration_ticks": { "type": "integer" }
                    },
                    "required": ["track_id","duration_ticks"]
                }
            },
            {
                "name": "insert_text_clip",
                "description": "Title/text clip (G-12): create a ClipSource::Text title/graphics clip spanning [start, start+duration) on track_id. `style` patches CaptionStyle::default() using the same partial-style vocabulary as set_caption_style (font_family/font_size/weight/fill/position/max_width) — omit for the default style. Returns the new clip's id. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "track_id": { "type": "string" },
                        "text": { "type": "string" },
                        "style": { "type": "object", "description": "Partial CaptionStyle patch: {\"font_family\":str,\"font_size\":number,\"weight\":int,\"fill\":\"#rrggbb\",\"position\":[x,y],\"max_width\":number} — every field optional." },
                        "start_ticks": { "type": "integer" },
                        "start_tc": { "type": "string" },
                        "start_seconds": { "type": "number" },
                        "duration_ticks": { "type": "integer" }
                    },
                    "required": ["track_id","text","duration_ticks"]
                }
            },

            // Clip properties (10 §3.5)
            {
                "name": "set_clip_prop",
                "description": "Universal clip-property setter — name, base transform (pos/scale/rotation/anchor/opacity), per-format reframe override, enabled, color_label — only supplied fields change. Speed and transitions have dedicated tools (set_clip_speed/set_transition). Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "name": { "type": "string" },
                        "transform": { "type": "object", "description": "Full base-transform replace: {\"x\":0,\"y\":0,\"scale_x\":1,\"scale_y\":1,\"rotation\":0,\"anchor_space\":\"center_offset\",\"anchor_x\":0,\"anchor_y\":0,\"opacity\":1}. anchor_space is optional (center_offset default) or absolute for legacy output-pixel pivots." },
                        "reframe": { "type": "object", "description": "{\"format_index\":N,\"transform\":{...}|null} — null clears the override for that format index." },
                        "enabled": { "type": "boolean" },
                        "color_label": { "type": ["integer","null"], "description": "Organizational swatch-palette index. null clears the label; omit the field entirely to leave it unchanged." }
                    },
                    "required": ["clip_id"]
                }
            },
            {
                "name": "set_clip_speed",
                "description": "Set a clip's playback speed — supply exactly one of `ratio` (SpeedMap::Constant, an exact rational) or `keys` (SpeedMap::Keyframed, a variable-speed ramp, G-11). Each key has clip-relative time, ratio, and optional outgoing interpolation: {kind:hold|linear|bezier,out_handle:[x,y],in_handle:[x,y]}. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "ratio": { "type": "object", "description": "Constant speed. {\"num\":int,\"den\":uint} — e.g. {\"num\":2,\"den\":1} for 2x, {\"num\":-1,\"den\":1} for reverse. Mutually exclusive with keys.", "properties": { "num": {"type":"integer"}, "den": {"type":"integer"} }, "required": ["num","den"] },
                        "keys": { "type": "array", "description": "Keyframed ramp control points, clip-relative. Mutually exclusive with ratio.", "items": { "type": "object", "properties": { "at_ticks": {"type":"integer"}, "at_tc": {"type":"string"}, "at_seconds": {"type":"number"}, "ratio": { "type": "object", "properties": { "num": {"type":"integer"}, "den": {"type":"integer"} }, "required": ["num","den"] }, "interp": { "type": "object", "description": "Optional outgoing interpolation: {kind:hold|linear|bezier,out_handle:[0..1,0..1],in_handle:[0..1,0..1]}. Defaults to hold." } }, "required": ["ratio"] } }
                    },
                    "required": ["clip_id"]
                }
            },
            // D-12 gyro stabilization (22 §6.5)
            {
                "name": "import_motion_metadata",
                "description": "Bind a gyro/IMU sidecar to a clip for D-12 stabilization. Accepts Gyroflow-compatible `.gcsv` or Photonic gyro JSON. The file is parsed immediately and rejected with a located error if malformed — an unknown axis convention or unit hard-fails rather than being guessed. Without `lens_profile_path` only rotation is corrected, not lens distortion. Call analyze_stabilization next. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "path": { "type": "string", "description": "Path to a .gcsv or Photonic gyro JSON sidecar." },
                        "lens_profile_path": { "type": "string", "description": "Optional lens-calibration JSON (camera_matrix + 4 Kannala-Brandt distortion coefficients). Omit for rotation-only correction." },
                        "smoothness": { "type": "number", "description": "0..1. 0 follows the original camera motion exactly; 1 is maximally smooth and demands the most crop. Default 0.5." },
                        "horizon_lock": { "type": "number", "description": "0..1 gravity-referenced levelling. Requires accelerometer data in the motion source; has no effect without it. Default 0." }
                    },
                    "required": ["clip_id","path"]
                }
            },
            {
                "name": "set_stabilization",
                "description": "Update an existing D-12 stabilization recipe; only supplied fields change. Any change clears the prior analysis, since the corrections were computed for the old settings — re-run analyze_stabilization afterwards. Out-of-range values are rejected, not clamped. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "smoothness": { "type": "number", "description": "0..1." },
                        "horizon_lock": { "type": "number", "description": "0..1. Needs accelerometer data." },
                        "max_zoom": { "type": "number", "description": ">= 1.0. Ceiling on how far the crop solver may zoom to hide edges the correction swings out of frame." },
                        "crop_mode": { "type": "string", "enum": ["static_safe","dynamic","transparent_edges"], "description": "static_safe: one fixed zoom sized for the worst frame. dynamic: tracks the requirement, smoothed. transparent_edges: no zoom, uncovered pixels left transparent." }
                    },
                    "required": ["clip_id"]
                }
            },
            {
                "name": "analyze_stabilization",
                "description": "Run D-12 stabilization analysis for a clip: estimate gyro bias, integrate the camera orientation, smooth it, apply horizon lock, and solve the crop path. Returns diagnostics including estimated bias, sample rate, clock drift, maximum required zoom, and any frame range that cannot be covered within max_zoom. Does not modify the document and creates no undo step — analysis is generation, not history.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "clip_id": { "type": "string" } },
                    "required": ["clip_id"]
                }
            },
            {
                "name": "get_stabilization_status",
                "description": "Report a clip's D-12 stabilization state — motion source, lens profile, strength settings, crop mode, sync-anchor count, and whether an analysis exists — without running anything. Location-bearing detail is redacted: only file names are returned, never full paths.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "clip_id": { "type": "string" } },
                    "required": ["clip_id"]
                }
            },
            {
                "name": "set_transition",
                "description": "Add, replace, or remove (transition=null) a clip's in or out transition. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "edge": { "type": "string", "enum": ["in","out"] },
                        "transition": { "type": ["object","null"], "description": "{\"kind\":\"cross_dissolve\"|\"dip_to_black\"|\"dip_to_color\"|\"wipe\"|\"push\",\"duration_ticks\":int,\"params\":{\"curve\":\"linear\"|\"ease_in\"|\"ease_out\"|\"ease_in_out\",\"color\":\"#rrggbb\"?,\"direction\":\"left\"|\"right\"|\"up\"|\"down\",\"softness\":number}}" }
                    },
                    "required": ["clip_id","edge"]
                }
            },

            // Clip organization: linking (14 §M-2, CAP-019 MCP parity)
            {
                "name": "link_clips",
                "description": "Link two clips (e.g. a split A/V pair) into the same link group so a future move can carry them together. Reuses whichever clip's group already exists, or mints a fresh one. Both clips must be in the same sequence. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id_a": { "type": "string" },
                        "clip_id_b": { "type": "string" }
                    },
                    "required": ["clip_id_a","clip_id_b"]
                }
            },
            {
                "name": "unlink_clips",
                "description": "Remove a clip from its link group — a no-op if it wasn't linked. Only the named clip leaves the group; its former partners stay linked to each other. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "clip_id": { "type": "string" } },
                    "required": ["clip_id"]
                }
            },
            {
                "name": "list_clips",
                "description": "List clips, optionally filtered by track, sequence, and/or a [range_start_ticks, range_end_ticks) time range.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string" },
                        "track_id": { "type": "string" },
                        "range_start_ticks": { "type": "integer" },
                        "range_end_ticks": { "type": "integer" }
                    }
                }
            },
            {
                "name": "get_clip",
                "description": "Full clip dump — timing, source, transform (incl. keyframe tracks), reframe overrides, effects, grade, composition ref, transitions, audio, speed.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "clip_id": { "type": "string" } },
                    "required": ["clip_id"]
                }
            },

            // Effects (10 §3.6)
            {
                "name": "add_effect",
                "description": "Push (or insert at `index`) an effect onto a clip's effect stack, seeded with the kind's registry default params. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "kind": { "type": "string", "enum": effect_kind_enum },
                        "index": { "type": "integer" }
                    },
                    "required": ["clip_id","kind"]
                }
            },
            {
                "name": "remove_effect",
                "description": "Remove one effect from a clip's stack by index. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "clip_id": { "type": "string" }, "effect_index": { "type": "integer" } },
                    "required": ["clip_id","effect_index"]
                }
            },
            {
                "name": "reorder_effects",
                "description": "Reorder a clip's effect stack — new_order is a permutation of 0..effects.len(). Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "clip_id": { "type": "string" }, "new_order": { "type": "array", "items": { "type": "integer" } } },
                    "required": ["clip_id","new_order"]
                }
            },
            {
                "name": "set_effect_param",
                "description": "Set one static (non-keyframed) param under effects[effect_index].params — path is a registry PropPath (e.g. \"params.radius\"), or the literal path \"enabled\" to toggle the effect itself. Use set_keyframe for animated params. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "effect_index": { "type": "integer" },
                        "path": { "type": "string" },
                        "value": { "type": "object", "description": "PropValue — {\"t\":\"float\",\"v\":number} | {\"t\":\"vec2\",\"v\":[number,number]} | {\"t\":\"color\",\"v\":{\"r\":n,\"g\":n,\"b\":n,\"a\":n}} | {\"t\":\"bool\",\"v\":boolean} | {\"t\":\"enum\",\"v\":integer}" }
                    },
                    "required": ["clip_id","effect_index","path","value"]
                }
            },
            {
                "name": "set_effect_zone",
                "description": "Set or clear an effect zone (26 §10 K-B3): half-open clip-relative [start_ticks, end_ticks). Outside the zone the effect is folded out of the graph at that tick (same as disabled). Pass clear=true to remove the zone. One undo step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "effect_index": { "type": "integer" },
                        "clear": { "type": "boolean", "description": "When true, remove the zone (whole-clip effect)." },
                        "start_ticks": { "type": "integer", "description": "Clip-relative zone start (inclusive)." },
                        "end_ticks": { "type": "integer", "description": "Clip-relative zone end (exclusive)." }
                    },
                    "required": ["clip_id","effect_index"]
                }
            },
            {
                "name": "effect_stack",
                "description": "Edit any of the four video effect stacks (26 §10 K-B1/K-B2): a timeline `clip`, a whole `track`, the sequence `master`, or a bin `asset` (inherited by every instance of that media). Evaluation order is asset -> clip -> track -> master. `op=list` is read-only; `add`/`remove`/`reorder`/`set_param`/`set_grade` are each one undo step. The clip-only add_effect/remove_effect/reorder_effects/set_effect_param tools remain as shorthand for scope=clip.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "scope": { "type": "string", "enum": ["clip","track","master","asset"] },
                        "op": { "type": "string", "enum": ["list","add","remove","reorder","set_param","set_grade"] },
                        "clip_id": { "type": "string", "description": "Required for scope=clip." },
                        "track_id": { "type": "string", "description": "Required for scope=track." },
                        "sequence_id": { "type": "string", "description": "scope=master; defaults to the active sequence." },
                        "asset_id": { "type": "string", "description": "Required for scope=asset." },
                        "effect_id": { "type": "string", "description": "op=add: stable manifest id (see list_effect_kinds), e.g. \"blur.gaussian\"." },
                        "kind": { "type": "string", "enum": effect_kind_enum, "description": "op=add: legacy EffectKind tag, used only when effect_id is absent." },
                        "index": { "type": "integer", "description": "op=add insert position (default: append); op=remove / op=set_param target index." },
                        "new_order": { "type": "array", "items": { "type": "integer" }, "description": "op=reorder: a permutation of 0..len." },
                        "path": { "type": "string", "description": "op=set_param: a registry PropPath (e.g. \"params.radius\"), or the literal \"enabled\"." },
                        "value": { "type": "object", "description": "op=set_param: PropValue - {\"t\":\"float\",\"v\":number} | {\"t\":\"vec2\",\"v\":[number,number]} | {\"t\":\"color\",\"v\":{\"r\":n,\"g\":n,\"b\":n,\"a\":n}} | {\"t\":\"bool\",\"v\":boolean} | {\"t\":\"enum\",\"v\":integer}" },
                        "grade": { "type": "object", "description": "op=set_grade: a Grade object (07 §1), or null to clear." }
                    },
                    "required": ["scope","op"]
                }
            },
            {
                "name": "paste_attributes",
                "description": "Paste Attributes (26 §10 K-B15): copy one clip's LOOK onto other, already-existing clips — effect stack, grade, transform (pos/scale/rotation/anchor/opacity + its keyframes) and clip audio (gain/fades/channel map). Timing is never touched: start, duration, source, source_in, speed, transitions, composition and per-format reframe all stay as they are, so this is NOT a clip copy (use insert_clip to lay down a new clip). Pasting onto N clips is ONE undo step. Targets that already match are skipped and reported in `skipped`. Refused if the grade carries a Lut3d whose asset is not in this project, or if any target clip id is unknown (no partial paste).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "source_clip_id": { "type": "string", "description": "The clip whose look is copied." },
                        "target_clip_ids": { "type": "array", "items": { "type": "string" }, "description": "Clips to stamp it onto; must be non-empty. May span tracks and sequences." },
                        "attributes": {
                            "type": "array",
                            "items": { "type": "string", "enum": ["effects","grade","transform","audio"] },
                            "description": "Families to transfer. Omit for all four; an empty array is refused. An unlisted family leaves the target's own value untouched (it is not reset)."
                        }
                    },
                    "required": ["source_clip_id","target_clip_ids"]
                }
            },
            {
                "name": "freeze_frame",
                "description": "Freeze Frame (26 §10 K-B14): hold the source frame visible at clip-relative `at_*` for the clip's whole timeline duration. Writes `source_in` to that source tick and sets speed to constant zero — not a new effect kind; the existing zero-rate SpeedMap path is the model. One undo step. Already-frozen at the same frame is a no-op (no history entry). Mid-clip freeze-then-resume is expressible via set_clip_speed with a zero-rate keyframed segment; this tool freezes the whole slot. Generators (solid/text/adjustment) accept the same verb (their source is time-invariant).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string", "description": "Clip to freeze." },
                        "at_ticks": { "type": "integer", "description": "Clip-relative tick of the frame to hold. Precedence: at_ticks > at_tc > at_seconds. Omit for the first frame." },
                        "at_tc": { "type": "string", "description": "Clip-relative timecode of the frame to hold." },
                        "at_seconds": { "type": "number", "description": "Clip-relative seconds of the frame to hold." }
                    },
                    "required": ["clip_id"]
                }
            },
            {
                "name": "list_effect_kinds",
                "description": "Registry introspection sourced from the effect manifest catalogue (spec 30 §2.7). Returns `effect_kinds`: one entry per manifest with `id`, `version`, `name`, `category`, `arity`, the legacy `kind` tag, and a `params` table (each `{path, kind, default, range, animatable, ui, group, display}`) — lets an agent discover effects and their param ranges without guessing.",
                "inputSchema": { "type": "object", "properties": {} }
            },

            // Effect presets, custom stacks and favourites (26 §10 K-B4). The
            // library is USER state in <config>/Photonic/effect_presets.json, not
            // document state: only effect_preset_apply edits the document, and it
            // is exactly one undo step.
            {
                "name": "effect_preset_list",
                "description": "List effect presets (26 §10 K-B4): the built-in catalogue first, then the user's own saved stacks from <config>/Photonic/effect_presets.json. Each entry carries `name`, `built_in`, `effect_ids`, `effect_count`, `has_grade`, `parameter_preset_for` (set when the preset is a single-effect parameter preset), a one-line `summary`, and `unresolvable_effect_ids` — ids this build has no manifest for, which still apply but land inert (39 §2.2), never dropped. Read-only; presets are app config, so nothing here appears in undo history.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "effect_preset_save",
                "description": "Save one scope's current effect stack AND its grade as a named preset (26 §10 K-B4). Addresses the scope exactly as effect_stack does (scope + clip_id/track_id/sequence_id/asset_id). This writes app config, NOT the document: there is no document mutation and no undo step. A built-in name is refused (NotSupportedV1); an existing user name is overwritten in place, keeping its position in the user's ordering. A scope with no effects and no grade is refused — there would be nothing to apply.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Preset name. Built-in names are read-only." },
                        "scope": { "type": "string", "enum": ["clip","track","master","asset"] },
                        "clip_id": { "type": "string", "description": "Required for scope=clip." },
                        "track_id": { "type": "string", "description": "Required for scope=track." },
                        "sequence_id": { "type": "string", "description": "scope=master; defaults to the active sequence." },
                        "asset_id": { "type": "string", "description": "Required for scope=asset." }
                    },
                    "required": ["name","scope"]
                }
            },
            {
                "name": "effect_preset_apply",
                "description": "Apply a preset (built-in or user) to a scope (26 §10 K-B4). Effects are APPENDED to the existing stack in the preset's own order; a preset's grade REPLACES the scope's grade, and a preset with no grade leaves it alone. Use paste_attributes for wholesale replacement. Exactly ONE undo step regardless of how many effects the preset holds or how many clips it lands on — scope=clip accepts clip_ids for a multi-clip apply, and an unknown id refuses the whole call rather than half-applying.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Resolved across built-ins first, then user presets (see effect_preset_list)." },
                        "scope": { "type": "string", "enum": ["clip","track","master","asset"] },
                        "clip_id": { "type": "string", "description": "scope=clip, single target." },
                        "clip_ids": { "type": "array", "items": { "type": "string" }, "description": "scope=clip, many targets — applied as ONE batch. Given instead of clip_id." },
                        "track_id": { "type": "string", "description": "Required for scope=track." },
                        "sequence_id": { "type": "string", "description": "scope=master; defaults to the active sequence." },
                        "asset_id": { "type": "string", "description": "Required for scope=asset." }
                    },
                    "required": ["name","scope"]
                }
            },
            {
                "name": "effect_preset_delete",
                "description": "Delete a user effect preset (app-level config — no document mutation, no undo step). Built-ins are read-only and refuse deletion with NotSupportedV1.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "name": { "type": "string" } },
                    "required": ["name"]
                }
            },
            {
                "name": "effect_preset_rename",
                "description": "Rename a user effect preset, keeping its position in the user's ordering (app-level config — no document mutation, no undo step). Refused with NotSupportedV1 if either `from` or `to` names a built-in; renaming onto an existing user preset replaces it.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "from": { "type": "string" }, "to": { "type": "string" } },
                    "required": ["from","to"]
                }
            },
            {
                "name": "effect_favourite_list",
                "description": "List the user's favourited effect ids in their own order (26 §10 K-B4) — an ordering over the manifest catalogue, not a copy of it. Each entry carries `id`, the manifest `name`, and `available`: false means this build has no manifest for that id, so it is kept untouched (39 §2.2) and simply not offered. Read-only.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "effect_favourite_set",
                "description": "Star or unstar one effect id (app-level config — no document mutation, no undo step). Idempotent: setting the state it already has succeeds and rewrites nothing. Starring an id this build has no manifest for is refused as a typo (see list_effect_kinds); UNstarring one always works, so a library carried from a build with more effects can be pruned.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Stable effect id from list_effect_kinds, e.g. \"blur.gaussian\"." },
                        "favourite": { "type": "boolean" }
                    },
                    "required": ["id","favourite"]
                }
            },

            // Keyframes (10 §3.7)
            {
                "name": "set_keyframe",
                "description": "Upsert one keyframe on a PropertyTrack (creates the track if absent), creating animation. target=clip_transform needs only clip_id; target=clip_effect also needs effect_index. P2 scope: clip transform + clip-effect params only (grade/audio/graph-node targets land with their domains). Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target": { "type": "string", "enum": ["clip_transform","clip_effect"] },
                        "clip_id": { "type": "string" },
                        "effect_index": { "type": "integer", "description": "Required when target=clip_effect." },
                        "path": { "type": "string", "description": "Registry PropPath, e.g. \"transform.x\" or \"params.radius\"." },
                        "at_ticks": { "type": "integer" },
                        "at_tc": { "type": "string" },
                        "at_seconds": { "type": "number" },
                        "value": { "type": "object", "description": "PropValue — {\"t\":\"float\",\"v\":number} | {\"t\":\"vec2\",\"v\":[number,number]} | {\"t\":\"color\",\"v\":{...}} | {\"t\":\"bool\",\"v\":boolean} | {\"t\":\"enum\",\"v\":integer}" },
                        "interp": { "type": "object", "description": "{\"kind\":\"hold\"} | {\"kind\":\"linear\"} | {\"kind\":\"bezier\",\"out_handle\":[x,y],\"in_handle\":[x,y]}" }
                    },
                    "required": ["target","clip_id","path","value","interp"]
                }
            },
            {
                "name": "remove_keyframe",
                "description": "Remove the keyframe at exactly `at_*` on a target's PropertyTrack. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target": { "type": "string", "enum": ["clip_transform","clip_effect"] },
                        "clip_id": { "type": "string" },
                        "effect_index": { "type": "integer" },
                        "path": { "type": "string" },
                        "at_ticks": { "type": "integer" },
                        "at_tc": { "type": "string" },
                        "at_seconds": { "type": "number" }
                    },
                    "required": ["target","clip_id","path"]
                }
            },
            {
                "name": "batch_set_keyframes",
                "description": "Set N keyframes (same or different targets/paths) as ONE undo step. Each entry has the same shape as set_keyframe's args.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "ops": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "target": { "type": "string", "enum": ["clip_transform","clip_effect"] },
                                    "clip_id": { "type": "string" },
                                    "effect_index": { "type": "integer" },
                                    "path": { "type": "string" },
                                    "at_ticks": { "type": "integer" },
                                    "at_tc": { "type": "string" },
                                    "at_seconds": { "type": "number" },
                                    "value": { "type": "object" },
                                    "interp": { "type": "object" }
                                },
                                "required": ["target","clip_id","path","value","interp"]
                            }
                        }
                    },
                    "required": ["ops"]
                }
            },
            {
                "name": "get_keyframes",
                "description": "Full PropertyTrack list (all keyframes) for a target.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target": { "type": "string", "enum": ["clip_transform","clip_effect"] },
                        "clip_id": { "type": "string" },
                        "effect_index": { "type": "integer", "description": "Required when target=clip_effect." }
                    },
                    "required": ["target","clip_id"]
                }
            },
            {
                "name": "copy_keyframes",
                "description": "K-B11: snapshot keyframe tracks from a target into a serializable clipboard payload (tracks + anchor). Optional `paths` filters which PropPaths to copy. Read-only — returns `{ clipboard }` for paste_keyframes.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target": { "type": "string", "enum": ["clip_transform","clip_effect"] },
                        "clip_id": { "type": "string" },
                        "effect_index": { "type": "integer" },
                        "paths": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Optional PropPath filter; omit to copy every non-empty track."
                        }
                    },
                    "required": ["target","clip_id"]
                }
            },
            {
                "name": "paste_keyframes",
                "description": "K-B11: paste a keyframe clipboard onto a target as ONE undo batch. `mapping` remaps source→dest paths (unmapped keep identity). `offset_ticks` adds to every keyframe time, or set `reanchor_ticks` so the clipboard anchor lands at that clip-relative tick.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target": { "type": "string", "enum": ["clip_transform","clip_effect"] },
                        "clip_id": { "type": "string" },
                        "effect_index": { "type": "integer" },
                        "clipboard": {
                            "type": "object",
                            "description": "KeyframeClipboard from copy_keyframes (tracks + anchor)."
                        },
                        "mapping": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "from": { "type": "string" },
                                    "to": { "type": "string" }
                                },
                                "required": ["from","to"]
                            }
                        },
                        "offset_ticks": { "type": "integer", "description": "Added to every keyframe at; ignored if reanchor_ticks is set." },
                        "reanchor_ticks": { "type": "integer", "description": "Land clipboard.anchor at this clip-relative tick." }
                    },
                    "required": ["target","clip_id","clipboard"]
                }
            },

            // Media (P2 subset — import/relink/list/remove; probe/proxy/transcode are P3 engine jobs)
            {
                "name": "import_media",
                "description": "Register one or more files as MediaAsset(s) in the media pool. probe is always null at this phase (ffprobe integration is P3) — the result flags each asset probed:false. Content-hashes each file now (head+tail+len digest) as a relink identity. `bin` names a bin to file the imported asset(s) under, looked up by exact name and created (top-level) if it doesn't exist. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "paths": { "type": "array", "items": { "type": "string" } },
                        "bin": { "type": "string", "description": "Bin name to file the imported asset(s) under; created if it doesn't exist." }
                    },
                    "required": ["paths"]
                }
            },
            {
                "name": "relink_media",
                "description": "Repoint an offline (or any) asset to a new file path. The file must exist (AssetOffline otherwise). If the asset carries a content_hash and the new file's hash differs, the call is refused with HashMismatch unless allow_hash_mismatch is true — a relink to the wrong take is invisible until export. Accepting a byte change records the new hash and clears the stale probe in the same undo step (re-run probe_media). Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "asset_id": { "type": "string" },
                        "new_path": { "type": "string" },
                        "allow_hash_mismatch": { "type": "boolean", "description": "Bind the asset to a file whose bytes differ from its recorded content_hash. Default false." }
                    },
                    "required": ["asset_id","new_path"]
                }
            },
            {
                "name": "find_offline_media",
                "description": "List every media-pool asset whose file is not reachable, with its recorded content_hash and the number of timeline clips that would go offline with it. The inventory a relink starts from (26 K-C6).",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "relink_media_batch",
                "description": "Relink offline media in bulk by scanning a folder — the moved-project case (26 K-C6). Each asset is matched strongest-identity-first: content_hash (survives a rename), then exact filename, then case-insensitive filename; ties resolve to the lexicographically smallest path and are flagged ambiguous. Every relink is verified against the asset's content_hash and an entry whose bytes differ is reported under skipped_hash_mismatch and NOT committed unless allow_hash_mismatch is true (accepting one records the new hash and clears the stale probe). dry_run returns the same plan without touching the document. The whole batch commits as ONE undo step. The scan is depth- and count-capped; scan_truncated reports when it hit a cap, hashed_scan reports whether by-hash discovery was possible.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "search_dir": { "type": "string", "description": "Folder to scan for replacement files." },
                        "asset_ids": { "type": "array", "items": { "type": "string" }, "description": "Restrict to these assets; default is every offline asset." },
                        "recursive": { "type": "boolean", "description": "Walk subdirectories. Default true." },
                        "dry_run": { "type": "boolean", "description": "Report the plan and change nothing. Default false." },
                        "allow_hash_mismatch": { "type": "boolean", "description": "Also commit entries whose bytes differ from the recorded content_hash. Default false." }
                    },
                    "required": ["search_dir"]
                }
            },
            {
                "name": "list_media",
                "description": "List media-pool assets with probe/proxy status, content hash, and bin. `bin` filters to assets filed under the bin with that exact name.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "bin": { "type": "string" } }
                }
            },
            {
                "name": "remove_asset",
                "description": "Remove an asset from the media pool. Not in the original 10-mcp-tools.md §3.1 catalog table — added to the P2 scope explicitly (ops::remove_asset already existed). Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "asset_id": { "type": "string" } },
                    "required": ["asset_id"]
                }
            },

            // Media bins (not in the original §3.1 catalog table — added when
            // MediaAsset.bin support landed in core; standard create_/remove_/
            // set_/list_ tools rather than an op-field mega-tool since each maps
            // 1:1 to a distinct TimelineCmd variant, per design rules 1/2)
                    {
                "name": "set_asset_tags",
                "description": "K-C2: replace free-form tags on a media asset, upserting each name into the project TagId registry and assigning MediaAsset.tag_ids. Empty tags clears. ONE undo batch.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "asset_id": { "type": "string" },
                        "tags": { "type": "array", "items": { "type": "string" }, "description": "Replacement tag names; empty clears." }
                    },
                    "required": ["asset_id"]
                }
            },
            {
                "name": "create_subclip",
                "description": "Create Subclip (26 §9 K-A8): add a zone-bounded pool entry that is a view of parent_asset_id over [in, out). Shares content_hash/proxy/source with the parent (no cache duplication). One undo step (AddAsset). Nested subclips are refused.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "parent_asset_id": { "type": "string" },
                        "in_ticks": { "type": "integer", "description": "Source-range start on the parent." },
                        "out_ticks": { "type": "integer", "description": "Source-range end (exclusive) on the parent." },
                        "in_seconds": { "type": "number" },
                        "out_seconds": { "type": "number" },
                        "name": { "type": "string", "description": "Optional label stored as a subclip: tag." }
                    },
                    "required": ["parent_asset_id"]
                }
            },
    {
                "name": "create_bin",
                "description": "Create a media bin (folder), optionally nested under `parent`. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "name": { "type": "string" }, "parent": { "type": "string" } },
                    "required": ["name"]
                }
            },
            {
                "name": "remove_bin",
                "description": "Remove a media bin. Assets/child bins referencing it are left untouched (a dangling ref reads as unfiled); re-adding restores the bin verbatim on undo. Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "bin_id": { "type": "string" } },
                    "required": ["bin_id"]
                }
            },
            {
                "name": "set_asset_bin",
                "description": "Move an asset into a bin (or to the pool root, by omitting/nulling bin_id). Supports undo.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "asset_id": { "type": "string" }, "bin_id": { "type": ["string","null"] } },
                    "required": ["asset_id"]
                }
            },
            {
                "name": "list_bins",
                "description": "List every media bin (folder) in the pool.",
                "inputSchema": { "type": "object", "properties": {} }
            },

            // ── Video domain, P3 engine slice (10-mcp-tools.md §3.13/§3.14/§3.15,
            // §3.1 engine-backed media ops, §6 async jobs). Playback tools mutate
            // engine/session state only — no undo step, no document change. All
            // engine-backed tools require a GPU adapter; on a machine without one
            // they fail with error_code "EngineUnavailable".
            {
                "name": "play",
                "description": "Start playback on the video engine (EngineCmd::Play). Session state only — no undo step. Audio opens lazily; on machines with no audio device playback proceeds on a soft clock instead of failing. Returns the engine status snapshot.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string", "description": "Sequence to activate before playing. Omit to keep the engine's current active sequence." }
                    }
                }
            },
            {
                "name": "pause",
                "description": "Pause playback (EngineCmd::Pause). Session state only — no undo step. Returns the engine status snapshot including the paused playhead tick.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "seek",
                "description": "Move the engine playhead (EngineCmd::Seek). Session state only — no undo step. Time precedence: at_ticks > at_tc > at_seconds. The engine presents the exact frame-start tick at/before the target.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string" },
                        "at_ticks": { "type": "integer", "description": "Exact tick (highest precedence)." },
                        "at_tc": { "type": "string", "description": "HH:MM:SS:FF or HH:MM:SS;FF, resolved against the sequence frame rate." },
                        "at_seconds": { "type": "number", "description": "Convenience; sub-tick rounding possible, not authoritative." }
                    },
                    "required": ["sequence_id"]
                }
            },
            {
                "name": "step",
                "description": "Single-frame step (CAP-004): pauses, then snaps the playhead exactly ±frames frame-starts and evaluates that tick. Session state only — no undo step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "frames": { "type": "integer", "description": "Signed frame count: +1 next frame, -1 previous." }
                    },
                    "required": ["frames"]
                }
            },
            {
                "name": "set_loop_range",
                "description": "Set or clear the playback loop range (EngineCmd::SetLoop). Session state only — no undo step. Omit/null `range` to clear. Each bound follows the ticks > tc > seconds precedence.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string" },
                        "range": {
                            "type": ["object","null"],
                            "description": "{start_ticks|start_tc|start_seconds, end_ticks|end_tc|end_seconds}; null clears the loop."
                        }
                    },
                    "required": ["sequence_id"]
                }
            },
            {
                "name": "set_proxy_mode",
                "description": "Set the session proxy policy (02 §6): auto | force_proxy | force_original. Session state, not document state — no undo step. NOTE: proxy generation has not landed; all modes currently decode originals (the flag still selects preview/full compile quality for cache-hash purposes).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "mode": { "type": "string", "enum": ["auto","force_proxy","force_original"] }
                    },
                    "required": ["mode"]
                }
            },
            {
                "name": "get_engine_status",
                "description": "Engine status snapshot (EngineStatus, 02 §1): playhead tick, playing flag, dropped-frame count, node-cache stats, audio xruns, snapshot doc revision, active sequence, and the most recent engine command error.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string", "description": "Informational — the engine session is a per-process singleton (10 §2)." }
                    }
                }
            },
            {
                "name": "render_frame_at",
                "description": "Compile + evaluate the frame graph at one tick, headlessly, and return the image (10 §4 — the visual-feedback-loop tool). output_format `png` (default) is 8-bit sRGB for display; `raw_rgba16f` returns base64 linear premultiplied f16 pixels — byte-deterministic, the golden-frame comparison basis. COST WARNINGS: quality \"full\" on an uncached 4K composite can far exceed the preview eval budget — default to quality \"preview\" for iterative loops and \"full\" only for final verification frames; cold seeks pay a DECODE cost (up to ~150 ms per uncached GOP, more for originals) before any GPU work, so a loop scrubbing far-apart ticks is decode-dominated; repeated calls at nearby ticks mostly hit the node-result cache. Each call is independent (no held playback state).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string" },
                        "at_ticks": { "type": "integer", "description": "Exact tick (highest precedence); snapped down to the frame start." },
                        "at_tc": { "type": "string", "description": "HH:MM:SS:FF or HH:MM:SS;FF." },
                        "at_seconds": { "type": "number", "description": "Convenience; sub-tick rounding possible." },
                        "format_index": { "type": "integer", "description": "Which SequenceFormat (aspect variant). Default: the active format. Applied per-call only — the document's active format is untouched." },
                        "quality": { "type": "string", "enum": ["preview","full"], "description": "preview = proxy-eligible sources and Draft processing (960px long edge); full = originals processed at full sequence resolution." },
                        "scale": { "type": "number", "exclusiveMinimum": 0, "maximum": 1, "description": "Output scale. PNG thumbnails shrink on the GPU before readback; raw pixels use deterministic CPU box downscale." },
                        "output_format": { "type": "string", "enum": ["png","raw_rgba16f"], "description": "Default png." }
                    },
                    "required": ["sequence_id","quality"]
                }
            },
            {
                "name": "probe_media",
                "description": "Force (re-)probe an asset with ffprobe (async job — poll get_job_status). On completion refreshes MediaAsset.probe (duration/streams/colorimetry) and the xxh3 content_hash used for relinking. The probe field is engine-derived cache, not an undoable edit.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "asset_id": { "type": "string" } },
                    "required": ["asset_id"]
                }
            },
            {
                "name": "generate_proxies",
                "description": "Batch-generate editing proxies (02 §6; async job — poll get_job_status, cancellable). Transcodes each file-backed video asset to a half-res, all-intra H.264/MP4 proxy via the ffmpeg sidecar, stored in the sidecar cache dir keyed by content hash and attached to MediaAsset.proxy (status pending→ready). ForceProxy then decodes the proxy where present. Reuses a cached proxy unless force=true. Proxies are never required for correctness (CAP-014).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "asset_ids": { "type": "array", "items": { "type": "string" } },
                        "force": { "type": "boolean" }
                    },
                    "required": ["asset_ids"]
                }
            },
            {
                "name": "remove_proxy",
                "description": "Detach the proxy from each asset. Generated (cache-owned) proxy files are deleted; Attached user-owned proxy files are never deleted (G-15A). Assets then decode originals regardless of ProxyMode until regenerated (see generate_proxies) or re-attached.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "asset_ids": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["asset_ids"]
                }
            },
            {
                "name": "attach_proxy",
                "description": "Attach an existing user-owned proxy file to a file-backed video asset without re-encoding (G-15A). Validates path, video stream, duration (within one source frame) and nominal frame rate. Set allow_mismatch=true to accept mismatches as warnings. Never copies the file. Export still uses originals (CAP-014).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "asset_id": { "type": "string" },
                        "path": { "type": "string", "description": "Absolute path to the proxy media file." },
                        "allow_mismatch": { "type": "boolean", "description": "If true, duration/frame-rate mismatches become warnings instead of errors." }
                    },
                    "required": ["asset_id", "path"]
                }
            },
            {
                "name": "detach_proxy",
                "description": "Clear an asset's proxy reference without deleting the file on disk (G-15A). Safe for Attached user-owned proxies; for Generated cache files prefer remove_proxy if cache cleanup is desired.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "asset_id": { "type": "string" }
                    },
                    "required": ["asset_id"]
                }
            },
            {
                "name": "transcode_media",
                "description": "Transcode an asset to an editing-friendly intermediate via ffmpeg (async job — poll get_job_status; cancellable). Distinct from proxies: a user-picked codec, not the fixed proxy profile. The output file is NOT auto-imported; call import_media on the returned output_path if wanted.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "asset_id": { "type": "string" },
                        "preset": { "type": "string", "enum": ["prores_proxy","prores_lt","dnxhr_lb","h264_high"] },
                        "out_path": { "type": "string", "description": "Default: <source stem>.<preset>.<mov|mp4> next to the source." }
                    },
                    "required": ["asset_id","preset"]
                }
            },
            {
                "name": "export_sequence",
                "description": "Start an export job (02 §7; async — poll get_job_status, cancellable between frames). Renders the timeline as snapshotted at call time through a dedicated engine session and encodes via the ffmpeg sidecar. Range defaults to the sequence work range, else [0, content end). When the preset has an audio slot, sequence audio is mixed offline and muxed (K-0.7), with optional LoudnessTarget gain. Upscaling beyond the format size returns NotSupportedV1.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "expected_revision": { "type": "integer", "minimum": 0, "description": "Optional revision precondition from get_timeline_snapshot." },
                        "sequence_id": { "type": "string" },
                        "out_path": { "type": "string", "description": "Destination file path. Extension should match the preset container." },
                        "preset": { "type": "string", "description": "Preset name (see list_export_presets). Default \"Web H.264\"." },
                        "format_index": { "type": "integer", "description": "Which SequenceFormat to export. Default: the active format." },
                        "range": { "type": "object", "properties": {"start_ticks":{"type":"integer","minimum":0},"end_ticks":{"type":"integer","minimum":1},"start_tc":{"type":"string"},"end_tc":{"type":"string"},"start_seconds":{"type":"number","minimum":0},"end_seconds":{"type":"number","exclusiveMinimum":0}},"allOf":[{"anyOf":[{"required":["start_ticks"]},{"required":["start_tc"]},{"required":["start_seconds"]}]},{"anyOf":[{"required":["end_ticks"]},{"required":["end_tc"]},{"required":["end_seconds"]}]}], "description": "Half-open range; ticks > tc > seconds precedence per bound." },
                        "overrides": { "type": "object", "properties":{"width":{"type":"integer","minimum":1},"height":{"type":"integer","minimum":1},"frame_rate":{"type":"object","properties":{"num":{"type":"integer","minimum":1},"den":{"type":"integer","minimum":1}},"required":["num","den"]}},"dependentRequired":{"width":["height"],"height":["width"]},"description": "Width/height together; nearest-source-frame retiming for an explicit frame rate." }
                    },
                    "required": ["sequence_id","out_path"]
                }
            },
            {
                "name": "get_job_status",
                "description": "Poll an async job (export/probe/transcode — one registry, 10 §6). States: queued | running {progress, message} | done {result} | failed {error_code, message} | cancelled. Terminal jobs are retained 10 minutes, then evicted (JobNotFound).",
                "inputSchema": {
                    "type": "object",
                    "properties": { "job_id": { "type": "string" } },
                    "required": ["job_id"]
                }
            },
            {
                "name": "cancel_job",
                "description": "Request cancellation of an async job. Cooperative: the worker stops at its next check point (between frames for exports; partial output files are removed for transcodes). Cancelling an already-finished job is a no-op, not an error.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "job_id": { "type": "string" } },
                    "required": ["job_id"]
                }
            },
            {
                "name": "list_export_presets",
                "description": "List export presets: the built-in catalog (05 §3.5, read-only) plus user-saved custom presets. Each entry is the full ExportPreset serde shape plus a built_in flag — use it as the template for save_export_preset.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "save_export_preset",
                "description": "Create or overwrite a custom export preset (app-level config, 05 §3.6 — no document mutation, no undo step). Built-in preset names are refused (NotSupportedV1). The preset object is validated (alpha allow-list etc.) before persisting.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "preset": { "type": "object", "description": "Full ExportPreset object in its serde shape — copy one from list_export_presets and edit." }
                    },
                    "required": ["name","preset"]
                }
            },
            {
                "name": "delete_export_preset",
                "description": "Delete a custom export preset (app-level config — no document mutation, no undo step). Built-ins are read-only and refuse deletion with NotSupportedV1.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "name": { "type": "string" } },
                    "required": ["name"]
                }
            },

            // ── Video domain, P4+ slice (10-mcp-tools.md §3.8 captions, §3.9 tts,
            // §3.10 grade, §3.11 node graph, §3.12 audio; 05 §4b title templates).
            // Captions (10 §3.8)
            {
                "name": "auto_caption",
                "description": "Transcribe audio into word-level caption cues (CAP-009; async job — poll get_job_status). Supply clip_id (transcribe that clip's asset) or sequence_id. provider defaults to the configured hosted service (set PHOTONIC_TRANSCRIBE_URL/PHOTONIC_TRANSCRIBE_TOKEN); pass provider=\"mock\" with mock_transcript for a deterministic offline/CI run. On completion inserts grouped cues on a new or existing caption track.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string" },
                        "clip_id": { "type": "string" },
                        "track_id": { "type": "string", "description": "Existing caption track to append to; omit to create one." },
                        "provider": { "type": "string", "enum": ["hosted","mock"], "description": "Default hosted." },
                        "language_hint": { "type": "string" },
                        "mock_transcript": { "type": "string", "description": "provider=mock only: the deterministic transcript distributed across the target range." },
                        "name": { "type": "string", "description": "Name for a newly-created caption track." }
                    }
                }
            },
            {
                "name": "add_caption_track",
                "description": "Add an empty caption track to a sequence (06 §3.6). Undoable.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string" },
                        "name": { "type": "string" }
                    },
                    "required": ["sequence_id"]
                }
            },
            {
                "name": "remove_caption_track",
                "description": "Remove a caption track and all its cues. STRUCTURAL: the committed core has no undoable caption-track removal (tracks are created/removed as side effects of bulk cue insertion, 06 §3.6), so this schedules a history checkpoint but is not a fine-grained undo step.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "track_id": { "type": "string" } },
                    "required": ["track_id"]
                }
            },
            {
                "name": "get_caption_track",
                "description": "Full cue + word + style dump for one caption track (folds list_caption_cues).",
                "inputSchema": {
                    "type": "object",
                    "properties": { "track_id": { "type": "string" } },
                    "required": ["track_id"]
                }
            },
            {
                "name": "set_caption_cue",
                "description": "Create a cue (omit cue_id) or edit one (pass cue_id: timing via RetimeCue, text/words via SetCueText, one undo step). `words` (explicit per-word timing) beats `text` (distributed proportionally). position_override is only honored on creation in v1.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "track_id": { "type": "string" },
                        "cue_id": { "type": "string", "description": "Omit to create a new cue." },
                        "start_ticks": { "type": "integer" },
                        "start_tc": { "type": "string" },
                        "start_seconds": { "type": "number" },
                        "end_ticks": { "type": "integer" },
                        "end_tc": { "type": "string" },
                        "end_seconds": { "type": "number" },
                        "text": { "type": "string" },
                        "words": { "type": "array", "items": { "type": "object" }, "description": "[{text, start_ticks|start_tc|start_seconds, end_*}]" },
                        "position_override": { "type": "array", "items": { "type": "number" }, "description": "Normalized [x, y]." }
                    },
                    "required": ["track_id"]
                }
            },
            {
                "name": "split_caption_cue",
                "description": "Split a cue at a tick — between the two words straddling it (CaptionCmd::SplitCue). Returns the new cue id.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "cue_id": { "type": "string" },
                        "at_ticks": { "type": "integer" },
                        "at_tc": { "type": "string" },
                        "at_seconds": { "type": "number" }
                    },
                    "required": ["cue_id"]
                }
            },
            {
                "name": "merge_caption_cues",
                "description": "Merge two cues on the same caption track into one (CaptionCmd::MergeCues).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "cue_id_a": { "type": "string" },
                        "cue_id_b": { "type": "string" }
                    },
                    "required": ["cue_id_a","cue_id_b"]
                }
            },
            {
                "name": "set_caption_word",
                "description": "Edit one word's text and/or timing (CAP-010), committed as a SetCueText over the cue's word list.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "cue_id": { "type": "string" },
                        "word_index": { "type": "integer" },
                        "text": { "type": "string" },
                        "start_ticks": { "type": "integer" },
                        "start_tc": { "type": "string" },
                        "start_seconds": { "type": "number" },
                        "end_ticks": { "type": "integer" },
                        "end_tc": { "type": "string" },
                        "end_seconds": { "type": "number" }
                    },
                    "required": ["cue_id","word_index"]
                }
            },
            {
                "name": "set_caption_style",
                "description": "Set the track-default style (track_id), a cue override (cue_id), or a word override (cue_id + word_index). Supplied style fields merge onto the current effective style (01 §7 cascade word→cue→track); `clear` removes a cue/word override.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "track_id": { "type": "string" },
                        "cue_id": { "type": "string" },
                        "word_index": { "type": "integer" },
                        "clear": { "type": "boolean" },
                        "style": { "type": "object", "description": "{font_family?, font_size?, weight?, fill? (#hex), position? [x,y], max_width?}" }
                    }
                }
            },
            {
                "name": "import_captions",
                "description": "Import SRT/VTT/ASS subtitles onto an existing caption track (06 §7). Format inferred from the file extension when omitted.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "track_id": { "type": "string" },
                        "path": { "type": "string" },
                        "format": { "type": "string", "enum": ["srt","vtt","ass"] }
                    },
                    "required": ["track_id","path"]
                }
            },
            {
                "name": "export_captions",
                "description": "Export a caption track to SRT/VTT/ASS (06 §7). Format inferred from the file extension when omitted. No document mutation.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "track_id": { "type": "string" },
                        "path": { "type": "string" },
                        "format": { "type": "string", "enum": ["srt","vtt","ass"] }
                    },
                    "required": ["track_id","path"]
                }
            },

            // TTS (10 §3.9)
            {
                "name": "generate_voiceover",
                "description": "Synthesize speech and place it as an audio clip sized to the returned audio (CAP-011; async job — poll get_job_status). provider defaults to the configured hosted TTS (PHOTONIC_TTS_URL/PHOTONIC_TTS_TOKEN); pass provider=\"mock\" for deterministic offline synthesis. also_caption adds word-level captions from the provider's alignment.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" },
                        "track_id": { "type": "string" },
                        "start_ticks": { "type": "integer" },
                        "start_tc": { "type": "string" },
                        "start_seconds": { "type": "number" },
                        "voice": { "type": "string" },
                        "provider": { "type": "string", "enum": ["hosted","mock"] },
                        "also_caption": { "type": "boolean" },
                        "caption_track_id": { "type": "string" }
                    },
                    "required": ["text","track_id"]
                }
            },

            // Grade (10 §3.10)
            {
                "name": "set_grade",
                "description": "Replace or clear a clip's color grade (07 §1). `grade` is a full Grade object {ops:[...], bypass}; omit or pass null to clear.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "grade": { "type": ["object","null"], "description": "Full Grade serde shape; null clears." }
                    },
                    "required": ["clip_id"]
                }
            },
            {
                "name": "apply_lut",
                "description": "Attach a 3D LUT (.cube) to the clip's grade stack, importing it as a LUT asset (07 §3.8). Omit/null lut_path to remove the LUT. Replaces any existing LUT on the clip.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "lut_path": { "type": "string", "description": "Path to a .cube LUT; omit/null removes." },
                        "intensity": { "type": "number", "description": "0..1 blend, default 1.0." }
                    },
                    "required": ["clip_id"]
                }
            },
            {
                "name": "copy_grade",
                "description": "Copy one clip's grade (incl. LUT reference) onto N target clips as a single undo step.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "source_clip_id": { "type": "string" },
                        "target_clip_ids": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["source_clip_id","target_clip_ids"]
                }
            },
            {
                "name": "grade_preset",
                "description": "Save the current clip grade as a named app-level preset, apply a preset to a clip, or list preset names. op: save (clip_id+name) | apply (clip_id+name) | list.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["save","apply","list"] },
                        "clip_id": { "type": "string" },
                        "name": { "type": "string" }
                    },
                    "required": ["op"]
                }
            },
            {
                "name": "get_scopes",
                "description": "Waveform/vectorscope/histogram data for a clip at a tick (07 §5) — data, not an image (the UI/agent renders it). Renders the frame headlessly (requires a GPU adapter, else EngineUnavailable). Reads the K-E2 per-clip tap by default: the clip's own texture after its Grade, before the track fold and CaptionOverlay (03 §3.6), so a clip under a caption track or another video track is measured, not the composite. Returns full luma/RGB histograms, a down-sampled luma waveform, a 32x32 vectorscope grid, and `tap` naming the readback point actually used.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "at_ticks": { "type": "integer" },
                        "at_tc": { "type": "string" },
                        "at_seconds": { "type": "number" },
                        "format_index": { "type": "integer" },
                        "tap": { "type": "string", "enum": ["clip","program"], "description": "Readback point (K-E2). 'clip' (default) = the clip's post-Grade, pre-fold texture; 'program' = the folded sequence pre-CaptionOverlay. A 'clip' tap the frame does not contain falls back to 'program' and says so in `tap`/`tap_fallback_reason`." }
                    },
                    "required": ["clip_id"]
                }
            },

            // Node graph (10 §3.11)
            {
                "name": "create_clip_composition",
                "description": "Instantiate a per-clip node composition (D-06): a fresh ClipIn→Output graph bound to the clip (default), a deep-clone paste of an existing graph_id, or detach=true to revert the clip to its plain source. Returns the new graph_id.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "graph_id": { "type": "string", "description": "Paste a deep-clone of this existing graph." },
                        "detach": { "type": "boolean", "description": "Revert the clip to its plain source." }
                    },
                    "required": ["clip_id"]
                }
            },
            {
                "name": "add_graph_node",
                "description": "Add a node to a graph (08 §2). `op` is a GraphOp in serde shape, e.g. {\"op\":\"blur\"}, {\"op\":\"solid_color\"}, {\"op\":\"merge\",\"mode\":\"normal\"}, {\"op\":\"grade\",\"grade\":{...}}. Returns node_id.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "graph_id": { "type": "string" },
                        "op": { "type": "object", "description": "GraphOp serde shape (08 §2)." },
                        "pos": { "type": "array", "items": { "type": "number" }, "description": "Editor [x, y]." }
                    },
                    "required": ["graph_id","op"]
                }
            },
            {
                "name": "remove_graph_node",
                "description": "Remove a node and its incident edges from a graph (undoable).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "graph_id": { "type": "string" },
                        "node_id": { "type": "string" }
                    },
                    "required": ["graph_id","node_id"]
                }
            },
            {
                "name": "add_graph_edge",
                "description": "Connect one node's output port to another's input port. Cycle-checked at edit time (01 §8) — fails clean with error_code CycleDetected, never panics.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "graph_id": { "type": "string" },
                        "from": { "type": "object", "description": "{node_id, port?} — output port, default 0." },
                        "to": { "type": "object", "description": "{node_id, port?} — input port, default 0." }
                    },
                    "required": ["graph_id","from","to"]
                }
            },
            {
                "name": "remove_graph_edge",
                "description": "Remove the edge at `edge_index` in the graph's edge list (see get_graph for indices).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "graph_id": { "type": "string" },
                        "edge_index": { "type": "integer" }
                    },
                    "required": ["graph_id","edge_index"]
                }
            },
            {
                "name": "set_graph_node_param",
                "description": "Set one PropPath under a node's params (08 §6.4). value is a PropValue, e.g. {\"t\":\"float\",\"v\":12.0}.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "graph_id": { "type": "string" },
                        "node_id": { "type": "string" },
                        "path": { "type": "string" },
                        "value": { "type": "object" }
                    },
                    "required": ["graph_id","node_id","path","value"]
                }
            },
            {
                "name": "set_project_graph",
                "description": "Set the project graph to an existing arena graph (graph_id), clear it (clear=true), or create a fresh empty project graph (omit both). Spliced after the active-sequence output (02 §2). Returns graph_id.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "graph_id": { "type": "string" },
                        "clear": { "type": "boolean" }
                    }
                }
            },
            {
                "name": "get_graph",
                "description": "Full node/edge/param dump for a graph, plus structural diagnostics (cycle / missing Output) and a compiles flag.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "graph_id": { "type": "string" } },
                    "required": ["graph_id"]
                }
            },

            // Audio (10 §3.12)
            {
                "name": "set_clip_audio",
                "description": "Per-clip audio (01 §5): gain trim, fades, channel map. A fade_*_ticks of 0 clears that fade; a positive value sets it. Auto-initializes the clip's ClipAudio container if absent.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "clip_id": { "type": "string" },
                        "gain_db": { "type": "number" },
                        "fade_in_ticks": { "type": "integer", "description": "0 clears; >0 sets." },
                        "fade_out_ticks": { "type": "integer" },
                        "fade_shape": { "type": "string", "enum": ["linear","equal_power","log","s_curve"] },
                        "channel_map": { "type": "string", "enum": ["as_source","mono_downmix","stereo_lr","channel_swap"] }
                    },
                    "required": ["clip_id"]
                }
            },
            {
                "name": "set_track_audio",
                "description": "Per-track fader/pan/mute/solo (09 §4). Auto-initializes the track's TrackAudio container if absent.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "track_id": { "type": "string" },
                        "volume_db": { "type": "number", "description": "-inf..+12, default 0." },
                        "pan": { "type": "number", "description": "-1 (L)..1 (R)." },
                        "muted": { "type": "boolean" },
                        "solo": { "type": "boolean" }
                    },
                    "required": ["track_id"]
                }
            },
            {
                "name": "audio_fx",
                "description": "Add/remove/reorder an EQ/compressor/limiter/gate unit in a track's pre-fader fx chain (09 §4). op: add (kind, index?) | remove (index) | reorder (new_order).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "track_id": { "type": "string" },
                        "op": { "type": "string", "enum": ["add","remove","reorder"] },
                        "kind": { "type": "string", "enum": ["eq","compressor","limiter","gate"] },
                        "index": { "type": "integer" },
                        "new_order": { "type": "array", "items": { "type": "integer" } }
                    },
                    "required": ["track_id","op"]
                }
            },
            {
                "name": "set_master_bus",
                "description": "Sequence master bus level and export loudness normalization (09 §4/§6.5). loudness: streaming (-14 LUFS) | broadcast (-23 LUFS) | none.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sequence_id": { "type": "string" },
                        "volume_db": { "type": "number" },
                        "loudness": { "type": "string", "enum": ["streaming","broadcast","none"] }
                    },
                    "required": ["sequence_id"]
                }
            },
            {
                "name": "get_audio_meters",
                "description": "Current/peak levels per track + master (09 §5). Live meters live inside the interactive audio mixer, which the headless MCP engine bridge does not run — returns NotSupportedV1.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "sequence_id": { "type": "string" } },
                    "required": ["sequence_id"]
                }
            },
            {
                "name": "get_waveform",
                "description": "Decoded waveform peak-pyramid summary for an asset or clip's asset (09 §8), sidecar-cached by content hash (01 §9); built via the ffmpeg audio decoder on a cache miss. Returns per-channel [min, max, rms] peak buckets (not an image). Supply asset_id or clip_id.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "asset_id": { "type": "string" },
                        "clip_id": { "type": "string" },
                        "resolution": { "type": "integer", "description": "Target peak buckets per channel, default 512." }
                    }
                }
            },

            // Title templates (05 §4b)
            {
                "name": "list_title_templates",
                "description": "List available vector title/lower-third templates (05 §4b). The shipped built-in library is a P6 deliverable not yet present in this build, so this returns an empty catalog.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "insert_title_template",
                "description": "Insert a vector title template onto the timeline as an embedded VectorDoc clip (05 §4b). The template library is not shipped in this build (P6) — returns NotSupportedV1.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "template": { "type": "string" },
                        "track_id": { "type": "string" },
                        "start_ticks": { "type": "integer" },
                        "start_tc": { "type": "string" },
                        "start_seconds": { "type": "number" },
                        "text_overrides": { "type": "object" }
                    },
                    "required": ["template","track_id"]
                }
            }
        ]);
    if let Some(definitions) = tools.as_array_mut() {
        for definition in definitions.iter_mut() {
            let name = definition["name"].as_str().unwrap_or_default();
            let output = known_output_schema(name);
            let behavior = known_behavior(name);
            if let Some(schema) = output {
                definition["outputSchema"] = result_schema(schema);
            }
            if let Some(behavior) = behavior {
                definition["annotations"] = behavior.annotations();
            }
        }
        definitions.extend(crate::handlers::video_transcript::tool_schemas());
        definitions.extend(crate::handlers::video_inspect::tool_schemas());
        definitions.push(crate::handlers::video_export::tool_schema(&definitions));
        definitions.extend(crate::handlers::video_workflows::tool_schemas());
        // Plans embed the complete schemas of their allowed operations,
        // including transcript tools; build them last without catalog lookup.
        definitions.push(crate::handlers::video_edits::tool_schema(definitions));
    }
    tools
}

/// Explicit behavior classifications for tools whose implementations have been
/// checked. MCP read-only includes the whole environment, not just document
/// history: playback, job cancellation, export, and cache writes are mutations.
#[derive(Clone, Copy, Debug)]
pub enum ToolBehavior {
    /// Reads the document or an in-memory catalog without editing state.
    ReadOnly,
    /// Adds document content without replacing or removing existing content.
    Additive,
    /// May change existing document or session state.
    Mutation,
    /// Reads external files or providers without modifying them.
    ExternalRead,
    /// May write files, change external state, or start background work.
    ExternalMutation,
}

impl ToolBehavior {
    fn annotations(self) -> Value {
        match self {
            Self::ReadOnly => json!({ "readOnlyHint": true, "openWorldHint": false }),
            Self::ExternalRead => json!({ "readOnlyHint": true, "openWorldHint": true }),
            Self::Additive => json!({
                "readOnlyHint": false, "destructiveHint": false,
                "idempotentHint": false, "openWorldHint": false
            }),
            Self::Mutation => json!({
                "readOnlyHint": false, "destructiveHint": true, "openWorldHint": false
            }),
            Self::ExternalMutation => json!({
                "readOnlyHint": false, "destructiveHint": true, "openWorldHint": true
            }),
        }
    }
}

/// Build a tool definition with a verified success schema and behavior. The
/// output automatically includes the common execution-error shape. Keep the
/// success schema faithful to all successful branches, including empty results.
pub fn tool_definition(
    name: &str,
    description: &str,
    input_schema: Value,
    success_output_schema: Value,
    behavior: ToolBehavior,
) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
        "outputSchema": result_schema(success_output_schema),
        "annotations": behavior.annotations(),
    })
}

fn result_schema(success: Value) -> Value {
    json!({
        "anyOf": [success, {
            "type": "object",
            "properties": {
                "error_code": { "type": "string", "description": "Machine-readable domain code, or ToolExecutionError when no specific code is available." },
                "message": { "type": "string" }
            },
            "required": ["error_code", "message"],
            "additionalProperties": true
        }]
    })
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required })
}

fn id_result(field: &str) -> Value {
    object_schema(
        json!({ field: { "type": "string", "format": "uuid" } }),
        &[field],
    )
}

fn list_result(field: &str, item: Value) -> Value {
    object_schema(
        json!({ field: { "type": "array", "items": item } }),
        &[field],
    )
}

fn message_schema() -> Value {
    object_schema(json!({ "message": { "type": "string" } }), &["message"])
}

fn action_definition_schema() -> Value {
    object_schema(
        json!({
            "name": { "type": "string" },
            "description": { "type": "string" },
            "inputSchema": { "type": "object" },
            "outputSchema": { "type": "object" },
            "annotations": { "type": "object" },
            "score": { "type": "integer", "minimum": 1 }
        }),
        &["name", "description", "inputSchema"],
    )
}

fn artboard_schema() -> Value {
    object_schema(
        json!({
            "id": { "type": "string", "format": "uuid" },
            "name": { "type": "string" },
            "x": { "type": "number" }, "y": { "type": "number" },
            "width": { "type": "number" }, "height": { "type": "number" },
            "active": { "type": "boolean" }
        }),
        &["id", "name", "x", "y", "width", "height", "active"],
    )
}

fn engine_status_schema() -> Value {
    object_schema(
        json!({
            "playhead_ticks": { "type": "integer" },
            "playing": { "type": "boolean" },
            "dropped_frames": { "type": "integer", "minimum": 0 },
            "cache": object_schema(json!({
                "hits": { "type": "integer", "minimum": 0 },
                "misses": { "type": "integer", "minimum": 0 },
                "resident_entries": { "type": "integer", "minimum": 0 },
                "resident_bytes": { "type": "integer", "minimum": 0 }
            }), &["hits", "misses", "resident_entries", "resident_bytes"]),
            "audio_xruns": { "type": "integer", "minimum": 0 },
            "doc_revision": { "type": "integer", "minimum": 0 },
            "active_sequence": { "type": ["string", "null"], "format": "uuid" },
            "last_error": { "type": ["object", "null"] },
            "snapshot_synced": { "type": "boolean" }
        }),
        &[
            "playhead_ticks",
            "playing",
            "dropped_frames",
            "cache",
            "audio_xruns",
            "doc_revision",
            "active_sequence",
            "last_error",
        ],
    )
}

fn job_status_schema() -> Value {
    object_schema(
        json!({
            "job_id": { "type": "string", "format": "uuid" },
            "kind": { "type": "string" },
            "status": { "oneOf": [
                object_schema(json!({ "state": { "const": "queued" } }), &["state"]),
                object_schema(json!({
                    "state": { "const": "running" }, "progress": { "type": "number" },
                    "message": { "type": "string" }
                }), &["state", "progress", "message"]),
                object_schema(json!({ "state": { "const": "done" }, "result": {} }), &["state", "result"]),
                object_schema(json!({
                    "state": { "const": "failed" }, "error_code": { "type": "string" },
                    "message": { "type": "string" }
                }), &["state", "error_code", "message"]),
                object_schema(json!({ "state": { "const": "cancelled" } }), &["state"])
            ]}
        }),
        &["job_id", "kind", "status"],
    )
}

// Deliberate allowlists, not name-prefix guesses. Unreviewed legacy tools keep
// their schemas/annotations absent instead of advertising invented contracts.
fn known_output_schema(name: &str) -> Option<Value> {
    Some(match name {
        "render_frame_at" => object_schema(
            json!({
                "width":{"type":"integer","minimum":1},"height":{"type":"integer","minimum":1},"tick":{"type":"integer","minimum":0},
                "render_ms":{"type":"integer","minimum":0},"output_format":{"type":"string","enum":["png","raw_rgba16f"]},
                "data_base64":{"type":"string"},"encoding":{"type":"string"},"sequence_id":{"type":"string","format":"uuid"},
                "revision":{"type":"integer","minimum":0},"snapshot_generation":{"type":"integer","minimum":0},"format_index":{"type":"integer","minimum":0},
                "quality":{"type":"string","enum":["preview","full"]},"processing_quality":{"type":"string","enum":["draft","full"]},
                "source_width":{"type":"integer","minimum":1},"source_height":{"type":"integer","minimum":1},"gpu_downscaled":{"type":"boolean"}
            }),
            &[
                "width",
                "height",
                "tick",
                "render_ms",
                "output_format",
                "sequence_id",
                "revision",
                "snapshot_generation",
                "format_index",
                "quality",
                "processing_quality",
                "source_width",
                "source_height",
                "gpu_downscaled",
            ],
        ),
        "get_scopes" => object_schema(
            json!({
                "tick":{"type":"integer"},"histogram":{"type":"object"},"waveform":{"type":"object"},"vectorscope":{"type":"object"},
                "tap":{"type":"string","enum":["clip","program"]},"width":{"type":"integer","minimum":1},"height":{"type":"integer","minimum":1},"tap_fallback_reason":{"type":"string"}
            }),
            &[
                "tick",
                "histogram",
                "waveform",
                "vectorscope",
                "tap",
                "width",
                "height",
            ],
        ),

        "search_actions" => object_schema(
            json!({
                "actions": { "type": "array", "items": action_definition_schema(), "maxItems": 50 },
                "count": { "type": "integer", "minimum": 0, "maximum": 50 },
                "query": { "type": "string" }
            }),
            &["actions", "count", "query"],
        ),
        "get_action_schema" => action_definition_schema(),
        "create_shape" | "add_graph_node" => id_result("node_id"),
        "create_sequence" => id_result("sequence_id"),
        "add_track" | "add_caption_track" => id_result("track_id"),
        "insert_clip"
        | "insert_edit"
        | "overwrite_edit"
        | "replace_clip_source"
        | "insert_adjustment_clip"
        | "insert_text_clip" => id_result("clip_id"),
        "split_clip" => id_result("new_clip_id"),
        "split_caption_cue" => id_result("new_cue_id"),
        "create_bin" => id_result("bin_id"),
        "add_marker" | "add_clip_marker" => id_result("marker_id"),
        "add_marker_category" => id_result("category_id"),
        "probe_media" | "generate_voiceover" => id_result("job_id"),
        "set_caption_cue" => {
            let mut schema = id_result("cue_id");
            schema["properties"]["note"] = json!({ "type": ["string", "null"] });
            schema
        }
        "set_paint" => object_schema(
            json!({
                "applied_count": { "type": "integer", "minimum": 0 },
                "target": { "enum": ["fill", "stroke"] },
                "skipped": { "type": "array", "items": { "type": "string" } }
            }),
            &["applied_count", "target", "skipped"],
        ),
        "save_document" => object_schema(
            json!({
                "path": { "type": "string" }, "bytes": { "type": "integer", "minimum": 0 }
            }),
            &["path", "bytes"],
        ),
        "list_artboards" => object_schema(
            json!({
                "artboards": { "type": "array", "items": artboard_schema() },
                "active_artboard": { "type": ["string", "null"], "format": "uuid" }
            }),
            &["artboards", "active_artboard"],
        ),
        "get_document_info" => object_schema(
            json!({
                "name": { "type": "string" },
                "canvas": object_schema(json!({
                    "width": { "type": "number" }, "height": { "type": "number" }
                }), &["width", "height"]),
                "layer_count": { "type": "integer", "minimum": 0 },
                "layers": { "type": "array", "items": { "type": "object" } },
                "artboard_count": { "type": "integer", "minimum": 0 },
                "artboards": { "type": "array", "items": artboard_schema() },
                "active_artboard": { "type": ["string", "null"], "format": "uuid" },
                "dpi": { "type": "number" }, "bleed_mm": { "type": "number" },
                "slug_mm": { "type": "number" }, "color_mode": { "enum": ["rgb", "cmyk"] },
                "nodes": object_schema(json!({
                    "total": { "type": "integer", "minimum": 0 }, "path": { "type": "integer", "minimum": 0 },
                    "text": { "type": "integer", "minimum": 0 }, "group": { "type": "integer", "minimum": 0 }
                }), &["total", "path", "text", "group"]),
                "font_names": { "type": "array", "items": { "type": "string" }, "maxItems": 20 },
                "fill_colors": { "type": "array", "items": { "type": "string" }, "maxItems": 20 }
            }),
            &[
                "name",
                "canvas",
                "layer_count",
                "layers",
                "artboard_count",
                "artboards",
                "active_artboard",
                "dpi",
                "bleed_mm",
                "slug_mm",
                "color_mode",
                "nodes",
                "font_names",
                "fill_colors",
            ],
        ),
        "get_document_state" => object_schema(
            json!({
                "id": { "type": "string", "format": "uuid" }, "name": { "type": "string" },
                "width": { "type": "number" }, "height": { "type": "number" },
                "node_count": { "type": "integer", "minimum": 0 }, "layer_count": { "type": "integer", "minimum": 0 },
                "active_layer_id": { "type": ["string", "null"], "format": "uuid" },
                "selection": { "type": "array", "items": { "type": "string", "format": "uuid" } },
                "layers": { "type": "array", "items": { "type": "object" } }
            }),
            &[
                "id",
                "name",
                "width",
                "height",
                "node_count",
                "layer_count",
                "active_layer_id",
                "selection",
                "layers",
            ],
        ),
        "list_sequences" => list_result(
            "sequences",
            object_schema(
                json!({
                    "sequence_id": { "type": "string", "format": "uuid" }, "name": { "type": "string" },
                    "frame_rate": object_schema(json!({
                        "num": { "type": "integer", "minimum": 0 }, "den": { "type": "integer", "minimum": 0 }
                    }), &["num", "den"]),
                    "formats": { "type": "array", "items": object_schema(json!({
                        "name": { "type": "string" }, "width": { "type": "integer", "minimum": 0 },
                        "height": { "type": "integer", "minimum": 0 }
                    }), &["name", "width", "height"]) },
                    "active_format": { "type": "integer", "minimum": 0 },
                    "video_track_count": { "type": "integer", "minimum": 0 },
                    "audio_track_count": { "type": "integer", "minimum": 0 }, "is_active": { "type": "boolean" }
                }),
                &[
                    "sequence_id",
                    "name",
                    "frame_rate",
                    "formats",
                    "active_format",
                    "video_track_count",
                    "audio_track_count",
                    "is_active",
                ],
            ),
        ),
        "list_clips" => list_result(
            "clips",
            object_schema(
                json!({
                    "clip_id": { "type": "string", "format": "uuid" }, "name": { "type": "string" },
                    "sequence_id": { "type": "string", "format": "uuid" }, "track_id": { "type": "string", "format": "uuid" },
                    "start_ticks": { "type": "integer" }, "duration_ticks": { "type": "integer" }, "enabled": { "type": "boolean" }
                }),
                &[
                    "clip_id",
                    "name",
                    "sequence_id",
                    "track_id",
                    "start_ticks",
                    "duration_ticks",
                    "enabled",
                ],
            ),
        ),
        "get_clip" => object_schema(
            json!({
                "sequence_id": { "type": "string", "format": "uuid" },
                "track_id": { "type": "string", "format": "uuid" }, "clip": { "type": "object" }
            }),
            &["sequence_id", "track_id", "clip"],
        ),
        "import_media" => list_result(
            "assets",
            object_schema(
                json!({
                    "asset_id": { "type": "string", "format": "uuid" }, "path": { "type": "string" },
                    "kind": { "type": "string" }, "probed": { "type": "boolean" },
                    "bin_id": { "type": ["string", "null"], "format": "uuid" }
                }),
                &["asset_id", "path", "kind", "probed", "bin_id"],
            ),
        ),
        "list_media" => list_result(
            "assets",
            object_schema(
                json!({
                    "asset_id": { "type": "string", "format": "uuid" }, "kind": { "type": "string" },
                    "source": { "type": "object" }, "probed": { "type": "boolean" },
                    "proxy_status": { "type": ["string", "null"] }, "content_hash": { "type": ["string", "null"] },
                    "bin_id": { "type": ["string", "null"], "format": "uuid" }
                }),
                &[
                    "asset_id",
                    "kind",
                    "source",
                    "probed",
                    "proxy_status",
                    "content_hash",
                    "bin_id",
                ],
            ),
        ),
        "list_bins" => list_result("bins", json!({ "type": "object" })),
        "list_markers" => list_result("markers", json!({ "type": "object" })),
        "list_clip_markers" => object_schema(
            json!({
                "markers": { "type": "array", "items": { "type": "object" } },
                "sequence_id": { "type": "string", "format": "uuid" }, "clip_start_ticks": { "type": "integer" }
            }),
            &["markers", "sequence_id", "clip_start_ticks"],
        ),
        "list_marker_categories" | "seed_marker_categories" => {
            list_result("categories", json!({ "type": "object" }))
        }
        "remove_marker_category" => object_schema(
            json!({
                "markers_retargeted": { "type": "integer", "minimum": 0 }
            }),
            &["markers_retargeted"],
        ),
        "list_effect_kinds" => list_result("effect_kinds", json!({ "type": "object" })),
        "list_export_presets" => list_result("presets", json!({ "type": "object" })),
        "list_title_templates" => list_result("templates", json!({ "type": "object" })),
        "get_keyframes" => list_result("tracks", json!({ "type": "object" })),
        "copy_keyframes" => {
            object_schema(json!({ "clipboard": { "type": "object" } }), &["clipboard"])
        }
        "create_subclip" => object_schema(
            json!({
                "asset_id": { "type": "string", "format": "uuid" },
                "parent_asset_id": { "type": "string", "format": "uuid" },
                "in_ticks": { "type": "integer" }, "out_ticks": { "type": "integer" }
            }),
            &["asset_id", "parent_asset_id", "in_ticks", "out_ticks"],
        ),
        "set_loop_range" => json!({ "anyOf": [message_schema(), object_schema(json!({
            "start_ticks": { "type": "integer" }, "end_ticks": { "type": "integer" }
        }), &["start_ticks", "end_ticks"])] }),
        "set_proxy_mode" => object_schema(
            json!({
                "mode": { "enum": ["Auto", "ForceProxy", "ForceOriginal"] }, "note": { "type": "string" }
            }),
            &["mode", "note"],
        ),
        "create_clip_composition" | "set_project_graph" => {
            json!({ "anyOf": [message_schema(), id_result("graph_id")] })
        }
        "effect_stack" => json!({ "anyOf": [message_schema(), object_schema(json!({
            "scope": { "type": "string" }, "effects": { "type": "array", "items": { "type": "object" } },
            "grade": { "type": ["object", "null"] }
        }), &["scope", "effects", "grade"])] }),
        "paste_attributes" => object_schema(
            json!({
                "source_clip_id": { "type": "string", "format": "uuid" },
                "attributes": { "type": "array", "items": { "enum": ["effects", "grade", "transform", "audio"] } },
                "updated": { "type": "array", "items": { "type": "string", "format": "uuid" } },
                "skipped": { "type": "array", "items": { "type": "string", "format": "uuid" } }
            }),
            &["source_clip_id", "attributes", "updated", "skipped"],
        ),
        "effect_preset_list" => object_schema(
            json!({
                "presets": { "type": "array", "items": { "type": "object" } },
                "library_path": { "type": ["string", "null"] }, "quarantined": { "type": ["string", "null"] }
            }),
            &["presets", "library_path", "quarantined"],
        ),
        "effect_preset_save" => object_schema(
            json!({
                "preset": { "type": "object" }, "replaced": { "type": "boolean" }
            }),
            &["preset", "replaced"],
        ),
        "effect_preset_apply" => object_schema(
            json!({
                "name": { "type": "string" }, "targets": { "type": "integer", "minimum": 0 },
                "commands": { "type": "integer", "minimum": 0 },
                "unresolvable_effect_ids": { "type": "array", "items": { "type": "string" } }
            }),
            &["name", "targets", "commands", "unresolvable_effect_ids"],
        ),
        "effect_favourite_list" => list_result("favourites", json!({ "type": "object" })),
        "relink_media" => object_schema(
            json!({
                "asset_id": { "type": "string", "format": "uuid" }, "new_path": { "type": "string" },
                "hash": { "enum": ["match", "mismatch", "unknown"] }
            }),
            &["asset_id", "new_path", "hash"],
        ),
        "find_offline_media" => object_schema(
            json!({
                "offline": { "type": "array", "items": { "type": "object" } },
                "pool_size": { "type": "integer", "minimum": 0 }
            }),
            &["offline"],
        ),
        "relink_media_batch" => object_schema(
            json!({
                "dry_run": { "type": "boolean" }, "scanned_files": { "type": "integer", "minimum": 0 },
                "scan_truncated": { "type": "boolean" }, "hashed_scan": { "type": "boolean" },
                "relinked": { "type": "array", "items": { "type": "object" } },
                "skipped_hash_mismatch": { "type": "array", "items": { "type": "object" } },
                "unmatched": { "type": "array", "items": { "type": "object" } }
            }),
            &[
                "dry_run",
                "scanned_files",
                "scan_truncated",
                "hashed_scan",
                "relinked",
                "skipped_hash_mismatch",
                "unmatched",
            ],
        ),
        "remove_proxy" => object_schema(
            json!({
                "assets": { "type": "array", "items": { "type": "object" } },
                "files_deleted": { "type": "integer", "minimum": 0 }
            }),
            &["assets", "files_deleted"],
        ),
        "attach_proxy" => object_schema(
            json!({
                "asset_id": { "type": "string", "format": "uuid" }, "path": { "type": "string" },
                "origin": { "const": "attached" }, "warnings": { "type": "array", "items": { "type": "string" } }
            }),
            &["asset_id", "path", "origin", "warnings"],
        ),
        "detach_proxy" => object_schema(
            json!({
                "asset_id": { "type": "string", "format": "uuid" }, "path": { "type": ["string", "null"] },
                "origin": { "enum": ["generated", "attached", "none"] },
                "detached": { "type": "boolean" }, "file_deleted": { "const": false }
            }),
            &["asset_id", "path", "origin", "detached", "file_deleted"],
        ),
        "import_captions" => object_schema(
            json!({
                "cues_imported": { "type": "integer", "minimum": 0 },
                "notes": { "type": "array", "items": { "type": "string" } }
            }),
            &["cues_imported", "notes"],
        ),
        "export_captions" => object_schema(
            json!({
                "path": { "type": "string" }, "notes": { "type": "array", "items": { "type": "string" } }
            }),
            &["path", "notes"],
        ),
        "grade_preset" => {
            json!({ "anyOf": [message_schema(), list_result("presets", json!({ "type": "string" }))] })
        }
        "get_audio_meters" => object_schema(
            json!({
                "sequence_id": { "type": "string", "format": "uuid" },
                "peak": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 },
                "rms": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 },
                "graph_latency_samples": { "type": "integer", "minimum": 0 }, "source": { "const": "mixer_output" }
            }),
            &[
                "sequence_id",
                "peak",
                "rms",
                "graph_latency_samples",
                "source",
            ],
        ),
        "get_waveform" => json!({ "anyOf": [
            object_schema(json!({ "channels": { "type": "integer", "minimum": 0 },
                "buckets": { "type": "array", "maxItems": 0 }
            }), &["channels", "buckets"]),
            object_schema(json!({
                "channels": { "type": "integer", "minimum": 0 }, "source_sample_rate": { "type": "integer", "minimum": 0 },
                "total_frames": { "type": "integer", "minimum": 0 }, "bucket_format": { "const": "[min, max, rms]" },
                "resolution": { "type": "integer", "minimum": 0 },
                "waveform": { "type": "array", "items": { "type": "array", "items": {
                    "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3
                } } }
            }), &["channels", "source_sample_rate", "total_frames", "bucket_format", "resolution", "waveform"])
        ] }),
        "match_frame" => object_schema(
            json!({
                "source_tick": { "type": "integer" }, "asset_id": { "type": ["string", "null"], "format": "uuid" },
                "clip_name": { "type": "string" }
            }),
            &["source_tick", "asset_id", "clip_name"],
        ),
        "add_edit_all_tracks" => object_schema(
            json!({
                "split_count": { "type": "integer", "minimum": 0 },
                "new_clip_ids": { "type": "array", "items": { "type": "string", "format": "uuid" } }
            }),
            &["split_count", "new_clip_ids"],
        ),
        "close_gap" => object_schema(
            json!({ "tracks_changed": { "type": "integer", "minimum": 0 } }),
            &["tracks_changed"],
        ),
        "insert_space" | "remove_space" | "remove_all_spaces_after" => object_schema(
            json!({
                "commands": { "type": "integer", "minimum": 0 }, "amount_ticks": { "type": "integer" }
            }),
            &["commands"],
        ),
        "remove_clips_after" => object_schema(
            json!({ "removed": { "type": "integer", "minimum": 0 } }),
            &["removed"],
        ),
        "play" | "pause" | "seek" | "step" | "get_engine_status" => engine_status_schema(),
        "get_job_status" => job_status_schema(),
        "auto_caption" => object_schema(
            json!({
                "job_id": { "type": "string", "format": "uuid" }, "track_id": { "type": "string", "format": "uuid" }
            }),
            &["job_id", "track_id"],
        ),
        "generate_proxies" => object_schema(
            json!({
                "job_id": { "type": "string", "format": "uuid" }, "skipped": { "type": "array", "items": { "type": "object" } }
            }),
            &["skipped"],
        ),
        "transcode_media" => object_schema(
            json!({
                "job_id": { "type": "string", "format": "uuid" }, "output_path": { "type": "string" }
            }),
            &["job_id", "output_path"],
        ),
        "export_sequence" => object_schema(
            json!({
                "job_id": { "type": "string", "format": "uuid" }, "total_frames": { "type": "integer", "minimum": 0 },
                "width": { "type": "integer", "minimum": 0 }, "height": { "type": "integer", "minimum": 0 },
                "preset": { "type": "string" }, "audio": { "type": "string" }
            }),
            &[
                "job_id",
                "total_frames",
                "width",
                "height",
                "preset",
                "audio",
            ],
        ),
        "get_caption_track" => object_schema(
            json!({
                "sequence_id": { "type": "string", "format": "uuid" }, "track": { "type": "object" }
            }),
            &["sequence_id", "track"],
        ),
        "get_graph" => object_schema(
            json!({
                "graph": { "type": "object" }, "diagnostics": { "type": "array", "items": { "type": "string" } },
                "compiles": { "type": "boolean" }
            }),
            &["graph", "diagnostics", "compiles"],
        ),
        "undo"
        | "redo"
        | "screenshot"
        | "delete_sequence"
        | "set_active_sequence"
        | "set_sequence_format"
        | "set_active_format"
        | "set_work_range"
        | "remove_track"
        | "set_track_prop"
        | "reorder_track"
        | "remove_clip"
        | "move_clip"
        | "move_clips"
        | "trim_clip"
        | "roll_edit"
        | "slip_clip"
        | "slide_clip"
        | "ripple_edit"
        | "lift_edit"
        | "extract_edit"
        | "set_clip_prop"
        | "link_clips"
        | "unlink_clips"
        | "set_clip_speed"
        | "freeze_frame"
        | "set_transition"
        | "add_effect"
        | "remove_effect"
        | "reorder_effects"
        | "set_effect_param"
        | "set_effect_zone"
        | "set_keyframe"
        | "remove_keyframe"
        | "batch_set_keyframes"
        | "paste_keyframes"
        | "remove_asset"
        | "set_asset_tags"
        | "remove_bin"
        | "set_asset_bin"
        | "cancel_job"
        | "save_export_preset"
        | "delete_export_preset"
        | "remove_caption_track"
        | "merge_caption_cues"
        | "set_caption_word"
        | "set_caption_style"
        | "set_grade"
        | "apply_lut"
        | "copy_grade"
        | "remove_graph_node"
        | "add_graph_edge"
        | "remove_graph_edge"
        | "set_graph_node_param"
        | "set_clip_audio"
        | "set_track_audio"
        | "audio_fx"
        | "set_master_bus"
        | "insert_title_template"
        | "remove_marker"
        | "set_marker"
        | "remove_clip_marker"
        | "update_marker_category"
        | "import_motion_metadata"
        | "set_stabilization"
        | "analyze_stabilization"
        | "get_stabilization_status"
        | "effect_preset_delete"
        | "effect_preset_rename"
        | "effect_favourite_set" => message_schema(),
        _ => return None,
    })
}

fn known_behavior(name: &str) -> Option<ToolBehavior> {
    use ToolBehavior::{Additive, ExternalMutation, ExternalRead, Mutation, ReadOnly};
    Some(match name {
        "search_actions"
        | "get_action_schema"
        | "get_document_info"
        | "get_document_state"
        | "list_artboards"
        | "screenshot"
        | "list_sequences"
        | "list_clips"
        | "get_clip"
        | "list_markers"
        | "list_clip_markers"
        | "list_marker_categories"
        | "list_media"
        | "list_bins"
        | "list_effect_kinds"
        | "get_keyframes"
        | "copy_keyframes"
        | "match_frame"
        | "get_job_status"
        | "get_caption_track"
        | "get_graph"
        | "get_audio_meters"
        | "list_title_templates"
        | "get_stabilization_status" => ReadOnly,
        "list_export_presets" | "find_offline_media" => ExternalRead,
        "create_shape"
        | "create_sequence"
        | "add_track"
        | "insert_clip"
        | "insert_adjustment_clip"
        | "insert_text_clip"
        | "create_bin"
        | "add_caption_track"
        | "add_graph_node" => Additive,
        "undo"
        | "redo"
        | "set_paint"
        | "delete_sequence"
        | "set_active_sequence"
        | "set_sequence_format"
        | "set_active_format"
        | "set_work_range"
        | "add_marker"
        | "set_marker"
        | "remove_marker"
        | "add_clip_marker"
        | "remove_clip_marker"
        | "seed_marker_categories"
        | "add_marker_category"
        | "update_marker_category"
        | "remove_marker_category"
        | "remove_track"
        | "set_track_prop"
        | "reorder_track"
        | "remove_clip"
        | "move_clip"
        | "move_clips"
        | "trim_clip"
        | "split_clip"
        | "roll_edit"
        | "slip_clip"
        | "slide_clip"
        | "ripple_edit"
        | "insert_edit"
        | "overwrite_edit"
        | "lift_edit"
        | "extract_edit"
        | "replace_clip_source"
        | "add_edit_all_tracks"
        | "close_gap"
        | "insert_space"
        | "remove_space"
        | "remove_all_spaces_after"
        | "remove_clips_after"
        | "set_clip_prop"
        | "link_clips"
        | "unlink_clips"
        | "set_clip_speed"
        | "freeze_frame"
        | "set_transition"
        | "add_effect"
        | "remove_effect"
        | "reorder_effects"
        | "set_effect_param"
        | "set_effect_zone"
        | "effect_stack"
        | "paste_attributes"
        | "set_keyframe"
        | "remove_keyframe"
        | "batch_set_keyframes"
        | "paste_keyframes"
        | "remove_asset"
        | "set_asset_tags"
        | "create_subclip"
        | "remove_bin"
        | "set_asset_bin"
        | "cancel_job"
        | "remove_caption_track"
        | "set_caption_cue"
        | "split_caption_cue"
        | "merge_caption_cues"
        | "set_caption_word"
        | "set_caption_style"
        | "set_grade"
        | "copy_grade"
        | "create_clip_composition"
        | "remove_graph_node"
        | "add_graph_edge"
        | "remove_graph_edge"
        | "set_graph_node_param"
        | "set_project_graph"
        | "set_clip_audio"
        | "set_track_audio"
        | "audio_fx"
        | "set_master_bus"
        | "set_stabilization"
        | "insert_title_template" => Mutation,
        "execute_action"
        | "save_document"
        | "import_media"
        | "relink_media"
        | "relink_media_batch"
        | "play"
        | "pause"
        | "seek"
        | "step"
        | "get_engine_status"
        | "render_frame_at"
        | "set_loop_range"
        | "set_proxy_mode"
        | "probe_media"
        | "generate_proxies"
        | "remove_proxy"
        | "attach_proxy"
        | "detach_proxy"
        | "transcode_media"
        | "export_sequence"
        | "save_export_preset"
        | "delete_export_preset"
        | "auto_caption"
        | "import_captions"
        | "export_captions"
        | "generate_voiceover"
        | "apply_lut"
        | "grade_preset"
        | "get_scopes"
        | "get_waveform"
        | "import_motion_metadata"
        | "analyze_stabilization"
        | "effect_preset_list"
        | "effect_favourite_list"
        | "effect_preset_save"
        | "effect_preset_apply"
        | "effect_preset_delete"
        | "effect_preset_rename"
        | "effect_favourite_set" => ExternalMutation,
        _ => return None,
    })
}
