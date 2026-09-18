//! WebVTT import/export (06 §7.1/§7.2).
//!
//! Unlike SRT, VTT can carry real per-word timing via inline `<hh:mm:ss.mmm>`
//! tags (`Hello <00:00:01.500>world`); when present, import extracts real
//! word boundaries instead of falling back to proportional distribution.
//! `NOTE`/`STYLE`/`REGION` blocks are skipped (not representable in
//! `CaptionStyle` v1 — cue text/timing only, matching 06 §7.1's scope).

use photonic_core::timeline::{CaptionCue, CaptionWord, Tick};

use super::{format_ms_timestamp, parse_ms_timestamp, ImportSummary, InterchangeError};
use crate::captions::proportional::distribute_caption_words;

enum Seg {
    Text(String),
    Time(Tick),
    OtherTag,
}

/// Tokenize cue text into text runs, inline timestamp tags, and other
/// (dropped) tags like `<b>`/`<c.classname>`/`<v Speaker>`.
fn tokenize(text: &str) -> Vec<Seg> {
    let mut segs = Vec::new();
    let mut buf = String::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(rel_end) = text[i..].find('>') {
                let inner = &text[i + 1..i + rel_end];
                if !buf.is_empty() {
                    segs.push(Seg::Text(std::mem::take(&mut buf)));
                }
                match parse_ms_timestamp(inner, '.') {
                    Some(t) => segs.push(Seg::Time(t)),
                    None => segs.push(Seg::OtherTag),
                }
                i += rel_end + 1;
                continue;
            }
        }
        let ch = text[i..].chars().next().unwrap();
        buf.push(ch);
        i += ch.len_utf8();
    }
    if !buf.is_empty() {
        segs.push(Seg::Text(buf));
    }
    segs
}

