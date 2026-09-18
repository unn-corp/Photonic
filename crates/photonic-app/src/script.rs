//! Lua scripting engine.
//!
//! Exposes a `photonic.*` API to Lua scripts so they can create and manipulate
//! vector art programmatically, then render it to PNG.
//!
//! Run with:  `photonic run my_scene.lua`

use anyhow::{Context, Result};
use mlua::prelude::*;
use photonic_core::{
    history::{Command, CommandHistory},
    node::PathNode,
    ops::boolean::{boolean_op as run_boolean_op, BooleanOp},
    style::Stroke,
    Color, Document, Fill, PathData, SceneNode, SceneNodeKind,
};
use photonic_render::HeadlessRenderer;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

// ─── Public entry point ───────────────────────────────────────────────────────

pub fn run_script(script_path: &Path) -> Result<()> {
    let script_dir = script_path.parent().unwrap_or(Path::new("."));

    let script = std::fs::read_to_string(script_path)
        .with_context(|| format!("Cannot read script: {}", script_path.display()))?;

    let doc: Arc<Mutex<Document>> = Arc::new(Mutex::new(Document::default_artboard()));
    let history: Arc<Mutex<CommandHistory>> = Arc::new(Mutex::new(CommandHistory::new(200)));
    let renderer = Arc::new(pollster::block_on(HeadlessRenderer::new()));

    let lua = Lua::new();
    register_api(
        &lua,
        Arc::clone(&doc),
        Arc::clone(&history),
        Arc::clone(&renderer),
        script_dir.to_path_buf(),
    )
    .map_err(|e| anyhow::anyhow!("API registration error: {}", e))?;

    lua.load(&script)
        .set_name(
            script_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .as_ref(),
        )
        .exec()
        .map_err(|e| anyhow::anyhow!("Script error in {}: {}", script_path.display(), e))?;

    Ok(())
}

// ─── API registration ─────────────────────────────────────────────────────────

