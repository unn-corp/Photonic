//! SRT import/export (06 §7.1/§7.2).
//!
//! SRT never carries word-level timing: import always distributes words
//! proportionally by character count across the cue span (§7.1's "no native
//! word-level timing" row). Export is plain text — words joined with spaces
//! (line-wrap is a render-time concern, §3.5, never stored).

use photonic_core::timeline::CaptionCue;

use super::{format_ms_timestamp, parse_ms_timestamp, ImportSummary, InterchangeError};
use crate::captions::proportional::distribute_caption_words;

pub fn parse_srt(input: &str) -> Result<(Vec<CaptionCue>, ImportSummary), InterchangeError> {
    let normalized = input.replace("\r\n", "\n");
    let mut cues = Vec::new();
    let mut summary = ImportSummary::default();

    for block in normalized.split("\n\n") {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        let mut lines = block.lines();
        let first = lines.next().unwrap_or("");

        let timestamp_line = if first.trim().parse::<u64>().is_ok() {
            // Numeric cue index — the real timestamp line follows.
            lines.next().ok_or_else(|| {
                InterchangeError::Parse(format!("cue with no timestamp line: {first:?}"))
            })?
        } else {
            // Lenient: tolerate a missing/non-numeric index line.
            first
        };

        let (start_str, rest) = timestamp_line.split_once("-->").ok_or_else(|| {
            InterchangeError::Parse(format!("malformed timestamp line: {timestamp_line:?}"))
        })?;
        let end_token = rest.split_whitespace().next().unwrap_or("");
        let start = parse_ms_timestamp(start_str.trim(), ',').ok_or_else(|| {
            InterchangeError::Parse(format!("bad start timestamp: {start_str:?}"))
        })?;
        let end = parse_ms_timestamp(end_token, ',')
            .ok_or_else(|| InterchangeError::Parse(format!("bad end timestamp: {end_token:?}")))?;

        let text = lines.collect::<Vec<_>>().join(" ");
        let words = distribute_caption_words(&text, start, end);
        if words.is_empty() {
            continue;
        }
        summary.any_words_approximated = true;
        cues.push(CaptionCue::new(start, end, words));
    }

    summary.cues_imported = cues.len();
    Ok((cues, summary))
}

pub fn write_srt(cues: &[CaptionCue]) -> String {
    let mut out = String::new();
    for (i, cue) in cues.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&(i + 1).to_string());
        out.push('\n');
        out.push_str(&format_ms_timestamp(cue.start, ','));
        out.push_str(" --> ");
        out.push_str(&format_ms_timestamp(cue.end, ','));
        out.push('\n');
        out.push_str(&cue.text());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::Tick;

    const SAMPLE: &str = "1\n00:00:01,000 --> 00:00:04,000\nHello world\n\n2\n00:00:05,500 --> 00:00:07,000\nSecond cue\nwith two lines\n";

    #[test]
    fn parses_two_cues() {
        let (cues, summary) = parse_srt(SAMPLE).unwrap();
        assert_eq!(cues.len(), 2);
        assert_eq!(summary.cues_imported, 2);
        assert!(summary.any_words_approximated);
        assert_eq!(cues[0].start, Tick::from_seconds(1));
        assert_eq!(cues[0].end, Tick::from_seconds(4));
        assert_eq!(cues[0].text(), "Hello world");
        assert_eq!(cues[1].text(), "Second cue with two lines");
    }

    #[test]
    fn words_are_chronological_within_each_cue() {
        let (cues, _) = parse_srt(SAMPLE).unwrap();
        for cue in &cues {
            for pair in cue.words.windows(2) {
                assert!(pair[0].end <= pair[1].start);
            }
        }
    }

    #[test]
    fn round_trips_through_write_and_parse() {
        let (cues, _) = parse_srt(SAMPLE).unwrap();
        let written = write_srt(&cues);
        let (reparsed, _) = parse_srt(&written).unwrap();
        assert_eq!(reparsed.len(), cues.len());
        for (a, b) in cues.iter().zip(reparsed.iter()) {
            assert_eq!(a.start, b.start);
            assert_eq!(a.end, b.end);
            assert_eq!(a.text(), b.text());
        }
    }

    #[test]
    fn tolerates_missing_index_line() {
        let input = "00:00:01,000 --> 00:00:02,000\nNo index here\n";
        let (cues, _) = parse_srt(input).unwrap();
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text(), "No index here");
    }

    #[test]
    fn rejects_malformed_timestamp_line() {
        let input = "1\nnot a timestamp\nsome text\n";
        assert!(parse_srt(input).is_err());
    }

    #[test]
    fn empty_input_yields_no_cues() {
        let (cues, summary) = parse_srt("").unwrap();
        assert!(cues.is_empty());
        assert_eq!(summary.cues_imported, 0);
    }

    #[test]
    fn japanese_cue_round_trips_without_interpolated_spaces() {
        // A CJK cue is distributed one CaptionWord per grapheme cluster
        // (42 §6.5), but re-joining it for export must not fabricate spaces
        // between clusters — a space would be baked into the exported subtitle.
        let input = "1\n00:00:01,000 --> 00:00:03,000\n\u{3053}\u{3093}\u{306B}\u{3061}\u{306F}\n"; // こんにちは
        let (cues, _) = parse_srt(input).unwrap();
        assert_eq!(cues.len(), 1);
        assert!(cues[0].words.len() > 1, "CJK cue distributes per cluster");
        assert_eq!(cues[0].text(), "\u{3053}\u{3093}\u{306B}\u{3061}\u{306F}");
        let written = write_srt(&cues);
        assert!(written.contains("\u{3053}\u{3093}\u{306B}\u{3061}\u{306F}"));
        // No space anywhere between the two leading clusters.
        assert!(
            !written.contains("\u{3053}\u{0020}\u{3093}"),
            "no interpolated space in exported cue text"
        );
    }
}
