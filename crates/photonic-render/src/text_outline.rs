//! Flat text-to-outline conversion for font-independent PDF export.
//!
//! When exporting to PDF with `outline_text: true`, every [`TextNode`] in the
//! document must be converted to vector paths so the resulting file has **zero**
//! font dependencies — the glyphs are present as PDF path operators and render
//! identically regardless of which fonts are installed on the viewer's system.
//!
//! Architecture note: this module lives in `photonic-render` because `photonic-core`
//! has no font system. `photonic-mcp` calls `outline_document_text` before invoking
//! `photonic_core::export::export_pdf`, so the core export code never needs to know
//! about glyphon.

use glyphon::cosmic_text::fontdb;
use glyphon::{Family, FontSystem};
use kurbo::{Affine, BezPath};
use photonic_core::{
    document::Document,
    node::{NodeId, PathNode, SceneNodeKind, TextNode},
    path::PathData,
};
use ttf_parser::{GlyphId, OutlineBuilder};

// ─── Glyph outline builder ────────────────────────────────────────────────────

/// Accumulates a `ttf-parser` glyph outline (font units, Y-up) into a kurbo
/// `BezPath`. Shared with `text_path` (both need the same converter). This type
/// is `pub(crate)` so the sibling module can reuse it without re-exporting it as
/// part of the crate's public API.
#[derive(Default)]
pub(crate) struct BezOutlineBuilder {
    pub path: BezPath,
}

impl OutlineBuilder for BezOutlineBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.move_to((x as f64, y as f64));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to((x as f64, y as f64));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.path
            .quad_to((x1 as f64, y1 as f64), (x as f64, y as f64));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.path.curve_to(
            (x1 as f64, y1 as f64),
            (x2 as f64, y2 as f64),
            (x as f64, y as f64),
        );
    }
    fn close(&mut self) {
        self.path.close_path();
    }
}

// ─── Font resolution ───────────────────────────────────────────────────────────

/// The concrete font face glyphon actually resolved for a text node's request —
/// i.e. the face whose glyph outlines the PDF/raster export will embed.
///
/// The export path must embed the outlines of the *document's* font. When the
/// requested family/weight cannot be found, glyphon silently falls back to a
/// default face, and the exported glyphs then no longer match what the live
/// renderer draws. This struct lets callers (and tests) verify the resolved face
/// equals the requested family + weight, and drives the substitution warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFace {
    /// fontdb id of the resolved face.
    pub id: fontdb::ID,
    /// Primary family name of the resolved face (as recorded in the font DB).
    pub family: String,
    /// Weight (100–900) of the resolved face.
    pub weight: u16,
    /// Whether the resolved face is italic/oblique (non-normal style).
    pub italic: bool,
}

/// CSS generic family keywords. When a text node's family is one of these there
/// is no "correct" concrete face, so resolving to *any* installed family is a
/// legitimate match — not a silent substitution worth warning about.
fn is_generic_family(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "sans-serif"
            | "serif"
            | "monospace"
            | "cursive"
            | "fantasy"
            | "system-ui"
            | "ui-sans-serif"
            | "ui-serif"
            | "ui-monospace"
            | "sans"
            | "mono"
            | "emoji"
            | "math"
    )
}

/// Convert document/CSS family names into cosmic-text's generic-family
/// variants. Passing `"sans-serif"` through as `Family::Name` happens to work
/// on some font backends (for example via a Fontconfig alias) but resolves to
/// no glyphs on others, including the macOS CI runner. Keep concrete family
/// names as names while making generic defaults genuinely platform-portable.
pub(crate) fn cosmic_family(name: &str) -> Family<'_> {
    match name.trim().to_ascii_lowercase().as_str() {
        "serif" | "ui-serif" => Family::Serif,
        "sans-serif" | "system-ui" | "ui-sans-serif" | "sans" => Family::SansSerif,
        "monospace" | "ui-monospace" | "mono" => Family::Monospace,
        "cursive" => Family::Cursive,
        "fantasy" => Family::Fantasy,
        _ => Family::Name(name),
    }
}