fn register_api(
    lua: &Lua,
    doc: Arc<Mutex<Document>>,
    history: Arc<Mutex<CommandHistory>>,
    renderer: Arc<HeadlessRenderer>,
    script_dir: std::path::PathBuf,
) -> LuaResult<()> {
    let p = lua.create_table()?;

    // ── Document info ─────────────────────────────────────────────────────────

    {
        let d = Arc::clone(&doc);
        p.set(
            "width",
            lua.create_function(move |_, ()| Ok(d.lock().unwrap().width))?,
        )?;
    }
    {
        let d = Arc::clone(&doc);
        p.set(
            "height",
            lua.create_function(move |_, ()| Ok(d.lock().unwrap().height))?,
        )?;
    }
    {
        let d = Arc::clone(&doc);
        p.set(
            "name",
            lua.create_function(move |_, ()| Ok(d.lock().unwrap().name.clone()))?,
        )?;
    }
    {
        let d = Arc::clone(&doc);
        p.set(
            "node_count",
            lua.create_function(move |_, ()| Ok(d.lock().unwrap().node_count() as u64))?,
        )?;
    }
    {
        let d = Arc::clone(&doc);
        p.set(
            "set_size",
            lua.create_function(move |_, (w, h): (f64, f64)| {
                let mut doc = d.lock().unwrap();
                doc.width = w;
                doc.height = h;
                Ok(())
            })?,
        )?;
    }

    // ── Shape creation ────────────────────────────────────────────────────────

    {
        let d = Arc::clone(&doc);
        let hist = Arc::clone(&history);
        p.set(
            "create_rect",
            lua.create_function(
                move |_, (x, y, w, h, opts): (f64, f64, f64, f64, Option<LuaTable>)| {
                    let path = PathData::rect(x, y, w, h);
                    add_shape(&d, &hist, path, "Rect", opts)
                },
            )?,
        )?;
    }
    {
        let d = Arc::clone(&doc);
        let hist = Arc::clone(&history);
        p.set(
            "create_ellipse",
            lua.create_function(
                move |_, (x, y, w, h, opts): (f64, f64, f64, f64, Option<LuaTable>)| {
                    let cx = x + w / 2.0;
                    let cy = y + h / 2.0;
                    let path = PathData::ellipse(cx, cy, w / 2.0, h / 2.0);
                    add_shape(&d, &hist, path, "Ellipse", opts)
                },
            )?,
        )?;
    }
    {
        let d = Arc::clone(&doc);
        let hist = Arc::clone(&history);
        p.set(
            "create_circle",
            lua.create_function(
                move |_, (cx, cy, r, opts): (f64, f64, f64, Option<LuaTable>)| {
                    let path = PathData::ellipse(cx, cy, r, r);
                    add_shape(&d, &hist, path, "Circle", opts)
                },
            )?,
        )?;
    }
    {
        let d = Arc::clone(&doc);
        let hist = Arc::clone(&history);
        p.set(
            "create_polygon",
            lua.create_function(
                move |_,
                      (cx, cy, radius, sides, opts): (
                    f64,
                    f64,
                    f64,
                    Option<usize>,
                    Option<LuaTable>,
                )| {
                    let sides = sides.unwrap_or(6).max(3);
                    let path = PathData::regular_polygon(cx, cy, radius, sides);
                    add_shape(&d, &hist, path, "Polygon", opts)
                },
            )?,
        )?;
    }
    {
        let d = Arc::clone(&doc);
        let hist = Arc::clone(&history);
        p.set(
            "create_star",
            lua.create_function(
                move |_,
                      (cx, cy, outer_r, inner_r, points, opts): (
                    f64,
                    f64,
                    f64,
                    f64,
                    Option<usize>,
                    Option<LuaTable>,
                )| {
                    let points = points.unwrap_or(5).max(3);
                    let path = PathData::star(cx, cy, outer_r, inner_r, points);
                    add_shape(&d, &hist, path, "Star", opts)
                },
            )?,
        )?;
    }
    {
        let d = Arc::clone(&doc);
        let hist = Arc::clone(&history);
        p.set(
            "create_path",
            lua.create_function(move |_, (svg, opts): (String, Option<LuaTable>)| {
                match PathData::from_svg(&svg) {
                    Ok(path) => add_shape(&d, &hist, path, "Path", opts),
                    Err(e) => Err(LuaError::external(format!("Invalid SVG path: {}", e))),
                }
            })?,
        )?;
    }

    // ── Node operations ───────────────────────────────────────────────────────

    {
        let d = Arc::clone(&doc);
        let hist = Arc::clone(&history);
        p.set(
            "delete",
            lua.create_function(move |_, id: String| {
                if let Ok(uuid) = id.parse::<uuid::Uuid>() {
                    let mut doc = d.lock().unwrap();
                    let mut h = hist.lock().unwrap();
                    h.execute_discrete(Command::RemoveNode { node_id: uuid }, &mut doc);
                }
                Ok(())
            })?,
        )?;
    }
    {
        let d = Arc::clone(&doc);
        let hist = Arc::clone(&history);
        p.set(
            "clear",
            lua.create_function(move |_, ()| {
                let mut doc = d.lock().unwrap();
                let ids: Vec<_> = doc.nodes.keys().copied().collect();
                let cmds: Vec<Command> = ids
                    .iter()
                    .map(|id| Command::RemoveNode { node_id: *id })
                    .collect();
                if !cmds.is_empty() {
                    hist.lock()
                        .unwrap()
                        .execute_discrete(Command::Batch(cmds), &mut doc);
                }
                Ok(())
            })?,
        )?;
    }
    {
        let d = Arc::clone(&doc);
        p.set(
            "list_nodes",
            lua.create_function(move |lua, ()| {
                let doc = d.lock().unwrap();
                let t = lua.create_table()?;
                for (i, node) in doc.nodes_in_draw_order().iter().enumerate() {
                    let nt = lua.create_table()?;
                    nt.set("id", node.id.to_string())?;
                    nt.set("name", node.name.clone())?;
                    nt.set("visible", node.visible)?;
                    nt.set("opacity", node.opacity as f64)?;
                    t.set(i + 1, nt)?;
                }
                Ok(t)
            })?,
        )?;
    }

    // ── Boolean path operations ───────────────────────────────────────────────

    {
        let d = Arc::clone(&doc);
        let hist = Arc::clone(&history);
        p.set(
            "boolean",
            lua.create_function(move |_, (id1, id2, op_str): (String, String, String)| {
                let uuid_a = id1.parse::<uuid::Uuid>().map_err(LuaError::external)?;
                let uuid_b = id2.parse::<uuid::Uuid>().map_err(LuaError::external)?;

                let (path_a, path_b, fill) =
                    {
                        let doc = d.lock().unwrap();
                        let node_a = doc.nodes.get(&uuid_a).ok_or_else(|| {
                            LuaError::external(format!("Node not found: {}", id1))
                        })?;
                        let node_b = doc.nodes.get(&uuid_b).ok_or_else(|| {
                            LuaError::external(format!("Node not found: {}", id2))
                        })?;

                        let path_a = match &node_a.kind {
                            SceneNodeKind::Path(p) => p.path_data.clone(),
                            _ => return Err(LuaError::external("First node is not a path")),
                        };
                        let path_b = match &node_b.kind {
                            SceneNodeKind::Path(p) => p.path_data.clone(),
                            _ => return Err(LuaError::external("Second node is not a path")),
                        };
                        let fill = match &node_a.kind {
                            SceneNodeKind::Path(p) => p.fill.clone(),
                            _ => Fill::solid(Color::new(0.2, 0.47, 0.87, 1.0)),
                        };
                        (path_a, path_b, fill)
                    };

                let bool_op = match op_str.to_lowercase().as_str() {
                    "union" => BooleanOp::Union,
                    "intersect" => BooleanOp::Intersect,
                    "subtract" => BooleanOp::Subtract,
                    "exclude" => BooleanOp::Exclude,
                    other => {
                        return Err(LuaError::external(format!("Unknown boolean op: {}", other)))
                    }
                };

                let result_path =
                    run_boolean_op(&path_a, &path_b, bool_op).map_err(LuaError::external)?;

                let kind = SceneNodeKind::Path(PathNode::new(result_path).with_fill(fill));
                let mut doc = d.lock().unwrap();
                let node_num = doc.node_count() + 1;
                let node =
                    SceneNode::new(format!("Boolean {}", node_num), Default::default(), kind);
                let node_id = node.id;
                hist.lock().unwrap().execute_discrete(
                    Command::AddNode {
                        node,
                        layer_id: None,
                    },
                    &mut doc,
                );
                Ok(node_id.to_string())
            })?,
        )?;
    }

    // ── Rendering ─────────────────────────────────────────────────────────────

    {
        let d = Arc::clone(&doc);
        let r = Arc::clone(&renderer);
        let dir = script_dir.clone();
        p.set(
            "save",
            lua.create_function(move |_, filename: String| {
                let path = if std::path::Path::new(&filename).is_absolute() {
                    std::path::PathBuf::from(&filename)
                } else {
                    dir.join(&filename)
                };

                let doc = d.lock().unwrap();
                let png = r.render_png(&doc);
                if png.is_empty() {
                    return Err(LuaError::external("Render failed (empty output)"));
                }
                std::fs::write(&path, &png).map_err(LuaError::external)?;
                println!("  → saved {} ({} bytes)", path.display(), png.len());
                Ok(())
            })?,
        )?;
    }

    // ── Utilities ─────────────────────────────────────────────────────────────

    p.set(
        "print",
        lua.create_function(|_, msg: String| {
            println!("{}", msg);
            Ok(())
        })?,
    )?;

    p.set(
        "sleep_ms",
        lua.create_function(|_, ms: u64| {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            Ok(())
        })?,
    )?;

    // ── Color utilities (photonic.color.*) ────────────────────────────────────

    let color = lua.create_table()?;

    color.set(
        "hex",
        lua.create_function(|_, s: String| {
            Color::from_hex(&s)
                .map(|c| c.to_hex())
                .ok_or_else(|| LuaError::external(format!("Invalid hex color: {}", s)))
        })?,
    )?;

    color.set(
        "rgb",
        lua.create_function(|_, (r, g, b): (u8, u8, u8)| {
            Ok(format!("#{:02X}{:02X}{:02X}", r, g, b))
        })?,
    )?;

    color.set(
        "rgba",
        lua.create_function(|_, (r, g, b, a): (u8, u8, u8, u8)| {
            Ok(format!("#{:02X}{:02X}{:02X}{:02X}", r, g, b, a))
        })?,
    )?;

    color.set(
        "rgbf",
        lua.create_function(|_, (r, g, b): (f64, f64, f64)| {
            let r = (r.clamp(0.0, 1.0) * 255.0) as u8;
            let g = (g.clamp(0.0, 1.0) * 255.0) as u8;
            let b = (b.clamp(0.0, 1.0) * 255.0) as u8;
            Ok(format!("#{:02X}{:02X}{:02X}", r, g, b))
        })?,
    )?;

    color.set(
        "hsv",
        lua.create_function(|_, (h, s, v): (f64, f64, f64)| {
            let (r, g, b) = hsv_to_rgb(h, s, v);
            Ok(format!("#{:02X}{:02X}{:02X}", r, g, b))
        })?,
    )?;

    color.set(
        "hsl",
        lua.create_function(|_, (h, s, l): (f64, f64, f64)| {
            let (r, g, b) = hsl_to_rgb(h, s, l);
            Ok(format!("#{:02X}{:02X}{:02X}", r, g, b))
        })?,
    )?;

    color.set(
        "lerp",
        lua.create_function(|_, (c1, c2, t): (String, String, f64)| {
            let a = Color::from_hex(&c1)
                .ok_or_else(|| LuaError::external(format!("Invalid color: {}", c1)))?;
            let b = Color::from_hex(&c2)
                .ok_or_else(|| LuaError::external(format!("Invalid color: {}", c2)))?;
            let t = t.clamp(0.0, 1.0) as f32;
            let c = Color {
                r: a.r + (b.r - a.r) * t,
                g: a.g + (b.g - a.g) * t,
                b: a.b + (b.b - a.b) * t,
                a: a.a + (b.a - a.a) * t,
            };
            Ok(c.to_hex())
        })?,
    )?;

    p.set("color", color)?;

    // ── Math extras ───────────────────────────────────────────────────────────

    let math_extra = lua.create_table()?;
    math_extra.set("TAU", std::f64::consts::TAU)?;
    math_extra.set("PI", std::f64::consts::PI)?;
    p.set("math", math_extra)?;

    lua.globals().set("photonic", p)?;
    Ok(())
}

