//! Proportional-by-character-count word timing distribution (06 §2.2, §7.1).
//!
//! Shared by two callers that both need to fabricate word-level timing from a
//! cue/segment that only has line-level timing: the hosted provider's
//! "cue-only source" degraded path (§2.2) and SRT/VTT/ASS import when no
//! native word timing is present (§7.1). One implementation, two thin
//! wrappers so each caller gets back its own word type without duplicating
//! the distribution math.

use photonic_core::timeline::{CaptionWord, Tick};

use super::provider::TranscribedWord;

/// Tokenize `text` for timing distribution (42 §6.5). UAX #29 word boundaries,
/// except that a run of scriptio-continua scalars (Han, Kana, Thai, …) is
/// emitted as one token *per grapheme cluster* rather than fabricating a word —
/// so karaoke timing sweeps per cluster. Whitespace bounds are dropped.
///
/// Punctuation that UAX #29 splits off a word (e.g. the `.` in `transcript.`)
/// is **re-attached to the preceding word token**, never dropped: these tokens
/// are also the caption's rendered/exported text (via `distribute_caption_words`),
/// so silently discarding punctuation would strip every full stop from a cue and
/// defeat the sentence-break detection in `grouping`. Leading punctuation and
/// emoji/symbol clusters with no preceding word are kept as their own token.
/// Returns borrowed slices of `text`.
fn tokenize_for_timing(text: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    // Byte range into `text` of the last emitted word token, so a following
    // punctuation bound can extend it (the bounds partition `text` in order,
    // so accumulating `len()` yields exact, char-aligned offsets).
    let mut last_word: Option<(usize, usize)> = None;
    let mut pos = 0usize;
    for bound in photonic_core::text_metrics::word_bounds(text) {
        let start = pos;
        let end = pos + bound.len();
        pos = end;

        if photonic_core::text_metrics::is_scriptio_continua_cluster(bound) {
            // A CJK/Thai/… run: one token per grapheme cluster, and it ends any
            // Latin-word attachment run.
            last_word = None;
            out.extend(photonic_core::text_metrics::graphemes(bound));
        } else if bound.chars().any(char::is_alphanumeric) {
            out.push(&text[start..end]);
            last_word = Some((start, end));
        } else if bound.chars().any(|c| !c.is_whitespace()) {
            // Punctuation / symbol bound: fold it into the preceding word so the
            // text is preserved, else keep it standalone (leading punctuation,
            // a bare emoji).
            match last_word {
                Some((ws, _)) => {
                    let merged = &text[ws..end];
                    *out.last_mut().expect("last_word implies a token") = merged;
                    last_word = Some((ws, end));
                }
                None => out.push(&text[start..end]),
            }
        } else {
            // Pure whitespace: dropped, and it breaks the attachment run.
            last_word = None;
        }
    }
    out
}

/// Split `text` into timing tokens (42 §6.5) and distribute `[start, end)`
/// across them proportionally by **half-width cell width** (42 §6.3).
/// Deterministic; rounds toward the start of each token. Returns `(text,
/// start, end)` triples in token order.
fn distribute_spans(text: &str, start: Tick, end: Tick) -> Vec<(String, Tick, Tick)> {
    let tokens: Vec<&str> = tokenize_for_timing(text);
    if tokens.is_empty() {
        return Vec::new();
    }
    let total_cells: usize = tokens
        .iter()
        .map(|t| photonic_core::text_metrics::cell_width(t))
        .sum();
    let span = (end.0 - start.0).max(0) as f64;

    let mut cursor = start.0 as f64;
    let mut out = Vec::with_capacity(tokens.len());
    for (i, tok) in tokens.iter().enumerate() {
        let is_last = i + 1 == tokens.len();
        let frac = if total_cells == 0 {
            1.0 / tokens.len() as f64
        } else {
            photonic_core::text_metrics::cell_width(tok) as f64 / total_cells as f64
        };
        let dur = span * frac;
        let w_start = Tick(cursor.round() as i64);
        let w_end = if is_last {
            end
        } else {
            Tick((cursor + dur).round() as i64)
        };
        // Guard degenerate zero/negative spans (e.g. start == end) so every
        // word still gets a valid, non-decreasing [start, end].
        let w_end = if w_end.0 < w_start.0 { w_start } else { w_end };
        out.push((tok.to_string(), w_start, w_end));
        cursor = w_end.0 as f64;
    }
    out
}

/// [`TranscribedWord`] flavor — used by the hosted adapter's degraded
/// cue-only path (06 §2.2). `confidence` is always `None` (the source never
/// had per-word confidence to begin with).
pub fn distribute_words_proportionally(text: &str, start: Tick, end: Tick) -> Vec<TranscribedWord> {
    distribute_spans(text, start, end)
        .into_iter()
        .map(|(text, start, end)| TranscribedWord {
            text,
            start,
            end,
            confidence: None,
        })
        .collect()
}