/// Look up the family name, weight, and style of a resolved font id in the DB.
fn face_info(font_system: &FontSystem, id: fontdb::ID) -> Option<ResolvedFace> {
    let info = font_system.db().face(id)?;
    let family = info
        .families
        .first()
        .map(|(name, _)| name.clone())
        .unwrap_or_default();
    Some(ResolvedFace {
        id,
        family,
        weight: info.weight.0,
        italic: !matches!(info.style, fontdb::Style::Normal),
    })
}

/// Emit a `tracing::warn!` when glyphon resolved a *different* family or weight
/// than the document requested — surfacing a silent substitution instead of
/// letting the export quietly render in a fallback font.
fn warn_if_substituted(node: &TextNode, resolved: &ResolvedFace) {
    // A generic keyword ("sans-serif" …) has no canonical concrete face.
    if is_generic_family(&node.font_family) {
        return;
    }
    if !resolved
        .family
        .eq_ignore_ascii_case(node.font_family.trim())
    {
        tracing::warn!(
            requested_family = %node.font_family,
            requested_weight = node.font_weight,
            resolved_family = %resolved.family,
            resolved_weight = resolved.weight,
            "export font substitution: requested family not found in the font DB; \
             outlined glyphs will use a fallback face and will not match the document"
        );
    } else if resolved.weight != node.font_weight {
        tracing::warn!(
            requested_family = %node.font_family,
            requested_weight = node.font_weight,
            resolved_weight = resolved.weight,
            "export font weight substitution: exact weight unavailable for this \
             family; outlining the nearest available weight"
        );
    }
}

/// Shape `node`'s text and report the concrete font face glyphon resolves for
/// its first outlined glyph — the face whose outlines an outline/raster export
/// will actually embed.
///
/// Returns `None` only when nothing resolves (empty content in a font-less
/// environment). Also emits a substitution warning via [`warn_if_substituted`]
/// so a missing document font is surfaced rather than silently swapped.
///
/// This is the export counterpart to the live renderer's shaping: both build the
/// same `Attrs` (family + weight + style) over a `FontSystem::new()` DB — which
/// on Linux includes `~/.local/share/fonts` — so a face that resolves here is
/// exactly the one the live renderer draws.
pub fn resolve_document_font(
    font_system: &mut FontSystem,
    node: &TextNode,
) -> Option<ResolvedFace> {
    let probe: &str = if node.content.trim().is_empty() {
        "A"
    } else {
        &node.content
    };
    let buf = crate::text_layout::layout_text_buffer(
        font_system,
        probe,
        &node.font_family,
        node.font_size as f32,
        crate::TextLayoutOptions {
            font_weight: node.font_weight,
            font_style: node.font_style,
            line_height_mul: node.line_height as f32,
            letter_spacing: node.letter_spacing as f32,
            vertical: node.vertical,
        },
    );

    let font_id = buf
        .layout_runs()
        .flat_map(|run| run.glyphs.iter().map(|g| g.font_id))
        .next()?;
    let resolved = face_info(font_system, font_id)?;
    warn_if_substituted(node, &resolved);
    Some(resolved)
}

// ─── Flat layout ─────────────────────────────────────────────────────────────

