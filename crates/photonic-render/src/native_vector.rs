//! Explicit managed-vector boundary. The pure GPU scene is sRGB-encoded
//! **premultiplied linear-light** RGB, not the straight-alpha bytes of a still.
//! Decoding it must not multiply alpha a second time. Legacy conversion remains
//! unchanged. CPU/raster/effect isolation paths need their own qualification.
use photonic_core::{
    color::Color, layer::BlendMode, node::SceneNodeKind, style::FillKind, Document,
};
use std::collections::HashSet;

fn unit(v: f32) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}
fn color(c: Color) -> bool {
    [c.r, c.g, c.b, c.a].into_iter().all(unit)
}
fn bounded(v: f64) -> bool {
    v.is_finite() && v.abs() <= 1_000_000.0
}

/// Fail closed before tessellation. This first managed contract covers bounded
/// solid-filled/stroked paths and plain structural groups. Unsupported paint or
/// isolation is an explicit error rather than a silently different compositor.
pub fn validate_document(doc: &Document) -> Result<(), &'static str> {
    if !doc.width.is_finite()
        || !doc.height.is_finite()
        || !(1.0..=32768.0).contains(&doc.width)
        || !(1.0..=32768.0).contains(&doc.height)
        || doc.nodes.len() > 4096
        || doc.layers.len() > 256
    {
        return Err("managed vector document exceeds finite dimension/node limits");
    }
    if crate::document_needs_cpu_compositor(doc) {
        return Err("managed vector CPU/raster/layer-effect compositing is not qualified");
    }
    let mut path_bytes = 0usize;
    for n in doc.nodes.values() {
        if let SceneNodeKind::Path(p) = &n.kind {
            path_bytes = path_bytes.saturating_add(p.path_data.as_svg().len());
        }
    }
    if path_bytes > 1_048_576 {
        return Err("managed vector total path data exceeds 1 MiB");
    }
    let mut seen = HashSet::new();
    fn visit(
        doc: &Document,
        id: photonic_core::NodeId,
        seen: &mut HashSet<photonic_core::NodeId>,
        depth: usize,
    ) -> Result<(), &'static str> {
        if depth > 64 || !seen.insert(id) {
            return Err("managed vector hierarchy is cyclic, duplicated or too deep");
        }
        let n = doc
            .nodes
            .get(&id)
            .ok_or("managed vector hierarchy references a missing node")?;
        if n.id != id
            || !unit(n.opacity)
            || !n.transform.matrix.into_iter().all(bounded)
            || n.blend_mode != BlendMode::Normal
            || n.symbol_ref.is_some()
            || n.outer_glow.enabled
            || n.inner_glow.enabled
            || n.gaussian_glow.enabled
            || n.drop_shadow.enabled
            || n.object_blur.enabled
            || n.feather.enabled
            || n.effects.iter().any(|e| e.enabled())
        {
            return Err("managed vector requires finite normal-blend nodes without unqualified effects or symbols");
        }
        match &n.kind {
            SceneNodeKind::Path(p)=>{
                let svg=p.path_data.as_svg();
                if svg.len()>65536 {return Err("managed vector path exceeds geometry limit");}
                let path=kurbo::BezPath::from_svg(svg).map_err(|_|"managed vector path is malformed")?;
                if (!svg.trim().is_empty() && path.elements().is_empty()) || path.elements().len()>8192 {return Err("managed vector path exceeds segment limit");}
                for el in path.elements(){
                    use kurbo::PathEl::*;
                    let valid_point = |p: &kurbo::Point| bounded(p.x) && bounded(p.y);
                    let valid = match el {
                        MoveTo(p) | LineTo(p) => valid_point(p),
                        QuadTo(p, q) => valid_point(p) && valid_point(q),
                        CurveTo(p, q, r) => valid_point(p) && valid_point(q) && valid_point(r),
                        ClosePath => true,
                    };
                    if !valid { return Err("managed vector path coordinates are non-finite or too large"); }
                }
                if p.fill.enabled && (!unit(p.fill.opacity) || !matches!(p.fill.kind,FillKind::None|FillKind::Solid(_)) || matches!(p.fill.kind,FillKind::Solid(c) if !color(c))) {return Err("managed vector requires bounded solid fill paint");}
                let s=&p.stroke;
                if s.enabled && (!color(s.color)||!unit(s.opacity)||!s.width.is_finite()||!(0.0..=1024.0).contains(&s.width)||!s.miter_limit.is_finite()||!(1.0..=128.0).contains(&s.miter_limit)||!s.dash_array.is_empty()||s.dash_offset!=0.0||s.paint.is_some()||s.width_profile_id.is_some()||s.arrowhead_start!=photonic_core::style::ArrowheadStyle::None||s.arrowhead_end!=photonic_core::style::ArrowheadStyle::None) {return Err("managed vector stroke paint or metrics are not qualified");}
            }
            SceneNodeKind::Group(g)=>{
                if g.clip_children||g.clip_node_id.is_some()||g.blend_spine_id.is_some()||g.live_boolean.is_some()||n.opacity!=1.0||n.transform!=photonic_core::transform::Transform::IDENTITY {return Err("managed vector group clipping, isolation or transform is not qualified");}
                for child in &g.children {visit(doc,*child,seen,depth+1)?;}
            }
            _=>return Err("managed vector source requires paths; outline text and keep raster images as qualified still sources"),
        }
        Ok(())
    }
    let mut layers = HashSet::new();
    for id in &doc.layer_order {
        if !layers.insert(*id) {
            return Err("managed vector layer order contains duplicates");
        }
        let layer = doc
            .layers
            .get(id)
            .ok_or("managed vector layer is missing")?;
        if layer.id != *id || !unit(layer.opacity) {
            return Err("managed vector layer identity or opacity is invalid");
        }
        for node in &layer.node_ids {
            visit(doc, *node, &mut seen, 0)?;
        }
    }
    Ok(())
}

