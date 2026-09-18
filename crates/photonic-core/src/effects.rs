//! Layer Styles — a reorderable, non-destructive effect stack (Photoshop parity).
//!
//! A node (and, later, a layer) carries an ordered `Vec<LayerEffect>`. Effects
//! composite in list order over the element's silhouette/fill, each with its own
//! `enabled` flag, blend mode and opacity. The older fixed-field effects
//! (`drop_shadow`, `outer_glow`, `inner_glow`, `gaussian_glow` on [`SceneNode`])
//! are migrated into this stack on load; `object_blur`/`feather` remain separate
//! (they distort the shape itself rather than adding a style layer).
//!
//! [`SceneNode`]: crate::node::SceneNode
use serde::{Deserialize, Serialize};

use crate::color::Color;
use crate::layer::BlendMode;
use crate::node::{DropShadow, GlowEffect};
use crate::raster::image::RasterImage;
use crate::style::{Fill, Gradient, GradientStop, PatternFill, StrokeAlign};

/// One entry in a node's/layer's Layer Styles stack. Composited in list order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayerEffect {
    DropShadow(DropShadow),
    InnerShadow(InnerShadow),
    OuterGlow(GlowEffect),
    InnerGlow(GlowEffect),
    Bevel(BevelEmboss),
    Satin(Satin),
    ColorOverlay(ColorOverlay),
    GradientOverlay(GradientOverlay),
    PatternOverlay(PatternOverlay),
    Stroke(StrokeEffect),
}

impl LayerEffect {
    /// Whether this effect is currently enabled (drawn).
    pub fn enabled(&self) -> bool {
        match self {
            LayerEffect::DropShadow(e) => e.enabled,
            LayerEffect::InnerShadow(e) => e.enabled,
            LayerEffect::OuterGlow(e) => e.enabled,
            LayerEffect::InnerGlow(e) => e.enabled,
            LayerEffect::Bevel(e) => e.enabled,
            LayerEffect::Satin(e) => e.enabled,
            LayerEffect::ColorOverlay(e) => e.enabled,
            LayerEffect::GradientOverlay(e) => e.enabled,
            LayerEffect::PatternOverlay(e) => e.enabled,
            LayerEffect::Stroke(e) => e.enabled,
        }
    }

    /// A short, stable kind label (for UI + the MCP API).
    pub fn kind(&self) -> &'static str {
        match self {
            LayerEffect::DropShadow(_) => "drop_shadow",
            LayerEffect::InnerShadow(_) => "inner_shadow",
            LayerEffect::OuterGlow(_) => "outer_glow",
            LayerEffect::InnerGlow(_) => "inner_glow",
            LayerEffect::Bevel(_) => "bevel",
            LayerEffect::Satin(_) => "satin",
            LayerEffect::ColorOverlay(_) => "color_overlay",
            LayerEffect::GradientOverlay(_) => "gradient_overlay",
            LayerEffect::PatternOverlay(_) => "pattern_overlay",
            LayerEffect::Stroke(_) => "stroke",
        }
    }
}

/// Inner shadow: an offset, blurred dark edge composited *inside* the shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InnerShadow {
    pub enabled: bool,
    pub color: Color,
    pub opacity: f32,
    #[serde(default)]
    pub blend_mode: BlendMode,
    /// Offset in document units (positive dx = right, dy = down).
    pub dx: f32,
    pub dy: f32,
    /// Blur radius (sigma) in document units; 0 = hard edge.
    pub blur: f32,
}

impl Default for InnerShadow {
    fn default() -> Self {
        Self {
            enabled: true,
            color: Color::new(0.0, 0.0, 0.0, 1.0),
            opacity: 0.5,
            blend_mode: BlendMode::Multiply,
            dx: 4.0,
            dy: 4.0,
            blur: 6.0,
        }
    }
}

/// Solid colour overlay filling the shape's silhouette.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorOverlay {
    pub enabled: bool,
    pub color: Color,
    pub opacity: f32,
    #[serde(default)]
    pub blend_mode: BlendMode,
}

impl Default for ColorOverlay {
    fn default() -> Self {
        Self {
            enabled: true,
            color: Color::new(0.85, 0.1, 0.1, 1.0),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
        }
    }
}

/// Gradient overlay filling the shape's silhouette.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GradientOverlay {
    pub enabled: bool,
    pub gradient: Gradient,
    pub opacity: f32,
    #[serde(default)]
    pub blend_mode: BlendMode,
    /// Gradient angle in degrees (for linear gradients).
    pub angle: f32,
    /// Scale (1.0 = fit the shape's bounding box).
    pub scale: f32,
}

impl Default for GradientOverlay {
    fn default() -> Self {
        Self {
            enabled: true,
            gradient: Gradient::linear(
                0.0,
                0.0,
                0.0,
                1.0,
                vec![
                    GradientStop::new(0.0, Color::new(0.0, 0.0, 0.0, 1.0)),
                    GradientStop::new(1.0, Color::new(1.0, 1.0, 1.0, 1.0)),
                ],
            ),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            angle: 90.0,
            scale: 1.0,
        }
    }
}

