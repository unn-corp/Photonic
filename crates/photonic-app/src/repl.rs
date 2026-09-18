//! Persistent Lua REPL that operates on the live document.
//!
//! Unlike `script.rs` (which creates a throwaway Lua VM per script), `LuaRepl`
//! keeps state across evaluations and writes to the same
//! `Arc<tokio::sync::Mutex<Document>>` that the renderer uses.

use anyhow::Result;
use mlua::prelude::*;
use photonic_core::{
    history::{Command, CommandHistory},
    node::PathNode,
    ops::boolean::{boolean_op as run_boolean_op, BooleanOp},
    style::Stroke,
    Color, Document, Fill, PathData, SceneNode, SceneNodeKind,
};
use photonic_render::HeadlessRenderer;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex;

// ─── Public REPL handle ───────────────────────────────────────────────────────

pub struct LuaRepl {
    lua: Lua,
    /// Captured stdout from Lua `print()` calls.
    output: Arc<StdMutex<Vec<String>>>,
}

impl LuaRepl {
    /// Create a new REPL bound to the running document and shared history.
    /// This blocks to initialise a headless wgpu device for `save()` support.
    pub fn new(doc: Arc<Mutex<Document>>, history: Arc<Mutex<CommandHistory>>) -> Result<Self> {
        let output: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));

        let lua = Lua::new();

        // Override global print → capture output
        {
            let out = Arc::clone(&output);
            let print_fn = lua
                .create_function(move |_, args: LuaMultiValue| {
                    let line = args
                        .iter()
                        .map(lua_val_to_string)
                        .collect::<Vec<_>>()
                        .join("\t");
                    out.lock().unwrap().push(line);
                    Ok(())
                })
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            lua.globals()
                .set("print", print_fn)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }

        // Headless renderer for photonic.save()
        let renderer = Arc::new(pollster::block_on(HeadlessRenderer::new()));
        let script_dir = std::env::current_dir().unwrap_or_default();

        register_live_api(&lua, doc, history, renderer, script_dir)
            .map_err(|e| anyhow::anyhow!("REPL API init: {e}"))?;

        Ok(Self { lua, output })
    }

    /// Fallback no-op REPL used when initialisation fails.
    pub fn new_empty() -> Self {
        let lua = Lua::new();
        let output = Arc::new(StdMutex::new(Vec::new()));
        Self { lua, output }
    }

    /// Evaluate a snippet. Returns (captured_print_lines, optional_error_message).
    pub fn eval(&self, code: &str) -> (Vec<String>, Option<String>) {
        self.output.lock().unwrap().clear();
        let err = self.lua.load(code).exec().err().map(|e| format!("{e}"));
        let lines = self.output.lock().unwrap().clone();
        (lines, err)
    }
}

// ─── Live API registration ────────────────────────────────────────────────────
//
// Mirrors script.rs `register_api` but uses `Arc<tokio::sync::Mutex<Document>>`
// with `blocking_lock()` (safe on the non-async main thread).

