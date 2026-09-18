//! One-off generator for the P1 golden-output fixture corpus
//! (`03-render-color-pipeline.md` §2.6, `tests/golden/`).
//!
//! Not run by CI or by `cargo test` — a checked-in developer tool in the same
//! spirit as `tools/gen-mcp-docs.py` (11-testing-phasing.md §2's convention:
//! "a checked-in script CI *consumes the output of* rather than *runs*").
//! Regenerate a fixture's `project.photon` (e.g. after a deliberate schema
//! change) with:
//!
//! ```sh
//! cargo run -p photonic-render --example gen_p1_golden_fixtures
//! ```
//!
//! This only (re)writes `project.photon` files. It never touches
//! `expected/reference.png` — that is the golden test harness's job
//! (`tests/golden_vector_equivalence.rs`, `PHOTONIC_BLESS_GOLDEN=1`).

use photonic_core::effects::{ColorOverlay, LayerEffect, StrokeEffect};
use photonic_core::layer::Layer;
use photonic_core::node::{GroupNode, PathNode, TextNode};
use photonic_core::ops::boolean::BooleanOp;
use photonic_core::raster::adjust::AdjustmentSpec;
use photonic_core::raster::mask::Mask;
use photonic_core::style::{
    ArrowheadStyle, FluidGradient, FluidGradientPoint, LineCap, LineJoin, MeshGradient, PatternFill,
};
use photonic_core::Document;
use photonic_core::{
    save_photon, BlendMode, Color, DropShadow, Feather, Fill, GaussianGlow, GlowEffect, Gradient,
    GradientStop, PathData, RasterImage, RasterNode, SceneNode, SceneNodeKind, Stroke, Symbol,
    Transform, WidthProfile,
};

fn main() {
    // `--example gen_p1_golden_fixtures video-fixtures` writes ONLY the two
    // acceptance-story vector fixtures into `photonic-video/tests/fixtures/`
    // (29 §3's fixture table, brief I task 6). It deliberately does NOT touch
    // the golden corpus, so a harness author can regenerate the `.photon`
    // timeline fixtures without churning the 31 blessed golden `project.photon`
    // files. Default (no arg) keeps the original golden-corpus behaviour.
    if std::env::args().nth(1).as_deref() == Some("video-fixtures") {
        write_video_fixtures();
        return;
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    for (name, doc) in fixtures() {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("expected")).expect("create case dir");
        let json = save_photon(&doc, None).expect("serialize fixture");
        std::fs::write(dir.join("project.photon"), json).expect("write project.photon");
        println!("wrote {}/project.photon", name);
    }
    println!(
        "\nNo expected/reference.png written (harness blesses those). Run:\n  \
         PHOTONIC_BLESS_GOLDEN=1 cargo test -p photonic-render --test golden_vector_equivalence -- --test-threads=1\n\
         then review `git diff --stat tests/golden/` before committing."
    );
}

