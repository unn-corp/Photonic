use egui_phosphor::regular as ph;
use photonic_core::PrimitiveKind;

mod tool_trait;
pub use tool_trait::{tool_for, CanvasTool};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Tool {
    #[default]
    Select,
    DirectSelect,
    /// Direct Select sub-variant: dragging an anchor pulls neighbours within a
    /// falloff radius (Blender-style proportional editing).
    ProportionalMove,
    Pan,
    Rectangle,
    RoundedRect,
    Ellipse,
    Polygon,
    Star,
    Spiral,
    Line,
    Arc,
    Grid,
    PolarGrid,
    Pen,
    ShapeBuilder,
    Text,
    Scissors,
    /// Freehand cut: slice filled paths into separate faces along a drawn line.
    Knife,
    /// Vector eraser: drag a circular head to boolean-subtract from path art.
    Eraser,
    MagicWand,
    Lasso,
    Pencil,
    Smooth,
    /// Shape variable-width strokes by dragging width handles on a path.
    Width,
    /// Paint pixels onto the active raster layer.
    RasterBrush,
    /// Erase pixels from the active raster layer.
    RasterEraser,
    /// Drag a region over raster artwork and convert it to editable vectors.
    AreaTrace,
}

impl Tool {
    pub const fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::DirectSelect => "Direct Select",
            Tool::ProportionalMove => "Proportional Move",
            Tool::Pan => "Pan",
            Tool::Rectangle => "Rect",
            Tool::RoundedRect => "Rounded Rect",
            Tool::Ellipse => "Ellipse",
            Tool::Polygon => "Polygon",
            Tool::Star => "Star",
            Tool::Spiral => "Spiral",
            Tool::Line => "Line",
            Tool::Arc => "Arc",
            Tool::Grid => "Grid",
            Tool::PolarGrid => "Polar Grid",
            Tool::Pen => "Pen",
            Tool::ShapeBuilder => "Shape Builder",
            Tool::Text => "Text",
            Tool::Scissors => "Scissors",
            Tool::Knife => "Knife",
            Tool::Eraser => "Vector Eraser",
            Tool::MagicWand => "Magic Wand",
            Tool::Lasso => "Lasso",
            Tool::Pencil => "Pencil",
            Tool::Smooth => "Smooth",
            Tool::Width => "Width",
            Tool::RasterBrush => "Brush",
            Tool::RasterEraser => "Eraser",
            Tool::AreaTrace => "Area Trace",
        }
    }

    pub const fn icon(self) -> &'static str {
        match self {
            Tool::Select => ph::CURSOR,
            Tool::DirectSelect => ph::VECTOR_TWO,
            Tool::ProportionalMove => ph::MAGNET,
            Tool::Pan => ph::HAND,
            Tool::Rectangle => ph::RECTANGLE,
            Tool::RoundedRect => ph::RECTANGLE,
            Tool::Ellipse => ph::CIRCLE,
            Tool::Polygon => ph::POLYGON,
            Tool::Star => ph::STAR,
            Tool::Spiral => ph::SPIRAL,
            Tool::Line => ph::LINE_SEGMENT,
            Tool::Arc => ph::CIRCLE_HALF,
            Tool::Grid => ph::GRID_FOUR,
            Tool::PolarGrid => ph::CIRCLES_THREE,
            Tool::Pen => ph::PEN_NIB,
            Tool::ShapeBuilder => ph::UNITE,
            Tool::Text => ph::TEXT_T,
            Tool::Scissors => ph::SCISSORS,
            Tool::Knife => ph::KNIFE,
            Tool::Eraser => ph::ERASER,
            Tool::MagicWand => ph::MAGIC_WAND,
            Tool::Lasso => ph::LASSO,
            Tool::Pencil => ph::PENCIL,
            Tool::Smooth => ph::WAVE_SINE,
            Tool::Width => ph::ARROWS_VERTICAL,
            Tool::RasterBrush => ph::PAINT_BRUSH,
            Tool::RasterEraser => ph::ERASER,
            Tool::AreaTrace => ph::VECTOR_THREE,
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Tool::Select => "Select and move objects",
            Tool::DirectSelect => "Edit individual anchor points",
            Tool::ProportionalMove => {
                "Drag an anchor to pull neighbours along a falloff radius (scroll: spread, Shift+scroll: curve)"
            }
            Tool::Pan => "Pan the canvas view",
            Tool::Rectangle => "Draw rectangles and squares",
            Tool::RoundedRect => "Draw rectangles with rounded corners",
            Tool::Ellipse => "Draw ellipses and circles",
            Tool::Polygon => "Draw regular polygons",
            Tool::Star => "Draw star shapes",
            Tool::Spiral => "Draw Archimedean spirals",
            Tool::Line => "Draw straight line segments with precise length and angle",
            Tool::Arc => "Draw open or closed arcs with configurable sweep angle",
            Tool::Grid => "Draw a rectangular grid with configurable rows and columns",
            Tool::PolarGrid => {
                "Draw a polar (radial) grid with concentric rings and radial sectors"
            }
            Tool::Pen => "Draw freeform paths with bezier curves",
            Tool::ShapeBuilder => "Combine or subtract overlapping shapes",
            Tool::Text => "Add text to the canvas",
            Tool::Scissors => "Cut a path at any point, splitting it into two open paths",
            Tool::Knife => {
                "Drag a freehand line across filled paths to slice them into separate faces"
            }
            Tool::Eraser => {
                "Drag a circular eraser head to subtract a swept region from path artwork"
            }
            Tool::MagicWand => {
                "Select all objects sharing a similar attribute (fill, stroke, opacity…)"
            }
            Tool::Lasso => "Drag a freehand lasso to select all objects within the enclosed region",
            Tool::Pencil => {
                "Draw freehand paths by dragging — anchor points auto-generated along the stroke"
            }
            Tool::Smooth => "Drag over a path to smooth its anchor points using corner-cutting",
            Tool::Width => {
                "Drag width handles on a stroke to shape a variable-width profile (Alt: one side)"
            }
            Tool::RasterBrush => "Paint pixels onto the selected raster layer by dragging",
            Tool::RasterEraser => "Erase pixels from the selected raster layer by dragging",
            Tool::AreaTrace => {
                "Drag over raster artwork to create a simplified editable vector group"
            }
        }
    }

    pub fn is_shape_creator(self) -> bool {
        !matches!(
            self,
            Tool::Select
                | Tool::DirectSelect
                | Tool::ProportionalMove
                | Tool::Pan
                | Tool::Pen
                | Tool::ShapeBuilder
                | Tool::Text
                | Tool::Scissors
                | Tool::Knife
                | Tool::Eraser
                | Tool::MagicWand
                | Tool::Lasso
                | Tool::Pencil
                | Tool::Smooth
                | Tool::Width
                | Tool::RasterBrush
                | Tool::RasterEraser
                | Tool::AreaTrace
        )
    }

    pub fn from_primitive(p: PrimitiveKind) -> Self {
        match p {
            PrimitiveKind::Rectangle => Tool::Rectangle,
            PrimitiveKind::RoundedRect => Tool::RoundedRect,
            PrimitiveKind::Ellipse => Tool::Ellipse,
            PrimitiveKind::Polygon => Tool::Polygon,
            PrimitiveKind::Star => Tool::Star,
            PrimitiveKind::Line => Tool::Line,
            PrimitiveKind::Arc => Tool::Arc,
        }
    }
}