/// [`CaptionWord`] flavor — used by SRT/VTT/ASS import when the source has
/// no native word-level timing (06 §7.1).
pub fn distribute_caption_words(text: &str, start: Tick, end: Tick) -> Vec<CaptionWord> {
    distribute_spans(text, start, end)
        .into_iter()
        .map(|(text, start, end)| CaptionWord::new(text, start, end))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_yields_no_words() {
        assert!(distribute_caption_words("", Tick(0), Tick(1000)).is_empty());
        assert!(distribute_caption_words("   ", Tick(0), Tick(1000)).is_empty());
    }

    #[test]
    fn single_word_spans_the_whole_range() {
        let words = distribute_caption_words("hello", Tick(100), Tick(500));
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].start, Tick(100));
        assert_eq!(words[0].end, Tick(500));
    }

    #[test]
    fn words_are_chronological_and_cover_the_full_span_by_char_count() {
        // "hi" (2 chars) + "world" (5 chars) => 7 total; span 700 ticks.
        let words = distribute_caption_words("hi world", Tick(0), Tick(700));
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "hi");
        assert_eq!(words[1].text, "world");
        assert_eq!(words[0].start, Tick(0));
        // "hi" gets 2/7 of the span ~= 200 ticks.
        assert_eq!(words[0].end, Tick(200));
        assert_eq!(words[1].start, Tick(200));
        // Last word always closes exactly at `end`.
        assert_eq!(words[1].end, Tick(700));
    }

    #[test]
    fn words_are_non_overlapping_and_ordered() {
        let words = distribute_caption_words(
            "the quick brown fox jumps over the lazy dog",
            Tick(0),
            Tick(9000),
        );
        for pair in words.windows(2) {
            assert!(pair[0].end <= pair[1].start);
            assert!(pair[0].start <= pair[0].end);
        }
        assert_eq!(words.last().unwrap().end, Tick(9000));
    }

    #[test]
    fn zero_duration_span_still_produces_valid_non_decreasing_words() {
        let words = distribute_caption_words("a b c", Tick(500), Tick(500));
        for w in &words {
            assert!(w.start <= w.end);
        }
        assert_eq!(words.last().unwrap().end, Tick(500));
    }

    #[test]
    fn transcribed_word_flavor_has_no_confidence() {
        let words = distribute_words_proportionally("a b", Tick(0), Tick(100));
        assert!(words.iter().all(|w| w.confidence.is_none()));
    }

    // ---- Task 3: script-aware tokenization + cell-weighted timing (42 §6.5) ----

    #[test]
    fn scriptio_continua_distributes_one_word_per_cluster() {
        // こんにちは — 5 hiragana clusters, one CaptionWord each, chronological,
        // last closes exactly at `end`.
        let words = distribute_caption_words(
            "\u{3053}\u{3093}\u{306B}\u{3061}\u{306F}",
            Tick(0),
            Tick(500),
        );
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["\u{3053}", "\u{3093}", "\u{306B}", "\u{3061}", "\u{306F}"]
        );
        assert_eq!(words[0].start, Tick(0));
        for pair in words.windows(2) {
            assert!(pair[0].end <= pair[1].start);
        }
        assert_eq!(words.last().unwrap().end, Tick(500));
    }

    #[test]
    fn mixed_latin_and_cjk_weights_by_cell_width() {
        // "Photonic は速い": tokens Photonic(8 cells), は(2), 速(2), い(2) = 14.
        // Over a 1400-tick span each cell is 100 ticks: Photonic ~800, 速 ~200.
        let words =
            distribute_caption_words("Photonic \u{306F}\u{901F}\u{3044}", Tick(0), Tick(1400));
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, vec!["Photonic", "\u{306F}", "\u{901F}", "\u{3044}"]);
        // Photonic (8/14 of 1400) = 800 ticks.
        assert_eq!(words[0].end.0 - words[0].start.0, 800);
        // 速 (2/14 of 1400) = 200 ticks.
        assert_eq!(words[2].end.0 - words[2].start.0, 200);
        assert_eq!(words.last().unwrap().end, Tick(1400));
    }

    #[test]
    fn thai_tone_marks_stay_attached_to_their_base() {
        // เกิดใหม่ — the sara-i (ิ) and mai-ek (่) attach to their bases, never
        // becoming standalone tokens.
        let words = distribute_caption_words(
            "\u{0E40}\u{0E01}\u{0E34}\u{0E14}\u{0E43}\u{0E2B}\u{0E21}\u{0E48}",
            Tick(0),
            Tick(600),
        );
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "\u{0E40}",         // เ
                "\u{0E01}\u{0E34}", // กิ (base + sara-i)
                "\u{0E14}",         // ด
                "\u{0E43}",         // ใ
                "\u{0E2B}",         // ห
                "\u{0E21}\u{0E48}", // ม่ (base + mai-ek)
            ]
        );
        // No token is a lone combining mark.
        assert!(words
            .iter()
            .all(|w| w.text != "\u{0E34}" && w.text != "\u{0E48}"));
        assert_eq!(words.last().unwrap().end, Tick(600));
    }

    #[test]
    fn trailing_punctuation_stays_attached_to_its_word() {
        // UAX #29 splits "transcript." into "transcript" + "."; the period must
        // ride with its word, not be dropped — these tokens ARE the caption
        // text, so a dropped stop would strip punctuation from every cue and
        // break sentence-break detection. (Regression for the tokenizer.)
        let words =
            distribute_caption_words("Hello there. A test transcript.", Tick(0), Tick(1000));
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, vec!["Hello", "there.", "A", "test", "transcript."]);
        assert_eq!(words.last().unwrap().end, Tick(1000));
    }

    #[test]
    fn emoji_zwj_sequence_is_one_token() {
        let words = distribute_caption_words(
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
            Tick(0),
            Tick(400),
        );
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].start, Tick(0));
        assert_eq!(words[0].end, Tick(400));
    }
}