/// Write the AS-1/AS-3 vector timeline fixtures next to the video test-media
/// corpus. See [`title_asset`] / [`title_doc_asset`] for the stable node-id
/// contract the acceptance-story scripts hardcode.
fn write_video_fixtures() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../photonic-video/tests/fixtures");
    for (name, doc) in [
        ("title_asset", title_asset()),
        ("title_doc_asset", title_doc_asset()),
    ] {
        let json = save_photon(&doc, None).expect("serialize video fixture");
        let path = dir.join(format!("{name}.photon"));
        std::fs::write(&path, json).expect("write .photon fixture");
        println!("wrote {}", path.display());
    }
    println!(
        "\nNote: node ids are hardcoded (deterministic); the top-level `layers`/\
         `nodes` map key ORDER may still vary between runs because `save_photon`\
         serialises `Document`'s HashMaps directly. That does not affect loading,\
         node resolution, or rendering — only exact byte order."
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Acceptance-story vector fixtures (29 §3 fixture table, brief I task 6)
// ═══════════════════════════════════════════════════════════════════════════

use photonic_core::node::NodeId;

/// Stable node id for `title_asset`'s single title node. AS-1 fades this clip
/// in via the *clip*'s `transform.opacity` (a timeline PropPath), but a stable
/// node id keeps the fixture diff-reproducible.
const TITLE_ASSET_NODE: u128 = 0x11A5_0000_0000_0000_0000_0000_0000_0001;
/// Stable node id for `title_doc_asset`'s headline node — AS-3 keyframes
/// `node.<TITLE_DOC_TITLE_NODE>.fill.color`. Recorded in the fixtures README.
const TITLE_DOC_TITLE_NODE: u128 = 0x11D0_0000_0000_0000_0000_0000_0000_0001;
/// Stable node id for `title_doc_asset`'s subtitle node.
const TITLE_DOC_SUBTITLE_NODE: u128 = 0x11D0_0000_0000_0000_0000_0000_0000_0002;

/// AS-1 title clip: a single text node exposing a solid fill so an opacity
/// fade + `fill.color` binding both have a real target. One renderable node,
/// named `title`, id [`TITLE_ASSET_NODE`].
fn title_asset() -> Document {
    let mut doc = Document::new("title-asset", 1920.0, 1080.0);
    let lid = doc.active_layer_id.unwrap();
    let mut text = TextNode::new("Photonic");
    text.font_size = 96.0;
    text.fill = Fill::solid(Color::new(1.0, 1.0, 1.0, 1.0));
    let mut node = SceneNode::new("title", lid, SceneNodeKind::Text(text));
    node.id = NodeId::from_u128(TITLE_ASSET_NODE);
    node.transform = Transform::translate(200.0, 480.0);
    doc.add_node(node, Some(lid));
    doc
}

/// AS-3 motion-graphics title document: two stably-named text nodes
/// (`title_line` = [`TITLE_DOC_TITLE_NODE`], `subtitle_line` =
/// [`TITLE_DOC_SUBTITLE_NODE`]) each carrying a solid fill, so the AS-3 script's
/// `node.<id>.fill.color` keyframe target resolves against a real colour.
fn title_doc_asset() -> Document {
    let mut doc = Document::new("title-doc-asset", 1920.0, 1080.0);
    let lid = doc.active_layer_id.unwrap();

    let mut title = TextNode::new("Motion");
    title.font_size = 120.0;
    title.fill = Fill::solid(Color::new(0.95, 0.2, 0.2, 1.0));
    let mut n = SceneNode::new("title_line", lid, SceneNodeKind::Text(title));
    n.id = NodeId::from_u128(TITLE_DOC_TITLE_NODE);
    n.transform = Transform::translate(200.0, 360.0);
    doc.add_node(n, Some(lid));

    let mut sub = TextNode::new("Graphics");
    sub.font_size = 64.0;
    sub.fill = Fill::solid(Color::new(0.15, 0.3, 0.8, 1.0));
    let mut n = SceneNode::new("subtitle_line", lid, SceneNodeKind::Text(sub));
    n.id = NodeId::from_u128(TITLE_DOC_SUBTITLE_NODE);
    n.transform = Transform::translate(200.0, 560.0);
    doc.add_node(n, Some(lid));

    doc
}

fn fixtures() -> Vec<(&'static str, Document)> {
    vec![
        ("paths_fills_basic", paths_fills_basic()),
        ("strokes_basic", strokes_basic()),
        ("gradient_linear", gradient_linear()),
        ("gradient_radial", gradient_radial()),
        ("blend_separable", blend_separable()),
        ("blend_nonseparable", blend_nonseparable()),
        ("text_basic", text_basic()),
        ("text_styled", text_styled()),
        ("raster_placement", raster_placement()),
        ("effect_stack_color_overlay_stroke", effect_stack()),
        (
            "boolean_groups",
            boolean_group("boolean-groups", BooleanOp::Union),
        ),
        // ── Batch 2 (corpus expansion, P1 architect-gate finding [1]) ──────────
        ("symbol_instances", symbol_instances()),
        (
            "boolean_subtract",
            boolean_group("boolean-subtract", BooleanOp::Subtract),
        ),
        (
            "boolean_intersect",
            boolean_group("boolean-intersect", BooleanOp::Intersect),
        ),
        (
            "boolean_exclude",
            boolean_group("boolean-exclude", BooleanOp::Exclude),
        ),
        ("variable_width_stroke", variable_width_stroke()),
        ("effect_glow_stack", effect_glow_stack()),
        ("effect_shadow_feather", effect_shadow_feather()),
        ("isolated_layer_opacity", isolated_layer_opacity()),
        ("isolated_layer_blend", isolated_layer_blend()),
        ("pattern_fill_grid", pattern_fill_grid()),
        ("gradient_mesh", gradient_mesh()),
        ("gradient_fluid", gradient_fluid()),
        ("text_on_path", text_on_path()),
        ("mixed_vector_raster_zorder", mixed_vector_raster_zorder()),
        ("dense_many_nodes", dense_many_nodes()),
        ("compound_path", compound_path()),
        ("nested_groups", nested_groups()),
        ("raster_mask", raster_mask()),
        ("adjustment_layer_raster", adjustment_layer_raster()),
        ("stroke_arrowheads", stroke_arrowheads()),
    ]
}

/// Two solid-fill paths (rect + ellipse), the baseline "just tessellate and
/// fill" case every other fixture builds on.
fn paths_fills_basic() -> Document {
    let mut doc = Document::new("paths-fills-basic", 120.0, 120.0);
    let rect = SceneNode::new(
        "rect",
        doc.active_layer_id.unwrap(),
        SceneNodeKind::Path(
            PathNode::new(PathData::rect(10.0, 10.0, 40.0, 40.0))
                .with_fill(Fill::solid(Color::new(0.85, 0.15, 0.15, 1.0))),
        ),
    );
    doc.add_node(rect, None);
    let ellipse = SceneNode::new(
        "ellipse",
        doc.active_layer_id.unwrap(),
        SceneNodeKind::Path(
            PathNode::new(PathData::ellipse(80.0, 45.0, 25.0, 20.0))
                .with_fill(Fill::solid(Color::new(0.15, 0.35, 0.85, 0.7))),
        ),
    );
    doc.add_node(ellipse, None);
    doc
}

/// Stroke-only paths (no fill) exercising cap/join/dash.
fn strokes_basic() -> Document {
    let mut doc = Document::new("strokes-basic", 120.0, 120.0);
    let mut round_stroke = Stroke::solid(Color::new(0.1, 0.6, 0.3, 1.0), 6.0);
    round_stroke.line_cap = LineCap::Round;
    round_stroke.line_join = LineJoin::Round;
    let line_a = SceneNode::new(
        "line-round",
        doc.active_layer_id.unwrap(),
        SceneNodeKind::Path(
            PathNode::new(PathData::line(10.0, 20.0, 100.0, 20.0))
                .with_fill(Fill::solid(Color::TRANSPARENT))
                .with_stroke(round_stroke),
        ),
    );
    doc.add_node(line_a, None);

    let mut dashed_stroke = Stroke::solid(Color::new(0.6, 0.2, 0.7, 1.0), 4.0);
    dashed_stroke.line_cap = LineCap::Square;
    dashed_stroke.dash_array = vec![8.0, 4.0];
    let line_b = SceneNode::new(
        "line-dashed",
        doc.active_layer_id.unwrap(),
        SceneNodeKind::Path(
            PathNode::new(PathData::line(10.0, 60.0, 100.0, 100.0))
                .with_fill(Fill::solid(Color::TRANSPARENT))
                .with_stroke(dashed_stroke),
        ),
    );
    doc.add_node(line_b, None);
    doc
}

fn gradient_linear() -> Document {
    let mut doc = Document::new("gradient-linear", 120.0, 120.0);
    let stops = vec![
        GradientStop::new(0.0, Color::new(1.0, 0.9, 0.2, 1.0)),
        GradientStop::new(0.5, Color::new(0.9, 0.3, 0.1, 1.0)),
        GradientStop::new(1.0, Color::new(0.4, 0.0, 0.5, 1.0)),
    ];
    let rect = SceneNode::new(
        "gradient-rect",
        doc.active_layer_id.unwrap(),
        SceneNodeKind::Path(
            PathNode::new(PathData::rect(10.0, 10.0, 100.0, 100.0)).with_fill(Fill::gradient(
                Gradient::linear(10.0, 10.0, 110.0, 110.0, stops),
            )),
        ),
    );
    doc.add_node(rect, None);
    doc
}

fn gradient_radial() -> Document {
    let mut doc = Document::new("gradient-radial", 120.0, 120.0);
    let stops = vec![
        GradientStop::new(0.0, Color::new(0.95, 0.95, 1.0, 1.0)),
        GradientStop::new(0.6, Color::new(0.2, 0.5, 0.9, 1.0)),
        GradientStop::new(1.0, Color::new(0.0, 0.05, 0.3, 1.0)),
    ];
    let ellipse = SceneNode::new(
        "gradient-ellipse",
        doc.active_layer_id.unwrap(),
        SceneNodeKind::Path(
            PathNode::new(PathData::ellipse(60.0, 60.0, 45.0, 45.0))
                .with_fill(Fill::gradient(Gradient::radial(60.0, 60.0, 45.0, stops))),
        ),
    );
    doc.add_node(ellipse, None);
    doc
}

/// Backdrop rect + four small rects, one per fixed-function `SEPARABLE_BLEND_MODES`
/// entry (`pipeline.rs` `[Multiply, Screen, Darken, Lighten]`) — the P1 refactor
/// must leave this pixel-identical since these modes never touch `COMPOSITE_SHADER`.
fn blend_separable() -> Document {
    blend_grid(
        "blend-separable",
        &[
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::Darken,
            BlendMode::Lighten,
        ],
    )
}

/// Same grid, but with non-separable / backdrop-read modes that `COMPOSITE_SHADER`
/// wiring (03 §2.4) will change the isolation path for — this case carries a
/// `tolerance_db.txt` (PSNR ≥ 45 dB) rather than requiring byte-exact match.
fn blend_nonseparable() -> Document {
    blend_grid(
        "blend-nonseparable",
        &[
            BlendMode::Hue,
            BlendMode::Saturation,
            BlendMode::Color,
            BlendMode::Luminosity,
        ],
    )
}

fn blend_grid(name: &str, modes: &[BlendMode; 4]) -> Document {
    let mut doc = Document::new(name, 120.0, 120.0);
    let backdrop = SceneNode::new(
        "backdrop",
        doc.active_layer_id.unwrap(),
        SceneNodeKind::Path(
            PathNode::new(PathData::rect(0.0, 0.0, 120.0, 120.0))
                .with_fill(Fill::solid(Color::new(0.8, 0.4, 0.2, 1.0))),
        ),
    );
    doc.add_node(backdrop, None);
    let positions = [(10.0, 10.0), (65.0, 10.0), (10.0, 65.0), (65.0, 65.0)];
    for (i, mode) in modes.iter().enumerate() {
        let (x, y) = positions[i];
        let mut node = SceneNode::new(
            format!("swatch-{i}"),
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(x, y, 45.0, 45.0))
                    .with_fill(Fill::solid(Color::new(0.3, 0.6, 0.9, 1.0))),
            ),
        );
        node.blend_mode = *mode;
        doc.add_node(node, None);
    }
    doc
}