fn register_live_api(
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
            lua.create_function(move |_, ()| Ok(d.blocking_lock().width))?,
        )?;
    }
    {
        let d = Arc::clone(&doc);
        p.set(
            "height",
            lua.create_function(move |_, ()| Ok(d.blocking_lock().height))?,
        )?;
    }
    {
        let d = Arc::clone(&doc);
        p.set(
            "name",
            lua.create_function(move |_, ()| Ok(d.blocking_lock().name.clone()))?,
        )?;
    }
    {
        let d = Arc::clone(&doc);
        p.set(
            "node_count",
            lua.create_function(move |_, ()| Ok(d.blocking_lock().node_count() as u64))?,
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
                    add_live_shape(&d, &hist, PathData::rect(x, y, w, h), "Rect", opts)
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
                    add_live_shape(
                        &d,
                        &hist,
                        PathData::ellipse(x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0),
                        "Ellipse",
                        opts,
                    )
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
                    add_live_shape(&d, &hist, PathData::ellipse(cx, cy, r, r), "Circle", opts)
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
                      (cx, cy, r, sides, opts): (
                    f64,
                    f64,
                    f64,
                    Option<usize>,
                    Option<LuaTable>,
                )| {
                    add_live_shape(
                        &d,
                        &hist,
                        PathData::regular_polygon(cx, cy, r, sides.unwrap_or(6).max(3)),
                        "Polygon",
                        opts,
                    )
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
                      (cx, cy, outer, inner, pts, opts): (
                    f64,
                    f64,
                    f64,
                    f64,
                    Option<usize>,
                    Option<LuaTable>,
                )| {
                    add_live_shape(
                        &d,
                        &hist,
                        PathData::star(cx, cy, outer, inner, pts.unwrap_or(5).max(3)),
                        "Star",
                        opts,
                    )
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
                    Ok(path) => add_live_shape(&d, &hist, path, "Path", opts),
                    Err(e) => Err(LuaError::external(format!("Invalid SVG path: {e}"))),
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
                    let mut doc = d.blocking_lock();
                    let mut h = hist.blocking_lock();
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
                let mut doc = d.blocking_lock();
                let ids: Vec<_> = doc.nodes.keys().copied().collect();
                let cmds: Vec<Command> = ids
                    .iter()
                    .map(|id| Command::RemoveNode { node_id: *id })
                    .collect();
                if !cmds.is_empty() {
                    hist.blocking_lock()
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
                let doc = d.blocking_lock();
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

    // ── Boolean ops ───────────────────────────────────────────────────────────
    {
        let d = Arc::clone(&doc);
        let hist = Arc::clone(&history);
        p.set(
            "boolean",
            lua.create_function(move |_, (id1, id2, op_str): (String, String, String)| {
                let uuid_a = id1.parse::<uuid::Uuid>().map_err(LuaError::external)?;
                let uuid_b = id2.parse::<uuid::Uuid>().map_err(LuaError::external)?;
                let (path_a, path_b, fill) = {
                    let doc = d.blocking_lock();
                    let na = doc
                        .nodes
                        .get(&uuid_a)
                        .ok_or_else(|| LuaError::external(format!("Node not found: {id1}")))?;
                    let nb = doc
                        .nodes
                        .get(&uuid_b)
                        .ok_or_else(|| LuaError::external(format!("Node not found: {id2}")))?;
                    let pa = match &na.kind {
                        SceneNodeKind::Path(p) => p.path_data.clone(),
                        _ => return Err(LuaError::external("not a path")),
                    };
                    let pb = match &nb.kind {
                        SceneNodeKind::Path(p) => p.path_data.clone(),
                        _ => return Err(LuaError::external("not a path")),
                    };
                    let fi = match &na.kind {
                        SceneNodeKind::Path(p) => p.fill.clone(),
                        _ => Fill::solid(Color::new(0.2, 0.47, 0.87, 1.0)),
                    };
                    (pa, pb, fi)
                };
                let op = match op_str.to_lowercase().as_str() {
                    "union" => BooleanOp::Union,
                    "intersect" => BooleanOp::Intersect,
                    "subtract" => BooleanOp::Subtract,
                    "exclude" => BooleanOp::Exclude,
                    other => return Err(LuaError::external(format!("Unknown op: {other}"))),
                };
                let result = run_boolean_op(&path_a, &path_b, op).map_err(LuaError::external)?;
                let kind = SceneNodeKind::Path(PathNode::new(result).with_fill(fill));
                let mut doc = d.blocking_lock();
                let n = doc.node_count() + 1;
                let node = SceneNode::new(format!("Boolean {n}"), Default::default(), kind);
                let node_id = node.id;
                hist.blocking_lock().execute_discrete(
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
        let dir = script_dir;
        p.set(
            "save",
            lua.create_function(move |_, filename: String| {
                let path = if std::path::Path::new(&filename).is_absolute() {
                    std::path::PathBuf::from(&filename)
                } else {
                    dir.join(&filename)
                };
                let doc = d.blocking_lock();
                let png = r.render_png(&doc);
                if png.is_empty() {
                    return Err(LuaError::external("Render failed"));
                }
                std::fs::write(&path, &png).map_err(LuaError::external)?;
                println!("  → saved {} ({} bytes)", path.display(), png.len());
                Ok(())
            })?,
        )?;
    }

    // ── Color utilities ───────────────────────────────────────────────────────
    let color = lua.create_table()?;
    color.set(
        "hex",
        lua.create_function(|_, s: String| {
            Color::from_hex(&s)
                .map(|c| c.to_hex())
                .ok_or_else(|| LuaError::external(format!("Invalid hex: {s}")))
        })?,
    )?;
    color.set(
        "rgb",
        lua.create_function(|_, (r, g, b): (u8, u8, u8)| Ok(format!("#{r:02X}{g:02X}{b:02X}")))?,
    )?;
    color.set(
        "rgbf",
        lua.create_function(|_, (r, g, b): (f64, f64, f64)| {
            let (r, g, b) = (
                (r.clamp(0., 1.) * 255.) as u8,
                (g.clamp(0., 1.) * 255.) as u8,
                (b.clamp(0., 1.) * 255.) as u8,
            );
            Ok(format!("#{r:02X}{g:02X}{b:02X}"))
        })?,
    )?;
    color.set(
        "hsv",
        lua.create_function(|_, (h, s, v): (f64, f64, f64)| {
            let (r, g, b) = hsv_to_rgb(h, s, v);
            Ok(format!("#{r:02X}{g:02X}{b:02X}"))
        })?,
    )?;
    color.set(
        "hsl",
        lua.create_function(|_, (h, s, l): (f64, f64, f64)| {
            let (r, g, b) = hsl_to_rgb(h, s, l);
            Ok(format!("#{r:02X}{g:02X}{b:02X}"))
        })?,
    )?;
    p.set("color", color)?;

    let math_extra = lua.create_table()?;
    math_extra.set("TAU", std::f64::consts::TAU)?;
    math_extra.set("PI", std::f64::consts::PI)?;
    p.set("math", math_extra)?;

    lua.globals().set("photonic", p)?;
    Ok(())
}

// ─── Helper: add a shape to the live document ─────────────────────────────────

fn add_live_shape(
    doc: &Arc<Mutex<Document>>,
    hist: &Arc<Mutex<CommandHistory>>,
    path: PathData,
    default_name: &str,
    opts: Option<LuaTable>,
) -> LuaResult<String> {
    let (fill_str, name, opacity, stroke_str, stroke_w) = if let Some(opts) = opts {
        (
            opts.get::<Option<String>>("fill")?,
            opts.get::<Option<String>>("name")?,
            opts.get::<Option<f64>>("opacity")?,
            opts.get::<Option<String>>("stroke")?,
            opts.get::<Option<f64>>("stroke_width")?,
        )
    } else {
        (None, None, None, None, None)
    };

    let color = fill_str
        .as_deref()
        .and_then(Color::from_hex)
        .unwrap_or(Color::new(0.2, 0.47, 0.87, 1.0));
    let fill = Fill::solid(color);
    let stroke = if let Some(sc) = stroke_str.as_deref().and_then(Color::from_hex) {
        Stroke::solid(sc, stroke_w.unwrap_or(1.0))
    } else {
        Stroke::none()
    };

    let kind = SceneNodeKind::Path(PathNode::new(path).with_fill(fill).with_stroke(stroke));

    let mut doc = doc.blocking_lock();
    let num = doc.node_count() + 1;
    let node_name = name.unwrap_or_else(|| format!("{default_name} {num}"));
    let mut node = SceneNode::new(node_name, Default::default(), kind);
    if let Some(op) = opacity {
        node.opacity = op.clamp(0.0, 1.0) as f32;
    }
    let node_id = node.id;
    hist.blocking_lock().execute_discrete(
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
    let s = s.clamp(0., 1.);
    let v = v.clamp(0., 1.);
    if s == 0.0 {
        let c = (v * 255.) as u8;
        return (c, c, c);
    }
    let i = (h * 6.) as u32;
    let f = h * 6. - i as f64;
    let (p, q, t) = (v * (1. - s), v * (1. - f * s), v * (1. - (1. - f) * s));
    let (r, g, b) = match i % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    ((r * 255.) as u8, (g * 255.) as u8, (b * 255.) as u8)
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let h = h.fract().abs();
    let s = s.clamp(0., 1.);
    let l = l.clamp(0., 1.);
    let a = s * l.min(1. - l);
    let f = |n: f64| -> f64 {
        let k = (n + h * 12.) % 12.;
        l - a * (k - 3.).min(9. - k).min(1.).max(-1.)
    };
    (
        (f(0.) * 255.) as u8,
        (f(8.) * 255.) as u8,
        (f(4.) * 255.) as u8,
    )
}

fn lua_val_to_string(v: &LuaValue) -> String {
    match v {
        LuaValue::Nil => "nil".into(),
        LuaValue::Boolean(b) => b.to_string(),
        LuaValue::Integer(i) => i.to_string(),
        LuaValue::Number(n) => n.to_string(),
        LuaValue::String(s) => s
            .to_str()
            .map(|b| b.to_string())
            .unwrap_or_else(|_| "?".to_string()),
        other => format!("{other:?}"),
    }
}
