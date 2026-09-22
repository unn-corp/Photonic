//! Starter title presets shared by the GUI and MCP.

use super::{CaptionBackground, CaptionStyle};
use crate::Color;

/// One starter title preset. 05 §4b's shipped set is `VectorDoc`-templated
/// with entrance/exit keyframes; this is the plain-`ClipSource::Text` subset
/// of it (see module doc's scope cut) — three placements that cover the
/// common cases (bottom-anchored name/role, dead-center hero text, a boxed
/// card).
pub struct TitlePreset {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub sample_text: &'static str,
    pub duration_secs: i64,
    pub style: fn() -> CaptionStyle,
}

fn lower_third_style() -> CaptionStyle {
    CaptionStyle {
        font_size: 40.0,
        position: [0.07, 0.84],
        max_width: 0.5,
        stroke: None,
        background: Some(CaptionBackground {
            color: Color::new(0.0, 0.0, 0.0, 0.55),
            corner_radius: 4.0,
            padding: 12.0,
        }),
        ..CaptionStyle::default()
    }
}

fn centered_title_style() -> CaptionStyle {
    CaptionStyle {
        font_size: 96.0,
        weight: 800,
        position: [0.5, 0.5],
        max_width: 0.8,
        ..CaptionStyle::default()
    }
}

fn caption_card_style() -> CaptionStyle {
    CaptionStyle {
        font_size: 34.0,
        weight: 600,
        position: [0.5, 0.5],
        max_width: 0.6,
        stroke: None,
        background: Some(CaptionBackground {
            color: Color::new(0.08, 0.08, 0.12, 0.85),
            corner_radius: 10.0,
            padding: 16.0,
        }),
        ..CaptionStyle::default()
    }
}

pub const TITLE_PRESETS: &[TitlePreset] = &[
    TitlePreset {
        name: "Lower Third",
        id: "lower_third",
        description: "Name / role bar anchored bottom-left — the classic interview caption.",
        sample_text: "Name Here\nRole / Title",
        duration_secs: 5,
        style: lower_third_style,
    },
    TitlePreset {
        name: "Centered Title",
        id: "centered_title",
        description: "Large hero text, dead center — cold opens and chapter cards.",
        sample_text: "Title Goes Here",
        duration_secs: 4,
        style: centered_title_style,
    },
    TitlePreset {
        name: "Caption Card",
        id: "caption_card",
        description: "Boxed text card for callouts, quotes, and CTAs.",
        sample_text: "Caption text goes here.",
        duration_secs: 4,
        style: caption_card_style,
    },
];