fn text_basic() -> Document {
    let mut doc = Document::new("text-basic", 160.0, 60.0);
    let mut text = TextNode::new("Photonic");
    text.font_size = 24.0;
    text.fill = Fill::solid(Color::new(0.1, 0.1, 0.1, 1.0));
    let mut node = SceneNode::new(
        "title",
        doc.active_layer_id.unwrap(),
        SceneNodeKind::Text(text),
    );
    node.transform = Transform::translate(10.0, 15.0);
    doc.add_node(node, None);
    doc
}

/// Styled text (TD-018 headless-text coverage): font-size variation, per-node
/// colour, and a multi-line node. Pure-vector so it exercises the GPU glyphon
/// path; re-blessed once headless text renders.
fn text_styled() -> Document {
    let mut doc = Document::new("text-styled", 200.0, 130.0);
    let layer = doc.active_layer_id.unwrap();

    let mut title = TextNode::new("Title");
    title.font_size = 28.0;
    title.fill = Fill::solid(Color::new(0.85, 0.15, 0.15, 1.0));
    let mut n = SceneNode::new("title", layer, SceneNodeKind::Text(title));
    n.transform = Transform::translate(12.0, 10.0);
    doc.add_node(n, None);

    let mut sub = TextNode::new("subtitle");
    sub.font_size = 12.0;
    sub.fill = Fill::solid(Color::new(0.15, 0.30, 0.80, 1.0));
    let mut n = SceneNode::new("subtitle", layer, SceneNodeKind::Text(sub));
    n.transform = Transform::translate(12.0, 48.0);
    doc.add_node(n, None);

    let mut body = TextNode::new("line one\nline two");
    body.font_size = 14.0;
    body.fill = Fill::solid(Color::new(0.1, 0.1, 0.1, 1.0));
    let mut n = SceneNode::new("body", layer, SceneNodeKind::Text(body));
    n.transform = Transform::translate(12.0, 72.0);
    doc.add_node(n, None);

    doc
}