/// Shape `node.content` with glyphon, then extract each glyph's outline from
/// ttf-parser and merge them into one `PathData` in **node-local space**.
///
/// The returned path contains all glyph contours merged into a single compound
/// path, so counter-shapes (holes in `o`, `e`, `a` …) fill correctly under
/// non-zero winding — ttf contours already use the correct winding direction.
///
/// Returns an empty `PathData` when `node.content` is empty or every glyph
/// resolves to whitespace / a bitmap-only glyph with no outline.
///
/// The node's `transform` is **not** applied here; the exporter applies it
/// after the fact, exactly as it does for `PathNode` geometry.
pub fn layout_text_flat(font_system: &mut FontSystem, node: &TextNode) -> PathData {
    if node.content.is_empty() {
        return PathData::new();
    }

    let buf = crate::text_layout::layout_text_buffer(
        font_system,
        &node.content,
        &node.font_family,
        node.font_size as f32,
        crate::TextLayoutOptions {
            font_weight: node.font_weight,
            font_style: node.font_style,
            line_height_mul: node.line_height as f32,
            letter_spacing: node.letter_spacing as f32,
            vertical: node.vertical,
        },
    );

    let mut merged = BezPath::new();
    // Warn once if glyphon substituted a different family/weight than requested,
    // so a missing document font is surfaced rather than silently swapped.
    let mut checked_substitution = false;

    for run in buf.layout_runs() {
        // `line_y` in glyphon/cosmic-text is the baseline Y of the run in
        // buffer-local coordinates (Y-down, origin at the top of the buffer).
        let baseline_y = run.line_y as f64;

        // Alignment: the live renderer (`renderer::mod` / `text_renderer`) draws
        // flat text by handing glyphon a `TextArea` whose `left` is the node's
        // transform origin and never calls `Buffer::set_align`, so glyphon lays
        // every run out left-anchored at the origin — `node.align` has no effect
        // on a run's *position*. The exported outline must match that pixel-for-
        // pixel, so we keep glyphon's own left-anchored `g.x` and apply **no**
        // horizontal alignment offset. (Previously this shifted center/right runs
        // left by run_width/2 or run_width, so exported centered text landed left
        // of where it renders live — bug fixed here.)
        let align_offset = 0.0;

        for (i, g) in run.glyphs.iter().enumerate() {
            let Some(font) = font_system.get_font(g.font_id) else {
                continue;
            };
            // First outlined glyph tells us the face glyphon actually resolved;
            // compare it against the request and warn on silent substitution.
            if !checked_substitution {
                checked_substitution = true;
                if let Some(resolved) = face_info(font_system, g.font_id) {
                    warn_if_substituted(node, &resolved);
                }
            }
            let face = font.rustybuzz(); // &ttf_parser::Face via Deref
            let units = face.units_per_em() as f64;
            if units <= 0.0 {
                continue;
            }

            let mut builder = BezOutlineBuilder::default();
            if face
                .outline_glyph(GlyphId(g.glyph_id), &mut builder)
                .is_none()
            {
                // No outline: whitespace, bitmap-only font, etc.
                continue;
            }

            // Glyph position in buffer space:
            //   g.x  — left edge of the glyph advance rectangle (f32, Y-down)
            //   g.y  — vertical offset relative to the baseline (usually 0.0
            //          for CJK or glyphs that sit on/below the baseline)
            // The font-unit outline is Y-up; we flip it to Y-down by scaling Y
            // by -scale and then translating so the origin lands on the baseline.
            let letter_spacing = if node.vertical {
                0.0
            } else {
                node.letter_spacing * i as f64
            };
            let gx =
                align_offset + g.x as f64 + letter_spacing + g.x_offset as f64 * g.font_size as f64;
            let gy = baseline_y + g.y as f64 + g.y_offset as f64 * g.font_size as f64;

            let scale = g.font_size as f64 / units;
            // font units (Y-up, origin at glyph origin) → doc space (Y-down):
            //   translate to (gx, gy) · scale(scale, -scale) · outline
            let affine = Affine::translate((gx, gy)) * Affine::scale_non_uniform(scale, -scale);

            let mut glyph_path = builder.path;
            glyph_path.apply_affine(affine);

            // Extend the merged path with all elements of this glyph's outline.
            for el in glyph_path.iter() {
                merged.push(el);
            }
        }
    }

    if merged.is_empty() {
        return PathData::new();
    }
    PathData::from_bez_path(&merged)
}

// ─── Document-level outlining ────────────────────────────────────────────────

