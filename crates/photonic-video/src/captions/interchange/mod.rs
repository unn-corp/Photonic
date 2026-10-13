//! Subtitle interchange: SRT/VTT/ASS import/export (06 §7).
//!
//! Hand-rolled rather than depending on `subparse` (the spec's first-choice
//! dependency): `subparse` is MPL-2.0 (a `[[licenses.clarify]]`-worthy
//! copyleft license this workspace's `deny.toml` does not allow without a new
//! exception) and, more fundamentally, has no WebVTT support at all — despite
//! this doc requiring VTT import/export — and no API for the word-level
//! extraction 06 §7.1 requires (VTT inline `<hh:mm:ss.mmm>` tags, ASS
//! `\k`/`\kf`/`\ko` tags); a custom layer on top would be needed regardless.
//! Hand-rolling the exact subset 06 §7's mapping tables specify is both
//! license-clean and less code than adapter-wrapping a mismatched dependency.
//!
//! `libass` is not a dependency either — rendering is `CaptionOverlay` IR
//! (06 §5), never libass compositing (06 §7, explicit).

pub mod ass;
pub mod srt;
pub mod vtt;

use photonic_core::timeline::Tick;

#[derive(Debug, thiserror::Error)]
pub enum InterchangeError {
    #[error("parse error: {0}")]
    Parse(String),
}

/// Non-blocking summary of what an import approximated or dropped —
/// surfaced to the user, never silent (06 §7.1's ASS-directive-drop rule,
/// generalized here since SRT/VTT can also hit the proportional-timing
/// fallback).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportSummary {
    pub cues_imported: usize,
    /// `true` if any cue's word timing was approximated (proportional
    /// distribution by character count) rather than sourced natively.
    pub any_words_approximated: bool,
    /// Free-form notes, e.g. `"3 styling directives dropped"` (ASS only).
    pub notes: Vec<String>,
}

/// Non-blocking summary of best-effort approximations made on export.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExportSummary {
    pub notes: Vec<String>,
}

/// The current subtitle writers do not serialize the explicit paragraph style.
/// Return one warning across track/cue/word scopes instead of silently losing it.
pub fn direction_export_notes(track: &photonic_core::timeline::CaptionTrack) -> Vec<String> {
    let explicit = std::iter::once(&track.style)
        .chain(track.cues.iter().filter_map(|c| c.style_override.as_ref()))
        .chain(
            track
                .cues
                .iter()
                .flat_map(|c| c.words.iter().filter_map(|w| w.style_override.as_ref())),
        )
        .any(|s| !s.direction.is_auto());
    if explicit {
        vec!["Explicit paragraph direction is not represented in this subtitle export; external players will choose their own text direction.".into()]
    } else {
        Vec::new()
    }
}

// ── Shared timestamp parsing (SRT `,` / VTT `.` millisecond timestamps) ────

/// Parse `[H]H:MM:SS<sep>mmm` (hours may be 1+ digits; VTT additionally
/// allows omitting hours entirely, `MM:SS<sep>mmm`).
pub(crate) fn parse_ms_timestamp(s: &str, frac_sep: char) -> Option<Tick> {
    let s = s.trim();
    let (hms, frac) = s.split_once(frac_sep)?;
    let parts: Vec<&str> = hms.split(':').collect();
    let (h, m, sec): (i64, i64, i64) = match parts.as_slice() {
        [h, m, s] => (h.parse().ok()?, m.parse().ok()?, s.parse().ok()?),
        [m, s] => (0, m.parse().ok()?, s.parse().ok()?),
        _ => return None,
    };
    let frac = frac.trim();
    if frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let ms: i64 = frac.parse().ok()?;
    let total_ms = (h * 3600 + m * 60 + sec) * 1000 + ms;
    Some(Tick(
        photonic_core::timeline::TICKS_PER_SECOND / 1000 * total_ms,
    ))
}

/// Format a [`Tick`] as `HH:MM:SS<sep>mmm`.
pub(crate) fn format_ms_timestamp(t: Tick, frac_sep: char) -> String {
    let ticks_per_ms = photonic_core::timeline::TICKS_PER_SECOND / 1000;
    let total_ms = t.0.max(0) / ticks_per_ms;
    let ms = total_ms % 1000;
    let total_sec = total_ms / 1000;
    let s = total_sec % 60;
    let total_min = total_sec / 60;
    let m = total_min % 60;
    let h = total_min / 60;
    format!("{h:02}:{m:02}:{s:02}{frac_sep}{ms:03}")
}

#[cfg(test)]
mod shared_tests {
    use super::*;

    #[test]
    fn ms_timestamp_round_trips() {
        let t =
            Tick::from_seconds(3661) + Tick(photonic_core::timeline::TICKS_PER_SECOND / 1000 * 250);
        let formatted = format_ms_timestamp(t, ',');
        assert_eq!(formatted, "01:01:01,250");
        assert_eq!(parse_ms_timestamp(&formatted, ',').unwrap(), t);
    }

    #[test]
    fn ms_timestamp_accepts_hourless_form() {
        assert_eq!(
            parse_ms_timestamp("01:02.500", '.').unwrap(),
            Tick(photonic_core::timeline::TICKS_PER_SECOND / 1000 * 62_500)
        );
    }

    #[test]
    fn ms_timestamp_rejects_garbage() {
        assert!(parse_ms_timestamp("not a timestamp", ',').is_none());
        assert!(parse_ms_timestamp("00:00:00", ',').is_none()); // no fractional part
    }
    #[test]
    fn explicit_caption_direction_loss_is_reported_for_subtitle_interchange() {
        let mut track = photonic_core::timeline::CaptionTrack::new("Directions");
        assert!(super::direction_export_notes(&track).is_empty());
        track.style.direction = photonic_core::timeline::TextDirection::Rtl;
        assert_eq!(super::direction_export_notes(&track).len(), 1);
        assert!(super::ass::write_ass(&track)
            .1
            .notes
            .iter()
            .any(|s| s.contains("paragraph direction")));
    }
}