fn raster_placement() -> Document {
    let mut doc = Document::new("raster-placement", 120.0, 120.0);
    let image = RasterImage::filled(32, 32, [255, 128, 0, 255]);
    let mut node = SceneNode::new(
        "raster",
        doc.active_layer_id.unwrap(),
        SceneNodeKind::Raster(RasterNode::new(image)),
    );
    // Place + scale 2x so the placement transform is exercised, not just 1:1.
    node.transform = Transform::scale(2.0, 2.0).then(&Transform::translate(20.0, 20.0));
    doc.add_node(node, None);
    doc
}

/// A path styled entirely via the Layer-Styles effect stack (`node.effects`)
/// rather than its own `fill`/`stroke` fields — exercises `ColorOverlay` +
/// `StrokeEffect` composited in list order.
fn effect_stack() -> Document {
    let mut doc = Document::new("effect-stack", 120.0, 120.0);
    let mut node = SceneNode::new(
        "styled-shape",
        doc.active_layer_id.unwrap(),
        SceneNodeKind::Path(
            PathNode::new(PathData::ellipse(60.0, 60.0, 40.0, 40.0))
                .with_fill(Fill::solid(Color::TRANSPARENT)),
        ),
    );
    node.effects = vec![
        LayerEffect::ColorOverlay(ColorOverlay {
            color: Color::new(0.2, 0.7, 0.4, 1.0),
            ..ColorOverlay::default()
        }),
        LayerEffect::Stroke(StrokeEffect {
            width: 5.0,
            fill: Fill::solid(Color::new(0.05, 0.2, 0.1, 1.0)),
            ..StrokeEffect::default()
        }),
    ];
    doc.add_node(node, None);
    doc
}

/// Live (non-destructive) boolean group (#25) of two overlapping rects, for
/// any `BooleanOp` variant. Operand paths are added, folded into a
/// `GroupNode` with `live_boolean` set, then removed from the layer's
/// top-level `node_ids` (mirrors what the `GroupNodes` command does) so only
/// the resolved shape renders — matching what a real group-and-boolean user
/// action produces. Shared by `boolean_groups`/`boolean_subtract`/
/// `boolean_intersect`/`boolean_exclude` (corpus expansion batch 2).
fn boolean_group(name: &str, op: BooleanOp) -> Document {
    let mut doc = Document::new(name, 120.0, 120.0);
    let lid = doc.active_layer_id.unwrap();
    let a = SceneNode::new(
        "operand-a",
        lid,
        SceneNodeKind::Path(
            PathNode::new(PathData::rect(20.0, 20.0, 50.0, 50.0))
                .with_fill(Fill::solid(Color::new(0.9, 0.5, 0.1, 1.0))),
        ),
    );
    let b = SceneNode::new(
        "operand-b",
        lid,
        SceneNodeKind::Path(
            PathNode::new(PathData::rect(50.0, 50.0, 50.0, 50.0))
                .with_fill(Fill::solid(Color::new(0.9, 0.5, 0.1, 1.0))),
        ),
    );
    let (aid, bid) = (a.id, b.id);
    doc.add_node(a, Some(lid));
    doc.add_node(b, Some(lid));

    let mut group = GroupNode::new();
    group.children = vec![aid, bid];
    group.live_boolean = Some(op);
    let gnode = SceneNode::new("resolved", lid, SceneNodeKind::Group(group));
    doc.add_node(gnode, Some(lid));

    // Operands are now represented only via the group's children list — drop
    // their top-level layer entries so they don't ALSO render standalone.
    if let Some(layer) = doc.layers.get_mut(&lid) {
        layer.node_ids.retain(|id| *id != aid && *id != bid);
    }
    doc
}

// ═══════════════════════════════════════════════════════════════════════════
// Batch 2 — corpus expansion (P1 architect-gate finding [1]: 10 -> 30+ cases)
// ═══════════════════════════════════════════════════════════════════════════

