//! Parent abstraction for every canvas tool (#190).
//!
//! Each tool is a zero-sized type implementing [`CanvasTool`], giving one
//! uniform interface for tool metadata + lifecycle. The point is *sweeping
//! additions*: a new cross-tool capability (metadata field, activation
//! behaviour, …) is added once here and adopted per tool, instead of touching
//! the ~28 scattered `match self.active_tool` sites. [`tool_for`] resolves a
//! [`Tool`] enum value to its implementation; because the registry is generated
//! by an exhaustive match, adding a `Tool` variant fails to compile until it is
//! registered here.

use super::Tool;
use crate::app::PhotonicApp;

/// The common interface every canvas tool implements. Metadata methods default
/// to the matching [`Tool`] enum data; behavioural hooks default to no-ops so a
/// new capability can be introduced here and overridden only by the tools that
/// need it.
pub trait CanvasTool {
    /// The enum variant this tool corresponds to.
    fn kind(&self) -> Tool;

    /// Short toolbar label.
    fn label(&self) -> &'static str {
        self.kind().label()
    }
    /// Phosphor icon glyph.
    fn icon(&self) -> &'static str {
        self.kind().icon()
    }
    /// One-line tooltip / help text.
    fn description(&self) -> &'static str {
        self.kind().description()
    }
    /// Whether the tool drags out a new primitive shape.
    fn is_shape_creator(&self) -> bool {
        self.kind().is_shape_creator()
    }

    /// Called once when this tool becomes active, after the previous tool's
    /// [`on_deactivate`](CanvasTool::on_deactivate). Central seam for cross-tool
    /// activation behaviour (reset transient state, prime cursors, …).
    fn on_activate(&self, _app: &mut PhotonicApp) {}

    /// Called once when switching away from this tool.
    fn on_deactivate(&self, _app: &mut PhotonicApp) {}
}

macro_rules! tool_registry {
    ($($variant:ident => $ty:ident),+ $(,)?) => {
        $(
            /// Zero-sized [`CanvasTool`] implementation.
            pub struct $ty;
            impl CanvasTool for $ty {
                fn kind(&self) -> Tool { Tool::$variant }
            }
        )+

        /// Resolve a [`Tool`] to its [`CanvasTool`] implementation. The match is
        /// exhaustive, so a new `Tool` variant must be registered here to compile.
        pub fn tool_for(kind: Tool) -> &'static dyn CanvasTool {
            match kind {
                $( Tool::$variant => &$ty, )+
            }
        }

        #[cfg(test)]
        const ALL_TOOLS: &[Tool] = &[ $( Tool::$variant ),+ ];
    };
}

tool_registry! {
    Select => SelectTool,
    DirectSelect => DirectSelectTool,
    ProportionalMove => ProportionalMoveTool,
    Pan => PanTool,
    Rectangle => RectangleTool,
    RoundedRect => RoundedRectTool,
    Ellipse => EllipseTool,
    Polygon => PolygonTool,
    Star => StarTool,
    Spiral => SpiralTool,
    Line => LineTool,
    Arc => ArcTool,
    Grid => GridTool,
    PolarGrid => PolarGridTool,
    Pen => PenTool,
    ShapeBuilder => ShapeBuilderTool,
    Text => TextTool,
    Scissors => ScissorsTool,
    Knife => KnifeTool,
    Eraser => EraserTool,
    MagicWand => MagicWandTool,
    Lasso => LassoTool,
    Pencil => PencilTool,
    Smooth => SmoothTool,
    Width => WidthTool,
    RasterBrush => RasterBrushTool,
    RasterEraser => RasterEraserTool,
    AreaTrace => AreaTraceTool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_covers_every_tool_and_metadata_matches_enum() {
        for &t in ALL_TOOLS {
            let tool = tool_for(t);
            assert_eq!(tool.kind(), t, "registry maps to the wrong kind");
            assert_eq!(tool.label(), t.label());
            assert_eq!(tool.icon(), t.icon());
            assert_eq!(tool.description(), t.description());
            assert_eq!(tool.is_shape_creator(), t.is_shape_creator());
        }
    }
}