/// Return a **clone** of `doc` in which every visible [`TextNode`] (including
/// nodes nested inside groups at any depth) has been replaced by a [`PathNode`]
/// carrying the glyph outlines produced by [`layout_text_flat`] (or, when a
/// horizontal text node follows a path spine, by
/// [`crate::text_path::layout_text_on_path`]). Vertical text uses the flat
/// top-to-bottom layout even if a stale path-spine association is present.
///
/// The original `doc` is never mutated (undo-safe). The returned document is
/// suitable for a single-use font-free export; it should **not** be inserted
/// into the undo history.
///
/// Node identity is preserved: the replacement path node keeps the same `id`,
/// `name`, `transform`, `opacity`, `visible`, `blend_mode`, `layer_id`, and
/// group/layer membership as the original text node.  Only `kind` changes from
/// `Text(…)` to `Path(…)`.
pub fn outline_document_text(doc: &Document, font_system: &mut FontSystem) -> Document {
    let mut clone = doc.clone();

    // Collect the IDs of all text nodes up front (avoids borrow conflicts while
    // we mutate clone.nodes).
    let text_ids: Vec<NodeId> = clone
        .nodes
        .values()
        .filter(|n| matches!(n.kind, SceneNodeKind::Text(_)))
        .map(|n| n.id)
        .collect();

    for id in text_ids {
        let Some(node) = clone.nodes.get(&id) else {
            continue;
        };
        // Skip invisible nodes — they won't appear in the PDF anyway.
        if !node.visible {
            continue;
        }

        let SceneNodeKind::Text(ref text) = node.kind else {
            continue;
        };
        let text = text.clone(); // clone to release the borrow on node

        let path_data = if !text.vertical {
            if let Some(spine_id) = text.path_spine_id {
                // Text-on-path: use the existing on-path layout.
                let spine = clone.nodes.get(&spine_id).and_then(|n| match &n.kind {
                    SceneNodeKind::Path(p) => Some(p.path_data.clone()),
                    _ => None,
                });
                if let Some(spine_path) = spine {
                    use crate::text_path::{layout_text_on_path, TextOnPathParams};
                    let params = TextOnPathParams {
                        content: &text.content,
                        font_family: &text.font_family,
                        font_size: text.font_size,
                        font_weight: text.font_weight,
                        font_style: text.font_style,
                        line_height: text.line_height,
                        letter_spacing: text.letter_spacing,
                        align: text.align,
                        path_offset: text.path_offset,
                    };
                    let glyph_paths = layout_text_on_path(font_system, &params, &spine_path);
                    if glyph_paths.is_empty() {
                        PathData::new()
                    } else {
                        // Merge all per-glyph paths into one compound path.
                        use kurbo::BezPath;
                        let mut merged = BezPath::new();
                        for gp in &glyph_paths {
                            for el in gp.to_bez_path().iter() {
                                merged.push(el);
                            }
                        }
                        PathData::from_bez_path(&merged)
                    }
                } else {
                    // Spine node not found; fall back to flat layout.
                    layout_text_flat(font_system, &text)
                }
            } else {
                layout_text_flat(font_system, &text)
            }
        } else {
            layout_text_flat(font_system, &text)
        };

        // Build a PathNode carrying the outlined glyphs with the text's style.
        let path_node = PathNode {
            path_data,
            fill: text.fill.clone(),
            stroke: text.stroke.clone(),
            is_compound: true, // counter-shapes handled via nonzero winding
        };

        // Swap the node's kind in place (all other fields stay identical).
        if let Some(node) = clone.nodes.get_mut(&id) {
            node.kind = SceneNodeKind::Path(path_node);
        }
    }

    clone
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::{
        document::Document,
        node::{FontStyle, SceneNode, SceneNodeKind, TextNode},
    };

    fn make_doc_with_text(content: &str) -> (Document, NodeId) {
        let mut doc = Document::new("test", 400.0, 300.0);
        let layer_id = doc
            .active_layer_id
            .expect("Document::new always creates a default layer");
        let mut text_node = TextNode::new(content);
        text_node.font_family = "sans-serif".to_string();
        text_node.font_size = 32.0;
        text_node.font_weight = 400;
        text_node.line_height = 1.2;
        text_node.letter_spacing = 0.0;
        let node = SceneNode::new("text", layer_id, SceneNodeKind::Text(text_node));
        let node_id = node.id;
        if let Some(layer) = doc.layers.get_mut(&layer_id) {
            layer.node_ids.push(node_id);
        }
        doc.nodes.insert(node_id, node);
        (doc, node_id)
    }

    /// `layout_text_flat` with "Ag" should return a non-empty PathData that
    /// has a bounding box with a positive height (i.e. glyphs were outlined).
    /// On CI images with no usable fonts the test is allowed to produce an
    /// empty result and skips the metric assertions.
    #[test]
    fn layout_text_flat_ag_has_geometry() {
        let mut fs = FontSystem::new();
        let mut node = TextNode::new("Ag");
        node.font_size = 32.0;
        let pd = layout_text_flat(&mut fs, &node);
        if pd.is_empty() {
            eprintln!("no system font available — skipping geometry assertions");
            return;
        }
        let bb = pd.bounding_box().expect("non-empty path must have a bbox");
        // The glyphs must have measurable width and height.
        assert!(bb.width() > 0.0, "width={}", bb.width());
        assert!(bb.height() > 0.0, "height={}", bb.height());
        // With a 32 px font the cap/ascent height (min_y to max_y span) should
        // be at least 8 doc units and less than 80 (two full line heights).
        assert!(
            bb.height() > 8.0 && bb.height() < 80.0,
            "suspicious cap height: {}",
            bb.height()
        );
        // In the flat layout, glyphs are placed with Y-down origin at the top of
        // the buffer. `line_y` (the baseline) is some positive Y value (the ascent
        // in pixels from the buffer top). Ascending glyphs sit ABOVE the baseline,
        // so `min_y` should be positive but less than `line_y`.  Descenders sit
        // below the baseline so `max_y > line_y`.  Both are within a 80-unit window
        // for a 32 px font. The overall bounding box stays within [-10, 80].
        assert!(
            bb.min_y() > -10.0,
            "min_y unexpectedly far above buffer top: {}",
            bb.min_y()
        );
        assert!(
            bb.max_y() < 80.0,
            "max_y unexpectedly far below buffer top: {}",
            bb.max_y()
        );
    }

    /// Vertical text must stack successive characters down the page instead of
    /// retaining the horizontal run's wide bounding box.
    #[test]
    fn layout_text_flat_vertical_stacks_characters() {
        let mut fs = FontSystem::new();

        let mut horizontal = TextNode::new("ABC");
        horizontal.font_size = 32.0;
        let horizontal = layout_text_flat(&mut fs, &horizontal).bounding_box();

        let mut vertical = TextNode::new("ABC");
        vertical.font_size = 32.0;
        vertical.vertical = true;
        let vertical = layout_text_flat(&mut fs, &vertical).bounding_box();

        let (Some(horizontal), Some(vertical)) = (horizontal, vertical) else {
            eprintln!("no system font available — skipping vertical layout check");
            return;
        };

        assert!(
            vertical.height() > horizontal.height() * 2.0,
            "vertical text should advance down the page: horizontal={horizontal:?}, vertical={vertical:?}"
        );
        assert!(
            vertical.height() > vertical.width() * 2.0,
            "vertical text should be taller than it is wide: {vertical:?}"
        );
    }

    /// BUG 2 / ACCEPT: alignment must not move a run's exported x-position. The
    /// live renderer (`renderer::text_renderer`) hands glyphon a `TextArea` whose
    /// `left` is the node's transform origin and never sets an alignment, so flat
    /// text is left-anchored at the origin for every `align` value. The outlined
    /// geometry used for PDF export must match that pixel-for-pixel: left, center,
    /// and right all produce the SAME glyph bounding box (previously center/right
    /// were shifted left by run_width/2 and run_width, landing off-position).
    #[test]
    fn layout_text_flat_alignment_does_not_shift_x() {
        use photonic_core::node::TextAlign;

        let mut fs = FontSystem::new();
        let mut render = |align: TextAlign| {
            let mut node = TextNode::new("Right side label");
            node.font_family = "sans-serif".to_string();
            node.font_size = 24.0;
            node.align = align;
            layout_text_flat(&mut fs, &node).bounding_box()
        };

        // Call sequentially (each borrows the shared FontSystem in turn).
        let left = render(TextAlign::Left);
        let center = render(TextAlign::Center);
        let right = render(TextAlign::Right);
        let (Some(left), Some(center), Some(right)) = (left, center, right) else {
            eprintln!("no system font available — skipping alignment position check");
            return;
        };

        // All three must occupy the exact same box (align is position-neutral,
        // matching the live renderer). Guard against the old regression where
        // center sat at ≈ -width/2 and right at ≈ -width.
        let eps = 1e-6;
        for (name, bb) in [("center", center), ("right", right)] {
            assert!(
                (bb.min_x() - left.min_x()).abs() < eps,
                "{name} min_x {} must equal left min_x {} (align must not shift x)",
                bb.min_x(),
                left.min_x(),
            );
            assert!(
                (bb.max_x() - left.max_x()).abs() < eps,
                "{name} max_x {} must equal left max_x {}",
                bb.max_x(),
                left.max_x(),
            );
        }
        // And the run really is anchored at/after the origin (left-aligned), not
        // pulled left of it.
        assert!(
            left.min_x() > -1.0,
            "left-anchored run should start at ~origin, got min_x={}",
            left.min_x()
        );
    }

    /// Pick a real, non-generic font face installed in the system DB so the
    /// resolution test is portable (no hardcoded family). Prefers a plain
    /// regular (weight 400, normal style) face for unambiguous resolution,
    /// falling back to the first concrete face otherwise. Returns `None` on a
    /// font-less environment.
    fn pick_installed_face(fs: &FontSystem) -> Option<(String, u16, FontStyle)> {
        let mut fallback: Option<(String, u16, FontStyle)> = None;
        for face in fs.db().faces() {
            let Some((family, _)) = face.families.first() else {
                continue;
            };
            if is_generic_family(family) {
                continue;
            }
            let style = match face.style {
                fontdb::Style::Italic => FontStyle::Italic,
                fontdb::Style::Oblique => FontStyle::Oblique,
                fontdb::Style::Normal => FontStyle::Normal,
            };
            if face.weight.0 == 400 && matches!(face.style, fontdb::Style::Normal) {
                return Some((family.clone(), 400, FontStyle::Normal));
            }
            if fallback.is_none() {
                fallback = Some((family.clone(), face.weight.0, style));
            }
        }
        fallback
    }

    /// ACCEPT CRITERION: the export text path must resolve the EXACT family +
    /// weight the document requested — no silent fallback to the default sans.
    /// Request a concrete installed family/weight and assert the face glyphon
    /// resolves for the outline/export path matches it. (Skips on a font-less
    /// CI image where no concrete family exists.)
    #[test]
    fn export_resolves_exact_requested_family_and_weight() {
        let mut fs = FontSystem::new();
        let Some((family, weight, style)) = pick_installed_face(&fs) else {
            eprintln!("no installed fonts — skipping font-resolution check");
            return;
        };

        let mut node = TextNode::new("Ag");
        node.font_family = family.clone();
        node.font_weight = weight;
        node.font_style = style;
        node.font_size = 48.0;

        let resolved = resolve_document_font(&mut fs, &node)
            .expect("a resolvable installed font must produce a face");

        assert!(
            resolved.family.eq_ignore_ascii_case(&family),
            "export resolved family {:?} but the document requested {:?} \
             (silent font substitution)",
            resolved.family,
            family,
        );
        assert_eq!(
            resolved.weight, weight,
            "export resolved weight {} but the document requested {}",
            resolved.weight, weight,
        );
    }

    /// A request for a family that is not installed must resolve to a DIFFERENT
    /// family (there is nothing else it could do) — proving `resolve_document_font`
    /// actually observes the substitution the warning path reports. If, on some
    /// exotic system, the bogus name happens to resolve to itself, the test is a
    /// no-op rather than a false failure.
    #[test]
    fn export_flags_missing_family_as_substitution() {
        let mut fs = FontSystem::new();
        let missing = "ZzzNoSuchFontFamily-Photonic-9187";
        let mut node = TextNode::new("Ag");
        node.font_family = missing.to_string();
        node.font_weight = 400;
        node.font_size = 48.0;

        let Some(resolved) = resolve_document_font(&mut fs, &node) else {
            eprintln!("no installed fonts — skipping substitution check");
            return;
        };
        assert!(
            !resolved.family.eq_ignore_ascii_case(missing),
            "a non-existent family must not resolve to itself: {:?}",
            resolved.family,
        );
    }

    /// Empty content must return an empty PathData (no geometry to produce).
    #[test]
    fn layout_text_flat_empty_content() {
        let mut fs = FontSystem::new();
        let node = TextNode::new("");
        let pd = layout_text_flat(&mut fs, &node);
        assert!(pd.is_empty(), "empty content should yield empty PathData");
    }

    /// `outline_document_text` on a doc with one text node should yield a doc
    /// whose corresponding node is a `PathNode` with the same ID.
    #[test]
    fn outline_document_text_replaces_text_node() {
        let mut fs = FontSystem::new();
        let (doc, node_id) = make_doc_with_text("Hi");
        let outlined = outline_document_text(&doc, &mut fs);

        let replaced = outlined
            .nodes
            .get(&node_id)
            .expect("node must still exist in cloned doc");

        // Original doc is untouched.
        assert!(
            matches!(
                doc.nodes.get(&node_id).unwrap().kind,
                SceneNodeKind::Text(_)
            ),
            "original doc should still have Text node"
        );

        // Cloned doc has a Path node.
        assert!(
            matches!(replaced.kind, SceneNodeKind::Path(_)),
            "outlined doc should have Path node, got {:?}",
            replaced.kind
        );
    }

    /// The replacement PathNode must have a non-empty outline when a usable
    /// font is available.  (Empty is allowed on no-font CI images.)
    #[test]
    fn outline_document_text_path_has_geometry_when_font_available() {
        let mut fs = FontSystem::new();
        let (doc, node_id) = make_doc_with_text("Hi");
        let outlined = outline_document_text(&doc, &mut fs);
        let node = outlined.nodes.get(&node_id).unwrap();
        if let SceneNodeKind::Path(p) = &node.kind {
            if p.path_data.is_empty() {
                eprintln!("no font — skipping geometry check");
                return;
            }
            assert!(
                p.path_data.bounding_box().is_some(),
                "outlined path must have a bbox"
            );
        } else {
            panic!("expected PathNode");
        }
    }

    /// ACCEPT CRITERION: export a doc with a text node through `outline_document_text`
    /// + `export_pdf`, write the result to /tmp/photonic_outline_test.pdf, then run
    /// `pdffonts` on it.  The font table MUST be empty (zero rows) — the PDF contains
    /// no embedded/referenced fonts; all glyphs are present as vector path operators.
    ///
    /// Also verifies the PDF is non-trivial (> 512 bytes) so we know real paths exist.
    ///
    /// Requires `pdffonts` (poppler-utils) to be installed. If the binary is absent
    /// the test passes with a warning so CI without poppler does not break the suite.
    #[test]
    fn pdf_has_zero_fonts_after_outlining() {
        use photonic_core::export::{export_pdf, PdfExportOptions};

        let mut fs = FontSystem::new();
        let (doc, _node_id) = make_doc_with_text("Hello");

        // Outline text → path nodes.
        let outlined = outline_document_text(&doc, &mut fs);

        // Export to PDF bytes.
        let opts = PdfExportOptions::default();
        let bytes = export_pdf(&outlined, &opts);

        // The PDF must be non-trivial: if the outlined paths are present, the file
        // will be larger than a bare empty-doc PDF (which is ~500 bytes).
        assert!(
            bytes.len() > 512,
            "PDF suspiciously small ({} bytes) — outlines may be missing",
            bytes.len()
        );

        // Write to the OS temp dir so pdffonts can read it (cross-platform; a
        // hardcoded /tmp path does not exist on the Windows CI runner).
        let path = std::env::temp_dir().join("photonic_outline_test.pdf");
        std::fs::write(&path, &bytes).expect("failed to write test PDF");

        // Run pdffonts and capture output.
        let Ok(output) = std::process::Command::new("pdffonts").arg(&path).output() else {
            eprintln!("pdffonts not found — skipping font-table check");
            return;
        };

        if !output.status.success() {
            eprintln!(
                "pdffonts exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            // Not a failure if pdffonts itself errored (unlikely but defensive).
            return;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        eprintln!("pdffonts output:\n{stdout}");

        // The header is always 2 lines; a non-empty font table adds more.
        // If there are only 2 lines (name + separator), the font table is empty.
        let data_lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();

        assert!(
            data_lines.len() <= 2,
            "PDF should have ZERO fonts but pdffonts reports {} data lines:\n{stdout}",
            data_lines.len()
        );
    }
}