/// Three instances of one master (translated + one recolored via a fill
/// override), plus the master itself hidden (`visible = false`) so only the
/// instances render — exercises `SceneNode::symbol_ref` resolution
/// (`Document::resolve_render_node`), which reads the master's *current*
/// `kind` live rather than a frozen copy.
fn symbol_instances() -> Document {
    let mut doc = Document::new("symbol-instances", 160.0, 160.0);
    let lid = doc.active_layer_id.unwrap();

    let mut master = SceneNode::new(
        "master",
        lid,
        SceneNodeKind::Path(
            PathNode::new(PathData::star(0.0, 0.0, 18.0, 8.0, 5))
                .with_fill(Fill::solid(Color::new(0.9, 0.7, 0.1, 1.0))),
        ),
    );
    master.visible = false; // definition only; instances carry the visible copies
    let master_id = doc.add_node(master, Some(lid));

    let sym = Symbol::new("star", master_id);
    let sym_id = sym.id;
    doc.symbols.push(sym);

    let positions = [(30.0, 30.0), (90.0, 50.0), (60.0, 120.0)];
    for (i, (x, y)) in positions.iter().enumerate() {
        // SceneNode::new mints a fresh id internally — avoids depending on the
        // `uuid` crate directly (not a `photonic-render` dependency); only the
        // starting `kind` is cloned from the master, which is fine since
        // `resolve_render_node` overwrites `kind` from the master live anyway.
        let master_kind = doc.nodes.get(&master_id).unwrap().kind.clone();
        let mut inst = SceneNode::new(format!("instance-{i}"), lid, master_kind);
        inst.symbol_ref = Some(sym_id);
        inst.transform = Transform::translate(*x, *y);
        doc.add_node(inst, Some(lid));
    }
    doc
}

/// A stroke-only line with a `WidthProfile` attached via `width_profile_id`
/// (widths ramp 2 -> 16 -> 4 along the path). NOTE (QA finding, see report):
/// `tessellate_stroke_variable` — the function that actually varies width
/// along the stroke — is wired ONLY into `renderer/tess_cache.rs` (the
/// interactive windowed renderer). `headless.rs` (what this harness, MCP
/// tools, and export all use) never resolves `width_profile_id` and always
/// calls the fixed-width `tessellate_stroke`. This fixture therefore renders
/// as a FLAT-width stroke today — the case is still valuable as a forward
/// guard (it will start actually exercising the variable-width path the
/// moment headless rendering is wired for it, and the blessed PNG will
/// correctly flag that day's diff as intentional, not a regression).
fn variable_width_stroke() -> Document {
    let mut doc = Document::new("variable-width-stroke", 140.0, 100.0);
    let lid = doc.active_layer_id.unwrap();
    let profile = WidthProfile::new("taper", vec![2.0, 16.0, 4.0]);
    let profile_id = profile.id;
    doc.width_profiles.push(profile);

    let mut stroke = Stroke::solid(Color::new(0.2, 0.3, 0.8, 1.0), 8.0);
    stroke.width_profile_id = Some(profile_id);
    let node = SceneNode::new(
        "tapered-line",
        lid,
        SceneNodeKind::Path(
            PathNode::new(PathData::line(10.0, 50.0, 130.0, 50.0))
                .with_fill(Fill::solid(Color::TRANSPARENT))
                .with_stroke(stroke),
        ),
    );
    doc.add_node(node, Some(lid));
    doc
}

/// Fixed-field glow stack: `outer_glow` + `inner_glow` + `gaussian_glow`
/// together (independent of each other — no mutual-exclusion in the effect
/// pipeline, unlike object-blur/feather below).
fn effect_glow_stack() -> Document {
    let mut doc = Document::new("effect-glow-stack", 120.0, 120.0);
    let mut node = SceneNode::new(
        "glowing-shape",
        doc.active_layer_id.unwrap(),
        SceneNodeKind::Path(
            PathNode::new(PathData::ellipse(60.0, 60.0, 25.0, 25.0))
                .with_fill(Fill::solid(Color::new(0.15, 0.2, 0.3, 1.0))),
        ),
    );
    node.outer_glow = GlowEffect {
        enabled: true,
        color: Color::new(0.3, 0.7, 1.0, 1.0),
        size: 14.0,
        ..GlowEffect::default()
    };
    node.inner_glow = GlowEffect {
        enabled: true,
        color: Color::new(1.0, 1.0, 1.0, 1.0),
        size: 6.0,
        ..GlowEffect::default()
    };
    node.gaussian_glow = GaussianGlow {
        enabled: true,
        color: Color::new(1.0, 0.4, 0.8, 1.0),
        radius: 10.0,
        ..GaussianGlow::default()
    };
    doc.add_node(node, None);
    doc
}

/// Fixed-field `drop_shadow` + `feather` together — independent of each
/// other (only `object_blur`/`feather` are mutually exclusive per
/// `headless.rs`'s `if object_blur.enabled { .. } else if feather.enabled`
/// chain; `drop_shadow` is a separate blur-job path entirely).
fn effect_shadow_feather() -> Document {
    let mut doc = Document::new("effect-shadow-feather", 120.0, 120.0);
    let mut node = SceneNode::new(
        "soft-shape",
        doc.active_layer_id.unwrap(),
        SceneNodeKind::Path(
            PathNode::new(PathData::rect(30.0, 30.0, 50.0, 50.0))
                .with_fill(Fill::solid(Color::new(0.8, 0.2, 0.2, 1.0))),
        ),
    );
    node.drop_shadow = DropShadow {
        enabled: true,
        dx: 6.0,
        dy: 6.0,
        blur: 8.0,
        ..DropShadow::default()
    };
    node.feather = Feather {
        enabled: true,
        radius: 5.0,
    };
    doc.add_node(node, None);
    doc
}