/// Pattern overlay tiling a pattern fill over the shape's silhouette.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatternOverlay {
    pub enabled: bool,
    pub pattern: PatternFill,
    pub opacity: f32,
    #[serde(default)]
    pub blend_mode: BlendMode,
    pub scale: f32,
}

impl Default for PatternOverlay {
    fn default() -> Self {
        Self {
            enabled: true,
            pattern: PatternFill::new(RasterImage::filled(8, 8, [200, 200, 200, 255])),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            scale: 1.0,
        }
    }
}

/// Stroke effect: a paint outline hugging the shape edge (inside/centre/outside).
/// Independent of a `PathNode`'s own stroke, and stackable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrokeEffect {
    pub enabled: bool,
    /// Stroke width in document units.
    pub width: f32,
    /// Where the stroke sits relative to the shape edge.
    pub position: StrokeAlign,
    /// The stroke paint (solid / gradient / pattern via `Fill`).
    pub fill: Fill,
    pub opacity: f32,
    #[serde(default)]
    pub blend_mode: BlendMode,
}

impl Default for StrokeEffect {
    fn default() -> Self {
        Self {
            enabled: true,
            width: 3.0,
            position: StrokeAlign::Outside,
            fill: Fill::solid(Color::new(0.1, 0.1, 0.1, 1.0)),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
        }
    }
}

/// Satin: an interior shading pass derived from the shape, offset and blurred,
/// giving a soft satin/sheen look.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Satin {
    pub enabled: bool,
    pub color: Color,
    pub opacity: f32,
    #[serde(default)]
    pub blend_mode: BlendMode,
    /// Angle in degrees.
    pub angle: f32,
    /// Offset distance in document units.
    pub distance: f32,
    /// Blur size in document units.
    pub size: f32,
    /// Invert the satin shape.
    pub invert: bool,
}

impl Default for Satin {
    fn default() -> Self {
        Self {
            enabled: true,
            color: Color::new(0.0, 0.0, 0.0, 1.0),
            opacity: 0.5,
            blend_mode: BlendMode::Multiply,
            angle: 19.0,
            distance: 11.0,
            size: 14.0,
            invert: true,
        }
    }
}

/// Bevel & Emboss style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BevelStyle {
    #[default]
    OuterBevel,
    InnerBevel,
    Emboss,
    PillowEmboss,
    StrokeEmboss,
}

/// Bevel & Emboss: a lit 3D edge derived from a distance/height field of the
/// shape, producing a highlight and a shadow along the edge per the global light.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BevelEmboss {
    pub enabled: bool,
    pub style: BevelStyle,
    /// Edge depth (0..~10, higher = steeper light response).
    pub depth: f32,
    /// Edge size in document units.
    pub size: f32,
    /// Soften/blur the lit edge, in document units.
    pub soften: f32,
    /// Light angle in degrees.
    pub angle: f32,
    /// Light altitude in degrees (0 = grazing, 90 = overhead).
    pub altitude: f32,
    pub highlight_color: Color,
    pub highlight_opacity: f32,
    #[serde(default)]
    pub highlight_blend: BlendMode,
    pub shadow_color: Color,
    pub shadow_opacity: f32,
    #[serde(default)]
    pub shadow_blend: BlendMode,
}

impl Default for BevelEmboss {
    fn default() -> Self {
        Self {
            enabled: true,
            style: BevelStyle::InnerBevel,
            depth: 1.0,
            size: 5.0,
            soften: 0.0,
            angle: 120.0,
            altitude: 30.0,
            highlight_color: Color::new(1.0, 1.0, 1.0, 1.0),
            highlight_opacity: 0.75,
            highlight_blend: BlendMode::Screen,
            shadow_color: Color::new(0.0, 0.0, 0.0, 1.0),
            shadow_opacity: 0.75,
            shadow_blend: BlendMode::Multiply,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_stack_serde_round_trips() {
        let stack = vec![
            LayerEffect::DropShadow(DropShadow::default()),
            LayerEffect::ColorOverlay(ColorOverlay::default()),
            LayerEffect::Stroke(StrokeEffect::default()),
            LayerEffect::Bevel(BevelEmboss::default()),
            LayerEffect::Satin(Satin::default()),
            LayerEffect::InnerShadow(InnerShadow::default()),
        ];
        let json = serde_json::to_string(&stack).expect("serialize");
        let back: Vec<LayerEffect> = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(stack, back);
        // The tagged representation carries a stable kind string.
        assert!(json.contains("\"type\":\"drop_shadow\""));
        assert!(json.contains("\"type\":\"color_overlay\""));
    }

    #[test]
    fn kind_and_enabled_are_consistent() {
        let e = LayerEffect::Stroke(StrokeEffect::default());
        assert_eq!(e.kind(), "stroke");
        assert!(e.enabled());
        let cs = ColorOverlay {
            enabled: false,
            ..Default::default()
        };
        assert!(!LayerEffect::ColorOverlay(cs).enabled());
    }
}