/// Copy authored swatches into linear Rec.709 for the GPU scene's shader inputs.
/// The original editable document and the Legacy renderer are never changed.
pub fn linearized_document(doc: &Document) -> Result<Document, &'static str> {
    validate_document(doc)?;
    let mut result = doc.clone();
    fn decode(c: &mut Color) {
        for v in [&mut c.r, &mut c.g, &mut c.b] {
            *v = if *v <= 0.04045 {
                *v / 12.92
            } else {
                ((*v + 0.055) / 1.055).powf(2.4)
            };
        }
    }
    for n in result.nodes.values_mut() {
        if let SceneNodeKind::Path(p) = &mut n.kind {
            if let FillKind::Solid(c) = &mut p.fill.kind {
                decode(c);
            }
            decode(&mut p.stroke.color);
        }
    }
    Ok(result)
}

/// Decode raw sRGB scene bytes directly into premultiplied linear Rec.709 f16.
/// No resampling, unpremultiplication or second coverage multiplication occurs.
pub fn decode_premultiplied_scene(bytes: &[u8], w: u32, h: u32) -> Result<Vec<u8>, &'static str> {
    let pixels = u64::from(w) * u64::from(h);
    if w == 0
        || h == 0
        || w > 8192
        || h > 8192
        || pixels > 16_777_216
        || bytes.len() as u64 != pixels * 4
    {
        return Err("managed vector readback size is invalid");
    }
    let mut out = Vec::with_capacity(bytes.len() * 2);
    for p in bytes.as_chunks::<4>().0 {
        let a = p[3] as f32 / 255.0;
        for c in 0..4 {
            let v = if c == 3 {
                a
            } else if a == 0.0 {
                0.0
            } else {
                let s = p[c] as f32 / 255.0;
                if s <= 0.04045 {
                    s / 12.92
                } else {
                    ((s + 0.055) / 1.055).powf(2.4)
                }
            };
            out.extend_from_slice(&f16_bits(v).to_le_bytes());
        }
    }
    Ok(out)
}
fn f16_bits(v: f32) -> u16 {
    let b = v.to_bits();
    let e = ((b >> 23) & 255) as i32 - 112;
    if e <= 0 {
        0
    } else {
        ((e as u16) << 10) | (((b & 0x7fffff) >> 13) as u16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::{
        node::{PathNode, SceneNode},
        path::PathData,
        style::Fill,
    };
    fn doc() -> Document {
        let mut d = Document::new("managed logo", 64.0, 64.0);
        d.add_node(
            SceneNode::new(
                "back",
                d.active_layer_id.unwrap(),
                SceneNodeKind::Path(
                    PathNode::new(PathData::rect(8.0, 8.0, 48.0, 48.0))
                        .with_fill(Fill::solid(Color::new(0.5, 0.2, 0.8, 0.5))),
                ),
            ),
            None,
        );
        d.add_node(
            SceneNode::new(
                "front",
                d.active_layer_id.unwrap(),
                SceneNodeKind::Path(
                    PathNode::new(PathData::rect(24.0, 24.0, 32.0, 32.0))
                        .with_fill(Fill::solid(Color::new(0.1, 0.7, 0.3, 0.75))),
                ),
            ),
            None,
        );
        d
    }
    fn half(b: &[u8]) -> f64 {
        let n = u16::from_le_bytes([b[0], b[1]]);
        let e = ((n >> 10) & 31) as i32;
        let f = (n & 1023) as f64;
        if e == 0 {
            f * 2f64.powi(-24)
        } else {
            (1.0 + f / 1024.0) * 2f64.powi(e - 15)
        }
    }
    fn eotf(v: f64) -> f64 {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    #[test]
    fn native_vector_actual_gpu_matches_independent_linear_alpha_over_and_preserves_document() {
        let original = doc();
        let before = serde_json::to_string(&original).unwrap();
        let _linear = linearized_document(&original).unwrap();
        assert_eq!(serde_json::to_string(&original).unwrap(), before);
        let renderer = pollster::block_on(crate::HeadlessRenderer::new());
        for size in [64, 32, 64] {
            let (rgba, w, h) = renderer
                .render_managed_vector_rgba(&original, size, size)
                .unwrap();
            let pixels = decode_premultiplied_scene(&rgba, w, h).unwrap();
            // Native uses the exact document canvas, unlike the Legacy preview margin.
            for (x, y, expected) in [
                (0, 0, [0.0; 4]),
                (
                    size / 3,
                    size / 3,
                    [eotf(0.5) * 0.5, eotf(0.2) * 0.5, eotf(0.8) * 0.5, 0.5],
                ),
                (
                    size / 2,
                    size / 2,
                    [
                        eotf(0.1) * 0.75 + eotf(0.5) * 0.125,
                        eotf(0.7) * 0.75 + eotf(0.2) * 0.125,
                        eotf(0.3) * 0.75 + eotf(0.8) * 0.125,
                        0.875,
                    ],
                ),
            ] {
                let i = ((y * size + x) * 8) as usize;
                for c in 0..4 {
                    let actual = half(&pixels[i + c * 2..i + c * 2 + 2]);
                    assert!(
                        (actual - expected[c]).abs() < 0.007,
                        "size {size} {x},{y} channel {c}: {actual} vs {}",
                        expected[c]
                    );
                }
            }
        }
    }
    #[test]
    fn native_vector_boundaries_reject_invalid_and_unqualified_documents() {
        for case in 0..10 {
            let mut d = doc();
            let id = d.layers[&d.active_layer_id.unwrap()].node_ids[0];
            match case {
                0 => d.width = f64::NAN,
                1 => d.nodes.get_mut(&id).unwrap().opacity = f32::NAN,
                2 => d.nodes.get_mut(&id).unwrap().transform.matrix[0] = f64::INFINITY,
                3 => d.nodes.get_mut(&id).unwrap().blend_mode = BlendMode::Multiply,
                4 => d.nodes.get_mut(&id).unwrap().drop_shadow.enabled = true,
                5 => d
                    .layers
                    .get_mut(&d.active_layer_id.unwrap())
                    .unwrap()
                    .node_ids
                    .push(id),
                6 => {
                    if let SceneNodeKind::Path(p) = &mut d.nodes.get_mut(&id).unwrap().kind {
                        p.fill = Fill::solid(Color::new(2.0, 0.0, 0.0, 1.0));
                    }
                }
                7 => {
                    d.layers
                        .get_mut(&d.active_layer_id.unwrap())
                        .unwrap()
                        .opacity = 0.5
                }
                8 => {
                    if let SceneNodeKind::Path(p) = &mut d.nodes.get_mut(&id).unwrap().kind {
                        p.stroke = photonic_core::style::Stroke::solid(Color::RED, f64::NAN);
                    }
                }
                _ => {
                    d.nodes.remove(&id);
                }
            }
            assert!(validate_document(&d).is_err(), "case {case}");
        }
        assert!(decode_premultiplied_scene(&[], 0, 64).is_err());
        assert!(decode_premultiplied_scene(&[0; 3], 1, 1).is_err());
        assert_eq!(
            decode_premultiplied_scene(&[255, 128, 99, 0], 1, 1).unwrap(),
            [0; 8]
        );
    }
}
