//! Caption data model (01 §7, verbatim).
//!
//! Word-level timing (CAP-009/010) is first-class: a cue owns a `Vec` of words
//! each with its own `[start, end]`, and style cascades word → cue → track. Line
//! breaks are deliberately NOT stored — wrapping is recomputed at render time
//! from `style.max_width` and font metrics (06 §3.5), so a later font/width
//! change re-flows automatically.

use super::ids::{CueId, TrackId};
use super::time::Tick;
use crate::Color;
use serde::{Deserialize, Serialize};

/// A caption lane on a sequence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaptionTrack {
    pub id: TrackId,
    pub name: String,
    /// Sorted, non-overlapping (invariant, enforced by edit ops).
    pub cues: Vec<CaptionCue>,
    /// Track-level default style (bottom of the cascade).
    pub style: CaptionStyle,
    #[serde(default = "super::grade::default_true")]
    pub enabled: bool,
}

impl CaptionTrack {
    pub fn new(name: impl Into<String>) -> Self {
        CaptionTrack {
            id: TrackId::new(),
            name: name.into(),
            cues: Vec::new(),
            style: CaptionStyle::default(),
            enabled: true,
        }
    }
}

/// One caption cue with word-level timing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaptionCue {
    pub id: CueId,
    pub start: Tick,
    pub end: Tick,
    pub words: Vec<CaptionWord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_override: Option<CaptionStyle>,
    /// Normalized sequence coords.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_override: Option<[f32; 2]>,
}

impl CaptionCue {
    pub fn new(start: Tick, end: Tick, words: Vec<CaptionWord>) -> Self {
        CaptionCue {
            id: CueId::new(),
            start,
            end,
            words,
            style_override: None,
            position_override: None,
        }
    }

    /// The cue's plain text. Words are joined with a single U+0020 **only where
    /// one belongs** (42 §6.5): no space is inserted on either side of a
    /// scriptio-continua cluster (CJK / Kana / Thai / …), so a cue tokenized per
    /// cluster re-joins seamlessly (`こんにちは`, not `こ ん に ち は`). For an
    /// all-ASCII cue this is byte-identical to the old `join(" ")`.
    pub fn text(&self) -> String {
        let mut out = String::new();
        let mut prev: Option<&str> = None;
        for w in &self.words {
            if let Some(p) = prev {
                if crate::text_metrics::needs_separator(p, &w.text) {
                    out.push(' ');
                }
            }
            out.push_str(&w.text);
            prev = Some(&w.text);
        }
        out
    }
}

/// A single word with its own timing (CAP-009/010).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaptionWord {
    pub text: String,
    pub start: Tick,
    pub end: Tick,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_override: Option<CaptionStyle>,
}

impl CaptionWord {
    pub fn new(text: impl Into<String>, start: Tick, end: Tick) -> Self {
        CaptionWord {
            text: text.into(),
            start,
            end,
            style_override: None,
        }
    }
}

/// Caption styling. Applied at track / cue / word scope; the cascade resolves
/// word → cue → track (01 §7, 06 §4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaptionStyle {
    pub font_family: String,
    pub font_size: f32,
    pub weight: u16,
    pub fill: Color,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<(Color, f32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<CaptionBackground>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlight: Option<KaraokeStyle>,
    /// Normalized position.
    pub position: [f32; 2],
    /// Normalized max width.
    pub max_width: f32,
    #[serde(default)]
    pub animation: CaptionAnim,
}

impl Default for CaptionStyle {
    fn default() -> Self {
        CaptionStyle {
            font_family: "sans-serif".to_string(),
            font_size: 48.0,
            weight: 700,
            fill: Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
            stroke: Some((
                Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
                2.0,
            )),
            background: None,
            highlight: None,
            position: [0.5, 0.85],
            max_width: 0.8,
            animation: CaptionAnim::None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaptionBackground {
    pub color: Color,
    pub corner_radius: f32,
    pub padding: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct KaraokeStyle {
    pub mode: KaraokeMode,
    pub active_color: Color,
    pub inactive_color: Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KaraokeMode {
    FillSweep,
    WordPop,
    Underline,
}

/// Per-word-timing-driven cue animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionAnim {
    #[default]
    None,
    FadeWords,
    SlideUp,
    Typewriter,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cue_text_joins_words() {
        let cue = CaptionCue::new(
            Tick(0),
            Tick(100),
            vec![
                CaptionWord::new("hello", Tick(0), Tick(50)),
                CaptionWord::new("world", Tick(50), Tick(100)),
            ],
        );
        assert_eq!(cue.text(), "hello world");
    }

    #[test]
    fn cue_text_omits_spaces_between_scriptio_continua_clusters() {
        // Per-cluster Japanese tokens re-join with no interpolated spaces
        // (42 §6.5) — the exported/rendered caption must read こんにちは.
        let ja = |c: &str| CaptionWord::new(c, Tick(0), Tick(1));
        let cue = CaptionCue::new(
            Tick(0),
            Tick(5),
            vec![
                ja("\u{3053}"),
                ja("\u{3093}"),
                ja("\u{306B}"),
                ja("\u{3061}"),
                ja("\u{306F}"),
            ],
        );
        assert_eq!(cue.text(), "\u{3053}\u{3093}\u{306B}\u{3061}\u{306F}"); // こんにちは

        // Mixed Latin + CJK: a space only where the seam is not scriptio-continua.
        let cue = CaptionCue::new(
            Tick(0),
            Tick(4),
            vec![
                CaptionWord::new("Photonic", Tick(0), Tick(1)),
                CaptionWord::new("\u{306F}", Tick(1), Tick(2)),
                CaptionWord::new("\u{901F}", Tick(2), Tick(3)),
                CaptionWord::new("\u{3044}", Tick(3), Tick(4)),
            ],
        );
        assert_eq!(cue.text(), "Photonic\u{306F}\u{901F}\u{3044}"); // Photonicは速い
    }

    #[test]
    fn caption_style_roundtrip() {
        let style = CaptionStyle {
            highlight: Some(KaraokeStyle {
                mode: KaraokeMode::FillSweep,
                active_color: Color {
                    r: 1.0,
                    g: 1.0,
                    b: 0.0,
                    a: 1.0,
                },
                inactive_color: Color {
                    r: 0.5,
                    g: 0.5,
                    b: 0.5,
                    a: 1.0,
                },
            }),
            ..CaptionStyle::default()
        };
        let j = serde_json::to_string(&style).unwrap();
        let back: CaptionStyle = serde_json::from_str(&j).unwrap();
        assert_eq!(style, back);
    }
}