/// Two layers; the top layer has `opacity < 1.0` (must composite as an
/// isolated unit per `headless.rs`'s `has_isolated_layer` predicate).
fn isolated_layer_opacity() -> Document {
    let mut doc = Document::new("isolated-layer-opacity", 120.0, 120.0);
    let base_lid = doc.active_layer_id.unwrap();
    let base = SceneNode::new(
        "base",
        base_lid,
        SceneNodeKind::Path(
            PathNode::new(PathData::rect(10.0, 10.0, 60.0, 60.0))
                .with_fill(Fill::solid(Color::new(0.9, 0.6, 0.1, 1.0))),
        ),
    );
    doc.add_node(base, Some(base_lid));

    let mut top_layer = Layer::new("Top (half opacity)");
    top_layer.opacity = 0.5;
    let top_lid = doc.add_layer(top_layer);
    let top = SceneNode::new(
        "top",
        top_lid,
        SceneNodeKind::Path(
            PathNode::new(PathData::rect(50.0, 50.0, 60.0, 60.0))
                .with_fill(Fill::solid(Color::new(0.1, 0.4, 0.9, 1.0))),
        ),
    );
    doc.add_node(top, Some(top_lid));
    doc
}

/// Two layers; the top layer has a non-`Normal` blend mode (also triggers
/// `has_isolated_layer`, distinct code path from per-node blend mode).
fn isolated_layer_blend() -> Document {
    let mut doc = Document::new("isolated-layer-blend", 120.0, 120.0);
    let base_lid = doc.active_layer_id.unwrap();
    let base = SceneNode::new(
        "base",
        base_lid,
        SceneNodeKind::Path(
            PathNode::new(PathData::rect(10.0, 10.0, 60.0, 60.0))
                .with_fill(Fill::solid(Color::new(0.9, 0.6, 0.1, 1.0))),
        ),
    );
    doc.add_node(base, Some(base_lid));

    let mut top_layer = Layer::new("Top (multiply)");
    top_layer.blend_mode = BlendMode::Multiply;
    let top_lid = doc.add_layer(top_layer);
    let top = SceneNode::new(
        "top",
        top_lid,
        SceneNodeKind::Path(
            PathNode::new(PathData::rect(50.0, 50.0, 60.0, 60.0))
                .with_fill(Fill::solid(Color::new(0.1, 0.4, 0.9, 1.0))),
        ),
    );
    doc.add_node(top, Some(top_lid));
    doc
}

fn pattern_fill_grid() -> Document {
    let mut doc = Document::new("pattern-fill-grid", 120.0, 120.0);
    // A 4x4 tile: two-tone checker so a grid tiling is visually obvious.
    let mut tile = RasterImage::filled(8, 8, [255, 255, 255, 255]);
    for y in 0..8u32 {
        for x in 0..8u32 {
            if (x < 4) != (y < 4) {
                let i = ((y * 8 + x) * 4) as usize;
                tile.pixels[i..i + 4].copy_from_slice(&[40, 40, 40, 255]);
            }
        }
    }
    let pattern = PatternFill::new(tile);
    let node = SceneNode::new(
        "patterned-rect",
        doc.active_layer_id.unwrap(),
        SceneNodeKind::Path(
            PathNode::new(PathData::rect(10.0, 10.0, 100.0, 100.0))
                .with_fill(Fill::pattern(pattern)),
        ),
    );
    doc.add_node(node, None);
    doc
}

fn gradient_mesh() -> Document {
    let mut doc = Document::new("gradient-mesh", 120.0, 120.0);
    // `MeshGradient::grid` lays its lines out over the fractional [0,1] range;
    // without ObjectBoundingBox units that's read as literal (tiny) document
    // coordinates, so the 100x100 rect below sees almost the whole grid
    // clamped to its last cell (solid yellow) — caught by eyeballing the
    // blessed PNG, not a renderer bug. `with_units` is the fix.
    let mesh = MeshGradient::grid(
        2,
        2,
        vec![
            Color::new(1.0, 0.2, 0.2, 1.0),
            Color::new(0.2, 1.0, 0.2, 1.0),
            Color::new(0.2, 0.2, 1.0, 1.0),
            Color::new(1.0, 1.0, 0.2, 1.0),
        ],
    )
    .with_units(photonic_core::style::GradientUnits::ObjectBoundingBox);
    let node = SceneNode::new(
        "mesh-rect",
        doc.active_layer_id.unwrap(),
        SceneNodeKind::Path(
            PathNode::new(PathData::rect(10.0, 10.0, 100.0, 100.0))
                .with_fill(Fill::mesh_gradient(mesh)),
        ),
    );
    doc.add_node(node, None);
    doc
}

fn gradient_fluid() -> Document {
    let mut doc = Document::new("gradient-fluid", 120.0, 120.0);
    let fluid = FluidGradient::new(vec![
        FluidGradientPoint::new(20.0, 20.0, Color::new(1.0, 0.3, 0.1, 1.0)),
        FluidGradientPoint::new(100.0, 20.0, Color::new(0.1, 0.3, 1.0, 1.0)),
        FluidGradientPoint::new(60.0, 100.0, Color::new(0.2, 0.9, 0.4, 1.0)),
    ]);
    let node = SceneNode::new(
        "fluid-ellipse",
        doc.active_layer_id.unwrap(),
        SceneNodeKind::Path(
            PathNode::new(PathData::ellipse(60.0, 60.0, 50.0, 50.0))
                .with_fill(Fill::fluid_gradient(fluid)),
        ),
    );
    doc.add_node(node, None);
    doc
}

