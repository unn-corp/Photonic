//! Global search catalog + matching for the app's command palette.
//!
//! Searchable items are the editor tools plus a handful of commands. The UI
//! (in `app.rs`) ranks **direct** matches (title contains the query) first, then
//! **semantic** matches — items whose keywords or a fuzzy subsequence of the
//! title relate to the query — shown under a "Related" header.

use crate::Tool;
use egui_phosphor::regular as ph;

/// What activating a search result does.
#[derive(Clone, Copy)]
pub enum SearchAction {
    Tool(Tool),
    ToggleGrid,
    ToggleGuides,
    ToggleAudit,
    FileMenu,
    EditMenu,
    ToolsMenu,
    Undo,
    Redo,
    FitView,
    OutlineMode,
    PixelPreview,
    OverprintPreview,
    CheckUpdates,
}

pub struct SearchItem {
    pub title: String,
    pub icon: &'static str,
    /// One-line description — shown in results and fed into semantic matching.
    pub description: String,
    pub keywords: &'static [&'static str],
    pub action: SearchAction,
}

const TOOLS: &[Tool] = &[
    Tool::Select,
    Tool::DirectSelect,
    Tool::ProportionalMove,
    Tool::Pan,
    Tool::Rectangle,
    Tool::RoundedRect,
    Tool::Ellipse,
    Tool::Polygon,
    Tool::Star,
    Tool::Spiral,
    Tool::Line,
    Tool::Arc,
    Tool::Grid,
    Tool::PolarGrid,
    Tool::Pen,
    Tool::ShapeBuilder,
    Tool::Text,
    Tool::Scissors,
    Tool::Knife,
    Tool::Eraser,
    Tool::MagicWand,
    Tool::Lasso,
    Tool::Pencil,
    Tool::Smooth,
    Tool::Width,
    Tool::AreaTrace,
    Tool::RasterBrush,
    Tool::RasterEraser,
];

fn tool_keywords(t: Tool) -> &'static [&'static str] {
    match t {
        Tool::Select => &["move", "arrow", "pointer"],
        Tool::DirectSelect => &["node", "anchor", "point", "edit"],
        Tool::ProportionalMove => &[
            "proportional",
            "falloff",
            "soft",
            "magnet",
            "spread",
            "blender",
        ],
        Tool::Pan => &["hand", "scroll", "navigate"],
        Tool::Rectangle => &["square", "box", "rect"],
        Tool::RoundedRect => &["rounded", "corner", "square"],
        Tool::Ellipse => &["circle", "oval", "round", "dot"],
        Tool::Polygon => &["hexagon", "pentagon", "sides", "shape"],
        Tool::Star => &["burst", "sparkle", "points"],
        Tool::Spiral => &["swirl", "coil", "helix"],
        Tool::Line => &["segment", "stroke", "straight"],
        Tool::Arc => &["curve", "semicircle"],
        Tool::Grid => &["table", "mesh", "rows", "columns"],
        Tool::PolarGrid => &["radial", "circular", "concentric"],
        Tool::Pen => &["bezier", "path", "curve", "draw"],
        Tool::ShapeBuilder => &["merge", "combine", "boolean", "union"],
        Tool::Text => &["type", "label", "font", "write"],
        Tool::Scissors => &["cut", "split", "snip"],
        Tool::Knife => &["slice", "cut", "divide", "freehand cut"],
        Tool::Eraser => &["erase", "subtract", "rubber", "vector erase"],
        Tool::MagicWand => &["select color", "wand", "fill select"],
        Tool::Lasso => &["freehand select", "loop"],
        Tool::Pencil => &["freehand", "sketch", "draw"],
        Tool::Smooth => &["simplify", "relax", "clean"],
        Tool::Width => &[
            "width",
            "variable width",
            "stroke width",
            "taper",
            "profile",
        ],
        Tool::RasterBrush => &["paint", "pixel", "draw", "raster"],
        Tool::RasterEraser => &["erase", "rubber", "raster", "delete pixels"],
        Tool::AreaTrace => &[
            "image trace",
            "vectorize",
            "bitmap to vector",
            "raster trace",
            "convert image",
        ],
    }
}

