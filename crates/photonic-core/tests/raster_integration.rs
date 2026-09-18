//! End-to-end integration: a raster node lives in the scene graph, survives a
//! format-v2 JSON round-trip, edits with the adjustment engine, and exports to
//! SVG as an embedded image.

use photonic_core::document::CURRENT_FORMAT_VERSION;
use photonic_core::export::{export_svg, SvgExportOptions};
use photonic_core::node::{RasterNode, SceneNode, SceneNodeKind};
use photonic_core::raster::{adjust, image::RasterImage};
use photonic_core::Document;

fn red_raster_node(doc: &Document) -> SceneNode {
    let img = RasterImage::filled(8, 8, [200, 30, 30, 255]);
    let layer = doc.active_layer_id.unwrap();
    SceneNode::new("photo", layer, SceneNodeKind::Raster(RasterNode::new(img)))
}

#[test]
fn raster_node_survives_v2_json_roundtrip() {
    let mut doc = Document::new("t", 64.0, 64.0);
    let node = red_raster_node(&doc);
    let id = doc.add_node(node, None);

    assert_eq!(doc.format_version, CURRENT_FORMAT_VERSION);
    // v5 added the §35 effect-scope / marker / group model; raster nodes are
    // unaffected and still round-trip.
    assert_eq!(CURRENT_FORMAT_VERSION, 5);

    let json = doc.to_json().unwrap();
    assert!(json.contains("\"type\": \"raster\"") || json.contains("\"type\":\"raster\""));

    let back = Document::from_json(&json).unwrap();
    let n = back
        .get_node(&id)
        .expect("raster node present after reload");
    match &n.kind {
        SceneNodeKind::Raster(r) => {
            assert_eq!((r.image.width, r.image.height), (8, 8));
            assert_eq!(r.image.pixel(0, 0), [200, 30, 30, 255]);
        }
        _ => panic!("expected raster node"),
    }
}

#[test]
fn v1_document_migrates_to_current() {
    // A minimal v1 doc with no raster nodes should load and migrate forward
    // through the whole chain (v1→v2→v3→v4→v5) to the current version.
    let mut doc = Document::new("t", 32.0, 32.0);
    doc.format_version = 1;
    let json = doc.to_json().unwrap();
    let back = Document::from_json(&json).unwrap();
    assert_eq!(back.format_version, CURRENT_FORMAT_VERSION);
}

#[test]
fn adjustment_engine_edits_node_pixels() {
    let mut doc = Document::new("t", 64.0, 64.0);
    let node = red_raster_node(&doc);
    let id = doc.add_node(node, None);

    if let Some(n) = doc.get_node_mut(&id) {
        if let SceneNodeKind::Raster(r) = &mut n.kind {
            adjust::invert(&mut r.image, None);
        }
    }
    match &doc.get_node(&id).unwrap().kind {
        SceneNodeKind::Raster(r) => {
            // invert of (200,30,30) → (55,225,225)
            assert_eq!(r.image.pixel(0, 0), [55, 225, 225, 255]);
        }
        _ => unreachable!(),
    }
}

#[test]
fn adjustment_spec_matches_direct_call() {
    use photonic_core::raster::adjust;
    use photonic_core::AdjustmentSpec;
    let base = RasterImage::filled(4, 4, [120, 60, 200, 255]);

    let mut a = base.clone();
    adjust::invert(&mut a, None);

    let mut b = base.clone();
    AdjustmentSpec::Invert.apply(&mut b, None);

    assert_eq!(a, b);
}

#[test]
fn adjustment_layer_node_roundtrips() {
    use photonic_core::node::RasterNode;
    use photonic_core::AdjustmentSpec;
    let mut doc = Document::new("t", 32.0, 32.0);
    let spec = AdjustmentSpec::HueSaturation {
        hue: 30.0,
        saturation: 0.2,
        lightness: 0.0,
    };
    let layer = doc.active_layer_id.unwrap();
    let node = SceneNode::new(
        "hue adj",
        layer,
        SceneNodeKind::Raster(RasterNode::adjustment_layer(spec.clone())),
    );
    let id = doc.add_node(node, None);

    let json = doc.to_json().unwrap();
    let back = Document::from_json(&json).unwrap();
    match &back.get_node(&id).unwrap().kind {
        SceneNodeKind::Raster(r) => {
            assert!(r.is_adjustment_layer());
            assert_eq!(r.adjustment.as_ref(), Some(&spec));
        }
        _ => panic!("expected raster adjustment node"),
    }
}

#[test]
fn svg_export_skips_adjustment_layers_no_placeholder() {
    use photonic_core::node::RasterNode;
    use photonic_core::AdjustmentSpec;
    // A document whose only node is an adjustment layer must NOT emit a bogus
    // 1×1 <image> placeholder (the round-2 audit's data-loss blocker).
    let mut doc = Document::new("t", 64.0, 64.0);
    let layer = doc.active_layer_id.unwrap();
    let adj = SceneNode::new(
        "levels adj",
        layer,
        SceneNodeKind::Raster(RasterNode::adjustment_layer(AdjustmentSpec::Invert)),
    );
    doc.add_node(adj, None);
    let svg = export_svg(&doc, &SvgExportOptions::default());
    assert!(
        !svg.contains("<image"),
        "adjustment layer must not emit an <image> placeholder"
    );

    // A real raster node alongside it still exports exactly one image.
    let img = SceneNode::new(
        "photo",
        layer,
        SceneNodeKind::Raster(RasterNode::new(RasterImage::filled(
            8,
            8,
            [10, 20, 30, 255],
        ))),
    );
    doc.add_node(img, None);
    let svg2 = export_svg(&doc, &SvgExportOptions::default());
    assert_eq!(svg2.matches("<image").count(), 1);
}

#[test]
fn svg_export_embeds_raster_image() {
    let mut doc = Document::new("t", 64.0, 64.0);
    let node = red_raster_node(&doc);
    doc.add_node(node, None);
    let svg = export_svg(&doc, &SvgExportOptions::default());
    assert!(svg.contains("<image"));
    assert!(svg.contains("data:image/png;base64,"));
}