// ─── Helper: add a shape node to the document ─────────────────────────────────

fn add_shape(
    doc: &Arc<Mutex<Document>>,
    hist: &Arc<Mutex<CommandHistory>>,
    path: PathData,
    default_name: &str,
    opts: Option<LuaTable>,
) -> LuaResult<String> {
    let (fill_color, name, opacity, stroke_color, stroke_width) = if let Some(opts) = opts {
        let fill_str: Option<String> = opts.get("fill")?;
        let name: Option<String> = opts.get("name")?;
        let opacity: Option<f64> = opts.get("opacity")?;
        let stroke_str: Option<String> = opts.get("stroke")?;
        let stroke_w: Option<f64> = opts.get("stroke_width")?;
        (fill_str, name, opacity, stroke_str, stroke_w)
    } else {
        (None, None, None, None, None)
    };

    let color = fill_color
        .as_deref()
        .and_then(Color::from_hex)
        .unwrap_or(Color::new(0.2, 0.47, 0.87, 1.0));

    let fill = Fill::solid(color);

    let stroke = if let Some(sc_str) = stroke_color {
        if let Some(sc) = Color::from_hex(&sc_str) {
            Stroke::solid(sc, stroke_width.unwrap_or(1.0))
        } else {
            Stroke::none()
        }
    } else {
        Stroke::none()
    };

    let kind = SceneNodeKind::Path(PathNode::new(path).with_fill(fill).with_stroke(stroke));

    let mut doc = doc.lock().unwrap();
    let node_num = doc.node_count() + 1;
    let node_name = name.unwrap_or_else(|| format!("{} {}", default_name, node_num));
    let mut node = SceneNode::new(node_name, Default::default(), kind);
    if let Some(op) = opacity {
        node.opacity = op.clamp(0.0, 1.0) as f32;
    }
    let node_id = node.id;
    hist.lock().unwrap().execute_discrete(
        Command::AddNode {
            node,
            layer_id: None,
        },
        &mut doc,
    );
    Ok(node_id.to_string())
}

// ─── Color math ──────────────────────────────────────────────────────────────

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (u8, u8, u8) {
    let h = h.fract().abs();
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);

    if s == 0.0 {
        let c = (v * 255.0) as u8;
        return (c, c, c);
    }

    let i = (h * 6.0) as u32;
    let f = h * 6.0 - i as f64;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);

    let (r, g, b) = match i % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let h = h.fract().abs();
    let s = s.clamp(0.0, 1.0);
    let l = l.clamp(0.0, 1.0);

    let a = s * l.min(1.0 - l);
    let f = |n: f64| -> f64 {
        let k = (n + h * 12.0) % 12.0;
        l - a * (k - 3.0).min(9.0 - k).min(1.0).max(-1.0)
    };
    (
        (f(0.0) * 255.0) as u8,
        (f(8.0) * 255.0) as u8,
        (f(4.0) * 255.0) as u8,
    )
}