/// Build the searchable catalog (tools + commands).
pub fn items() -> Vec<SearchItem> {
    let mut v: Vec<SearchItem> = TOOLS
        .iter()
        .map(|&t| SearchItem {
            title: t.label().to_string(),
            icon: t.icon(),
            description: t.description().to_string(),
            keywords: tool_keywords(t),
            action: SearchAction::Tool(t),
        })
        .collect();

    let cmd = |title: &str,
               icon: &'static str,
               description: &str,
               keywords: &'static [&'static str],
               action: SearchAction| SearchItem {
        title: title.to_string(),
        icon,
        description: description.to_string(),
        keywords,
        action,
    };
    v.extend([
        cmd(
            "Toggle Grid",
            ph::GRID_FOUR,
            "Show or hide the canvas grid overlay",
            &["snap", "layout", "squares"],
            SearchAction::ToggleGrid,
        ),
        cmd(
            "Toggle Guides",
            ph::RULER,
            "Show or hide ruler guides on the canvas",
            &["rulers", "alignment", "lines"],
            SearchAction::ToggleGuides,
        ),
        cmd(
            "Audit Log",
            ph::MAGNIFYING_GLASS,
            "Open the MCP audit log of recent AI tool calls",
            &["mcp", "history", "tool calls", "log"],
            SearchAction::ToggleAudit,
        ),
        cmd(
            "File Menu",
            ph::FILE,
            "New, open, save, and export documents",
            &["new", "open", "save", "export"],
            SearchAction::FileMenu,
        ),
        cmd(
            "Preferences",
            ph::GEAR,
            "Edit application settings, theme, and options",
            &["settings", "edit", "options", "theme"],
            SearchAction::EditMenu,
        ),
        cmd(
            "Tools Menu",
            ph::SQUARES_FOUR,
            "Browse all tools and pin them to the sidebar",
            &["pin", "toolbar", "all tools"],
            SearchAction::ToolsMenu,
        ),
        cmd(
            "Undo",
            ph::ARROW_COUNTER_CLOCKWISE,
            "Undo the last action",
            &["back", "revert", "ctrl z"],
            SearchAction::Undo,
        ),
        cmd(
            "Redo",
            ph::ARROW_CLOCKWISE,
            "Redo the last undone action",
            &["forward", "ctrl y"],
            SearchAction::Redo,
        ),
        cmd(
            "Fit to View",
            ph::FRAME_CORNERS,
            "Zoom and center the artboards in the viewport",
            &["zoom", "frame", "center", "fit artboard"],
            SearchAction::FitView,
        ),
        cmd(
            "Outline Mode",
            ph::SQUARES_FOUR,
            "Toggle wireframe outline preview of all shapes",
            &["wireframe", "skeleton", "preview"],
            SearchAction::OutlineMode,
        ),
        cmd(
            "Pixel Preview",
            ph::GRID_NINE,
            "Preview the active artboard at export pixel size with nearest-neighbour sampling to show true aliasing and pixel snapping",
            &["aliasing", "pixels", "raster", "export preview", "nearest"],
            SearchAction::PixelPreview,
        ),
        cmd(
            "Overprint Preview",
            ph::DROP,
            "Simulate overprint: solid fills matching an overprint spot ink multiply into the backdrop instead of knocking out",
            &["print", "spot color", "ink", "multiply", "knockout", "prepress"],
            SearchAction::OverprintPreview,
        ),
        cmd(
            "Check for Updates",
            ph::ARROW_CLOCKWISE,
            "Download and install the latest Photonic release",
            &["upgrade", "version", "download", "new"],
            SearchAction::CheckUpdates,
        ),
    ]);
    v
}

/// The text embedded for an item's semantic vector: title + description.
pub fn corpus_text(it: &SearchItem) -> String {
    format!("{}. {}", it.title, it.description)
}

// ─── On-device semantic index (background embedder) ─────────────────────────────

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// Background semantic search over the (fixed) catalog using a local embedding
/// model. The worker loads the model (downloading once), embeds the corpus, then
/// answers queries with cosine-ranked `(item index, score)` results. Indices map
/// into `items()` (same deterministic order). Falls back silently to nothing if
/// the model can't load — the UI then uses keyword/fuzzy matching.
pub struct SemanticIndex {
    req_tx: Sender<String>,
    res_rx: Receiver<Vec<(usize, f32)>>,
    ready: Arc<AtomicBool>,
    last_query: String,
    pub results: Vec<(usize, f32)>,
}

impl SemanticIndex {
    pub fn new(corpus: Vec<String>) -> Self {
        let (req_tx, req_rx) = channel::<String>();
        let (res_tx, res_rx) = channel::<Vec<(usize, f32)>>();
        let ready = Arc::new(AtomicBool::new(false));
        let ready_w = Arc::clone(&ready);
        std::thread::Builder::new()
            .name("photonic-embed".into())
            .spawn(move || {
                let embedder = match photonic_embed::Embedder::new() {
                    Ok(e) => e,
                    Err(_) => return, // unavailable → UI uses keyword fallback
                };
                let corpus_vecs = match embedder.embed(&corpus) {
                    Ok(v) => v,
                    Err(_) => return,
                };
                ready_w.store(true, Ordering::SeqCst);
                while let Ok(mut query) = req_rx.recv() {
                    // Coalesce to the most recent pending query.
                    while let Ok(newer) = req_rx.try_recv() {
                        query = newer;
                    }
                    let q = query.trim();
                    if q.is_empty() {
                        let _ = res_tx.send(Vec::new());
                        continue;
                    }
                    if let Ok(qv) = embedder.embed(&[q.to_string()]) {
                        let mut scored: Vec<(usize, f32)> = corpus_vecs
                            .iter()
                            .enumerate()
                            .map(|(i, v)| (i, photonic_embed::cosine(&qv[0], v)))
                            .collect();
                        scored.sort_by(|a, b| {
                            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                        });
                        let _ = res_tx.send(scored);
                    }
                }
            })
            .ok();
        Self {
            req_tx,
            res_rx,
            ready,
            last_query: String::new(),
            results: Vec::new(),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    /// Submit a query (only re-sends when it changed).
    pub fn set_query(&mut self, q: &str) {
        if q != self.last_query {
            self.last_query = q.to_string();
            if q.trim().is_empty() {
                self.results.clear();
            }
            let _ = self.req_tx.send(q.to_string());
        }
    }

    /// Drain the latest ranked results.
    pub fn pump(&mut self) {
        while let Ok(res) = self.res_rx.try_recv() {
            self.results = res;
        }
    }
}

/// True if every (non-space) char of `q` appears in `s` in order.
pub fn fuzzy_subseq(q: &str, s: &str) -> bool {
    let mut chars = s.chars();
    for qc in q.chars().filter(|c| !c.is_whitespace()) {
        loop {
            match chars.next() {
                Some(c) if c == qc => break,
                Some(_) => continue,
                None => return false,
            }
        }
    }
    true
}