fn plain_text(segs: &[Seg]) -> String {
    segs.iter()
        .filter_map(|s| match s {
            Seg::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Build words for one cue's raw text. Returns `(words, approximated)`.
fn words_from_cue_text(text: &str, start: Tick, end: Tick) -> (Vec<CaptionWord>, bool) {
    let segs = tokenize(text);
    if !segs.iter().any(|s| matches!(s, Seg::Time(_))) {
        return (
            distribute_caption_words(&plain_text(&segs), start, end),
            true,
        );
    }

    // Real per-word timing: each `<time>` tag marks the start of the text
    // run that follows it; the run's end is the next tag's time (or the
    // cue's end for the trailing run). Text before the first tag (if any)
    // is anchored at the cue's own start.
    let mut runs: Vec<(Tick, String)> = Vec::new();
    let mut cursor = start;
    let mut buf = String::new();
    for seg in segs {
        match seg {
            Seg::Text(t) => buf.push_str(&t),
            Seg::OtherTag => {}
            Seg::Time(t) => {
                if !buf.trim().is_empty() {
                    runs.push((cursor, std::mem::take(&mut buf)));
                } else {
                    buf.clear();
                }
                cursor = t;
            }
        }
    }
    if !buf.trim().is_empty() {
        runs.push((cursor, buf));
    }

    let mut words = Vec::new();
    for (i, (run_start, run_text)) in runs.iter().enumerate() {
        let run_end = runs.get(i + 1).map(|(t, _)| *t).unwrap_or(end);
        let run_end = if run_end.0 < run_start.0 {
            *run_start
        } else {
            run_end
        };
        words.extend(distribute_caption_words(
            run_text.trim(),
            *run_start,
            run_end,
        ));
    }
    (words, false)
}

pub fn parse_vtt(input: &str) -> Result<(Vec<CaptionCue>, ImportSummary), InterchangeError> {
    let normalized = input.replace("\r\n", "\n");
    let mut lines = normalized.lines();
    let header = lines.next().unwrap_or("");
    if !header.trim_start().starts_with("WEBVTT") {
        return Err(InterchangeError::Parse("missing WEBVTT header".to_string()));
    }

    let mut cues = Vec::new();
    let mut summary = ImportSummary::default();
    let rest: String = lines.collect::<Vec<_>>().join("\n");

    for block in rest.split("\n\n") {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        let first_word = block.split_whitespace().next().unwrap_or("");
        if first_word == "NOTE" || first_word == "STYLE" || first_word == "REGION" {
            continue;
        }

        let mut block_lines = block.lines();
        let first = block_lines.next().unwrap_or("");
        let timestamp_line = if first.contains("-->") {
            first
        } else {
            match block_lines.next() {
                Some(l) if l.contains("-->") => l,
                _ => continue, // malformed/unsupported block: skip leniently
            }
        };
        let Some((start_str, rest_ts)) = timestamp_line.split_once("-->") else {
            continue;
        };
        let end_token = rest_ts.split_whitespace().next().unwrap_or("");
        let (Some(start), Some(end)) = (
            parse_ms_timestamp(start_str.trim(), '.'),
            parse_ms_timestamp(end_token, '.'),
        ) else {
            continue;
        };

        let text_lines: Vec<&str> = block_lines.collect();
        let raw_text = text_lines.join("\n");
        let (words, approximated) = words_from_cue_text(&raw_text, start, end);
        if words.is_empty() {
            continue;
        }
        summary.any_words_approximated |= approximated;
        cues.push(CaptionCue::new(start, end, words));
    }

    summary.cues_imported = cues.len();
    Ok((cues, summary))
}

/// `with_word_timestamps`: emit per-word inline `<hh:mm:ss.mmm>` tags
/// (06 §7.2: "optional, off by default").
pub fn write_vtt(cues: &[CaptionCue], with_word_timestamps: bool) -> String {
    let mut out = String::from("WEBVTT\n");
    for cue in cues {
        out.push('\n');
        out.push_str(&format_ms_timestamp(cue.start, '.'));
        out.push_str(" --> ");
        out.push_str(&format_ms_timestamp(cue.end, '.'));
        out.push('\n');
        if with_word_timestamps {
            for (i, w) in cue.words.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                out.push('<');
                out.push_str(&format_ms_timestamp(w.start, '.'));
                out.push('>');
                out.push_str(&w.text);
            }
        } else {
            out.push_str(&cue.text());
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::Tick;

    #[test]
    fn parses_cue_without_word_timestamps_via_proportional_fallback() {
        let input = "WEBVTT\n\n00:00:01.000 --> 00:00:04.000\nHello world\n";
        let (cues, summary) = parse_vtt(input).unwrap();
        assert_eq!(cues.len(), 1);
        assert!(summary.any_words_approximated);
        assert_eq!(cues[0].text(), "Hello world");
    }

    #[test]
    fn parses_inline_word_timestamps_as_real_per_word_timing() {
        let input = "WEBVTT\n\n00:00:01.000 --> 00:00:04.000\nHello <00:00:02.000>world\n";
        let (cues, summary) = parse_vtt(input).unwrap();
        assert_eq!(cues.len(), 1);
        assert!(!summary.any_words_approximated);
        let words = &cues[0].words;
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "Hello");
        assert_eq!(words[0].start, Tick::from_seconds(1));
        assert_eq!(words[0].end, Tick::from_seconds(2));
        assert_eq!(words[1].text, "world");
        assert_eq!(words[1].start, Tick::from_seconds(2));
        assert_eq!(words[1].end, Tick::from_seconds(4));
    }

    #[test]
    fn strips_non_timestamp_tags() {
        let input = "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\n<b>Hello</b> <c.yellow>world</c>\n";
        let (cues, _) = parse_vtt(input).unwrap();
        assert_eq!(cues[0].text(), "Hello world");
    }

    #[test]
    fn skips_note_and_style_blocks() {
        let input = "WEBVTT\n\nNOTE this is a comment\nspanning lines\n\n00:00:00.000 --> 00:00:01.000\nActual cue\n";
        let (cues, _) = parse_vtt(input).unwrap();
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text(), "Actual cue");
    }

    #[test]
    fn rejects_missing_header() {
        assert!(parse_vtt("00:00:00.000 --> 00:00:01.000\nNo header\n").is_err());
    }

    #[test]
    fn round_trips_plain_export_through_parse() {
        let input = "WEBVTT\n\n00:00:01.000 --> 00:00:04.000\nHello world\n\n00:00:05.000 --> 00:00:06.500\nSecond cue\n";
        let (cues, _) = parse_vtt(input).unwrap();
        let written = write_vtt(&cues, false);
        let (reparsed, _) = parse_vtt(&written).unwrap();
        assert_eq!(reparsed.len(), cues.len());
        for (a, b) in cues.iter().zip(reparsed.iter()) {
            assert_eq!(a.start, b.start);
            assert_eq!(a.end, b.end);
            assert_eq!(a.text(), b.text());
        }
    }

    #[test]
    fn round_trips_word_timestamps_when_enabled() {
        let input = "WEBVTT\n\n00:00:01.000 --> 00:00:04.000\nHello <00:00:02.500>world\n";
        let (cues, _) = parse_vtt(input).unwrap();
        let written = write_vtt(&cues, true);
        assert!(written.contains("<00:00:01.000>Hello"));
        assert!(written.contains("<00:00:02.500>world"));
        let (reparsed, summary) = parse_vtt(&written).unwrap();
        assert!(!summary.any_words_approximated);
        assert_eq!(reparsed[0].words, cues[0].words);
    }

    #[test]
    fn japanese_cue_round_trips_without_interpolated_spaces() {
        // Plain export re-joins per-cluster CJK words (42 §6.5) with no
        // fabricated spaces — the emitted cue text must read こんにちは.
        let input =
            "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\n\u{3053}\u{3093}\u{306B}\u{3061}\u{306F}\n"; // こんにちは
        let (cues, _) = parse_vtt(input).unwrap();
        assert_eq!(cues.len(), 1);
        assert!(cues[0].words.len() > 1, "CJK cue distributes per cluster");
        assert_eq!(cues[0].text(), "\u{3053}\u{3093}\u{306B}\u{3061}\u{306F}");
        let written = write_vtt(&cues, false);
        assert!(written.contains("\u{3053}\u{3093}\u{306B}\u{3061}\u{306F}"));
        assert!(
            !written.contains("\u{3053}\u{0020}\u{3093}"),
            "no interpolated space in exported cue text"
        );
    }
}