/// A text node with `path_spine_id` set to a spine path (Type-on-a-Path).
/// NOTE (QA finding, see report — SEVERE, bigger than the path-spine gap
/// this fixture was meant to cover): text does not render AT ALL under
/// headless rendering, path or no path. `compositor.rs:185` explicitly
/// no-ops `SceneNodeKind::Text(_) => {}`, and `headless.rs` (the GPU path)
/// has zero glyphon/text-atlas wiring of any kind — confirmed by grepping
/// both files. Blessed as a fully blank canvas; this is ALSO true of the
/// pre-existing `text_basic` fixture (not introduced by this batch — same
/// grep, same blank blessed PNG). Kept in the corpus specifically so a
/// future headless-text implementation has an immediate, obvious, already-
/// wired regression check rather than starting from zero coverage.
fn text_on_path() -> Document {
    let mut doc = Document::new("text-on-path", 160.0, 100.0);
    let lid = doc.active_layer_id.unwrap();
    let spine = SceneNode::new(
        "spine",
        lid,
        SceneNodeKind::Path(
            PathNode::new(PathData::arc(80.0, 90.0, 70.0, 70.0, 180.0, 360.0, false))
                .with_fill(Fill::solid(Color::TRANSPARENT)),
        ),
    );
    let spine_id = spine.id;
    doc.add_node(spine, Some(lid));

    let mut text = TextNode::new("On A Path");
    text.font_size = 16.0;
    text.fill = Fill::solid(Color::new(0.1, 0.1, 0.1, 1.0));
    text.path_spine_id = Some(spine_id);
    let mut node = SceneNode::new("path-text", lid, SceneNodeKind::Text(text));
    // Without an explicit transform, text renders at the local origin and was
    // rendering fully off-canvas (caught by eyeballing a BLANK blessed PNG —
    // a fixture-authoring bug, not a renderer gap; text_basic's own transform
    // is the same requirement, missed here on the first pass).
    node.transform = Transform::translate(20.0, 60.0);
    doc.add_node(node, Some(lid));
    doc
}

/// Alternating raster and vector nodes in z-order (raster, path, raster,
/// path) — a mixed-content document, which routes to the CPU compositor
/// (`has_raster` predicate) so vector/raster draw order must interleave
/// correctly rather than "all vectors as one plane beneath the rasters."
fn mixed_vector_raster_zorder() -> Document {
    let mut doc = Document::new("mixed-vector-raster-zorder", 120.0, 120.0);
    let lid = doc.active_layer_id.unwrap();

    let mut raster_a = SceneNode::new(
        "raster-back",
        lid,
        SceneNodeKind::Raster(RasterNode::new(RasterImage::filled(
            60,
            60,
            [80, 80, 220, 255],
        ))),
    );
    raster_a.transform = Transform::translate(10.0, 10.0);
    doc.add_node(raster_a, Some(lid));

    let path_a = SceneNode::new(
        "path-mid",
        lid,
        SceneNodeKind::Path(
            PathNode::new(PathData::ellipse(60.0, 45.0, 30.0, 30.0))
                .with_fill(Fill::solid(Color::new(0.9, 0.6, 0.1, 0.85))),
        ),
    );
    doc.add_node(path_a, Some(lid));

    let mut raster_b = SceneNode::new(
        "raster-front",
        lid,
        SceneNodeKind::Raster(RasterNode::new(RasterImage::filled(
            40,
            40,
            [220, 80, 80, 200],
        ))),
    );
    raster_b.transform = Transform::translate(60.0, 60.0);
    doc.add_node(raster_b, Some(lid));

    let path_b = SceneNode::new(
        "path-top",
        lid,
        SceneNodeKind::Path(
            PathNode::new(PathData::regular_polygon(90.0, 30.0, 18.0, 6))
                .with_fill(Fill::solid(Color::new(0.2, 0.8, 0.3, 1.0))),
        ),
    );
    doc.add_node(path_b, Some(lid));
    doc
}

/// ~200 small shapes in a grid — a dense-scene perf/regression case (large
/// node count exercises the P1 tessellation-cache/persistent-buffer path at
/// a scale the other, deliberately minimal, fixtures don't).
fn dense_many_nodes() -> Document {
    let mut doc = Document::new("dense-many-nodes", 200.0, 200.0);
    let lid = doc.active_layer_id.unwrap();
    let cols = 14;
    let rows = 15; // 14*15 = 210 nodes
    let cell = 200.0 / cols as f64;
    for row in 0..rows {
        for col in 0..cols {
            let cx = cell * (col as f64 + 0.5);
            let cy = cell * (row as f64 + 0.5);
            let hue_t = (col + row) as f32 / (cols + rows) as f32;
            let node = SceneNode::new(
                format!("cell-{row}-{col}"),
                lid,
                SceneNodeKind::Path(
                    PathNode::new(PathData::ellipse(cx, cy, cell * 0.35, cell * 0.35))
                        .with_fill(Fill::solid(Color::new(hue_t, 1.0 - hue_t, 0.5, 1.0))),
                ),
            );
            doc.add_node(node, Some(lid));
        }
    }
    doc
}

/// A single `is_compound: true` path made of two disjoint rectangle
/// subpaths — "treated as one shape" tessellation/undo/selection semantics,
/// distinct from a `GroupNode` of two separate paths.
fn compound_path() -> Document {
    let mut doc = Document::new("compound-path", 100.0, 100.0);
    let path = PathData::from_svg("M10,10 L40,10 L40,40 L10,40 Z M60,60 L90,60 L90,90 L60,90 Z")
        .expect("valid compound SVG path data");
    let mut pn = PathNode::new(path).with_fill(Fill::solid(Color::new(0.3, 0.5, 0.9, 1.0)));
    pn.is_compound = true;
    let node = SceneNode::new(
        "compound",
        doc.active_layer_id.unwrap(),
        SceneNodeKind::Path(pn),
    );
    doc.add_node(node, None);
    doc
}

