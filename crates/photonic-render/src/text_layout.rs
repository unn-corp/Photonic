//! Shared text shaping and layout setup for live rendering and outlines.

use glyphon::{Attrs, Buffer, FontSystem, Metrics, Shaping, Style as GlyphonStyle, Weight};
use photonic_core::node::FontStyle;

/// Layout settings shared by live, capture, and outline text rendering.
#[derive(Debug, Clone, Copy)]
pub struct TextLayoutOptions {
    /// Font weight (100–900).
    pub font_weight: u16,
    /// Font style.
    pub font_style: FontStyle,
    /// Line height multiplier.
    pub line_height_mul: f32,
    /// Additional advance between characters in vertical mode.
    pub letter_spacing: f32,
    /// Stack characters top-to-bottom when true.
    pub vertical: bool,
}

impl Default for TextLayoutOptions {
    fn default() -> Self {
        Self {
            font_weight: 400,
            font_style: FontStyle::Normal,
            line_height_mul: 1.2,
            letter_spacing: 0.0,
            vertical: false,
        }
    }
}

/// Shape text using the same font attributes and layout metrics in every
/// renderer path. Glyphon/cosmic-text has no writing-mode API, so vertical text
/// is represented as one character per line; this preserves glyphon rasterizing
/// for live text while exposing the same top-to-bottom runs to outline export.
pub(crate) fn layout_text_buffer(
    font_system: &mut FontSystem,
    content: &str,
    font_family: &str,
    font_size: f32,
    options: TextLayoutOptions,
) -> Buffer {
    let font_size = font_size.max(0.01);
    let line_height = (font_size * options.line_height_mul.max(0.1)).max(0.01);
    let line_height = if options.vertical {
        // In vertical mode letter spacing is the extra advance between the
        // successive character lines rather than horizontal glyph spacing.
        (line_height + options.letter_spacing).max(font_size * 0.1)
    } else {
        line_height
    };

    let mut buffer = Buffer::new(font_system, Metrics::new(font_size, line_height));
    buffer.set_size(font_system, None, None);

    let glyph_style = match options.font_style {
        FontStyle::Italic => GlyphonStyle::Italic,
        FontStyle::Oblique => GlyphonStyle::Oblique,
        FontStyle::Normal => GlyphonStyle::Normal,
    };
    let attrs = Attrs::new()
        .family(crate::text_outline::cosmic_family(font_family))
        .weight(Weight(options.font_weight))
        .style(glyph_style);
    let layout_content = if options.vertical {
        vertical_layout_content(content)
    } else {
        content.to_owned()
    };

    buffer.set_text(font_system, &layout_content, attrs, Shaping::Advanced);
    buffer.shape_until_scroll(font_system, false);
    buffer
}

fn vertical_layout_content(content: &str) -> String {
    let mut layout = String::with_capacity(content.len().saturating_mul(2));
    for (index, character) in content.chars().enumerate() {
        if index > 0 {
            layout.push('\n');
        }
        layout.push(character);
    }
    layout
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertical_layout_advances_down_while_horizontal_advances_right() {
        let mut font_system = FontSystem::new();
        let horizontal = layout_text_buffer(
            &mut font_system,
            "ABC",
            "sans-serif",
            32.0,
            TextLayoutOptions::default(),
        );
        let vertical = layout_text_buffer(
            &mut font_system,
            "ABC",
            "sans-serif",
            32.0,
            TextLayoutOptions {
                vertical: true,
                ..TextLayoutOptions::default()
            },
        );

        let horizontal_origins: Vec<(f32, f32)> = horizontal
            .layout_runs()
            .flat_map(|run| run.glyphs.iter().map(move |glyph| (glyph.x, run.line_y)))
            .collect();
        let vertical_origins: Vec<(f32, f32)> = vertical
            .layout_runs()
            .flat_map(|run| run.glyphs.iter().map(move |glyph| (glyph.x, run.line_y)))
            .collect();

        if horizontal_origins.len() < 3 || vertical_origins.len() < 3 {
            eprintln!("no system font available — skipping shared layout check");
            return;
        }

        assert!(
            horizontal_origins[2].0 > horizontal_origins[0].0,
            "horizontal glyph origins should advance along x: {horizontal_origins:?}"
        );
        assert!(
            vertical_origins[2].1 > vertical_origins[0].1,
            "vertical glyph origins should advance along y: {vertical_origins:?}"
        );
        let vertical_x = vertical_origins[0].0;
        assert!(
            vertical_origins
                .iter()
                .all(|(x, _)| (*x - vertical_x).abs() < 0.001),
            "vertical glyph origins should share x: {vertical_origins:?}"
        );
    }
}