/// A group nested inside another group, with opacity set on the OUTER group
/// only — exercises `group_opacity_map`'s ancestor-opacity propagation
/// (`headless.rs`) through two levels, not just one.
fn nested_groups() -> Document {
    let mut doc = Document::new("nested-groups", 120.0, 120.0);
    let lid = doc.active_layer_id.unwrap();

    let leaf = SceneNode::new(
        "leaf",
        lid,
        SceneNodeKind::Path(
            PathNode::new(PathData::rect(20.0, 20.0, 60.0, 60.0))
                .with_fill(Fill::solid(Color::new(0.8, 0.3, 0.3, 1.0))),
        ),
    );
    let leaf_id = leaf.id;
    doc.add_node(leaf, Some(lid));

    let mut inner_group = GroupNode::new();
    inner_group.children = vec![leaf_id];
    let inner_node = SceneNode::new("inner-group", lid, SceneNodeKind::Group(inner_group));
    let inner_id = inner_node.id;
    doc.add_node(inner_node, Some(lid));

    let mut outer_group = GroupNode::new();
    outer_group.children = vec![inner_id];
    let mut outer_node = SceneNode::new("outer-group", lid, SceneNodeKind::Group(outer_group));
    outer_node.opacity = 0.4;
    doc.add_node(outer_node, Some(lid));

    // The leaf and inner-group entries were pushed to the layer's top-level
    // node_ids by add_node; strip them so only the outer group is a
    // top-level draw root (mirrors the boolean-group fixtures' pattern).
    if let Some(layer) = doc.layers.get_mut(&lid) {
        layer
            .node_ids
            .retain(|id| *id != leaf_id && *id != inner_id);
    }
    doc
}

/// A raster node with a non-destructive layer mask: left half fully opaque
/// (255), right half fully transparent (0) — a hard-edge mask so the case is
/// visually unambiguous.
fn raster_mask() -> Document {
    let mut doc = Document::new("raster-mask", 100.0, 100.0);
    let image = RasterImage::filled(80, 80, [220, 60, 60, 255]);
    let mut mask = Mask::full(80, 80);
    for y in 0..80u32 {
        for x in 0..80u32 {
            if x >= 40 {
                mask.data[(y * 80 + x) as usize] = 0;
            }
        }
    }
    let mut node = SceneNode::new(
        "masked-raster",
        doc.active_layer_id.unwrap(),
        SceneNodeKind::Raster(RasterNode {
            image,
            mask: Some(mask),
            source_uri: None,
            adjustment: None,
        }),
    );
    node.transform = Transform::translate(10.0, 10.0);
    doc.add_node(node, None);
    doc
}

/// A non-destructive adjustment layer (`RasterNode::adjustment_layer`) over
/// a solid-fill path beneath it — adjustment layers carry no own pixels;
/// they re-apply their spec to the composite below on every render.
fn adjustment_layer_raster() -> Document {
    let mut doc = Document::new("adjustment-layer-raster", 100.0, 100.0);
    let lid = doc.active_layer_id.unwrap();
    let base = SceneNode::new(
        "base",
        lid,
        SceneNodeKind::Path(
            PathNode::new(PathData::rect(0.0, 0.0, 100.0, 100.0))
                .with_fill(Fill::solid(Color::new(0.5, 0.5, 0.5, 1.0))),
        ),
    );
    doc.add_node(base, Some(lid));

    let adjustment = SceneNode::new(
        "brightness-contrast",
        lid,
        SceneNodeKind::Raster(RasterNode::adjustment_layer(
            AdjustmentSpec::BrightnessContrast {
                brightness: 0.3,
                contrast: 0.2,
            },
        )),
    );
    doc.add_node(adjustment, Some(lid));
    doc
}

/// A stroked path with arrowheads at both ends plus a dash pattern. NOTE (QA
/// finding, see report — two SEPARATE gaps, not one): (1) `arrowhead_start`/
/// `arrowhead_end` are wired only in `renderer/mod.rs` (windowed), zero
/// references in `headless.rs` — same "windowed-only" pattern as
/// variable-width strokes / text-on-path / glow effects. (2) `dash_array` has
/// ZERO rendering references anywhere in `photonic-render` (`headless.rs`,
/// `renderer/mod.rs`, `tessellator.rs`, `compositor.rs` all clean) — it is
/// pure unrendered data model today, not even in the interactive renderer.
/// This was already true of the pre-existing `strokes_basic` fixture's
/// "line-dashed" case (committed before this batch); confirmed by eyeballing
/// both blessed PNGs — solid lines, no dashes, in each. This fixture renders
/// as a single plain solid line under headless: no arrowheads, no dashes.
fn stroke_arrowheads() -> Document {
    let mut doc = Document::new("stroke-arrowheads", 120.0, 80.0);
    let mut stroke = Stroke::solid(Color::new(0.15, 0.15, 0.15, 1.0), 4.0);
    stroke.arrowhead_start = ArrowheadStyle::OpenArrow;
    stroke.arrowhead_end = ArrowheadStyle::FilledArrow;
    stroke.dash_array = vec![10.0, 5.0];
    stroke.dash_corner_alignment = true;
    let node = SceneNode::new(
        "arrow-line",
        doc.active_layer_id.unwrap(),
        SceneNodeKind::Path(
            PathNode::new(PathData::line(15.0, 40.0, 105.0, 40.0))
                .with_fill(Fill::solid(Color::TRANSPARENT))
                .with_stroke(stroke),
        ),
    );
    doc.add_node(node, None);
    doc
}
