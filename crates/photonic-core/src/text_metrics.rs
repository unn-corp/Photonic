//! Shared Unicode text measurement for caption authoring (42 §6.3, §6.5).
//!
//! Every function here is **pure, integer-only and platform-independent**:
//! grapheme segmentation (UAX #29) plus the frozen East Asian Width table are
//! the only inputs, so the same string measures identically on every machine.
//! That determinism is load-bearing — cue boundaries computed from these
//! widths are *persisted* (§6.1), so authoring-time budgeting must never
//! consult a font, a locale, a float, or a measured advance width.
//!
//! This module lives in `photonic-core` (not `photonic-video`) on purpose: it
//! is the only crate both `photonic-video` (grouping / tokenization) and
//! `photonic-render` (cue re-joining) can share — `photonic-render` cannot
//! depend on `photonic-video`.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

/// Half-width cell width of `s` (42 §6.3). Iterates UAX #29 grapheme clusters
/// (extended = true); a cluster whose scalars are all zero-advance counts 0,
/// otherwise its width is East Asian Width `W`|`F` => 2, everything else => 1.
///
/// Integer arithmetic over frozen Unicode tables — identical on every machine.
///
/// Hard invariants (exercised by the tests below):
/// - **I1** monotone under concatenation:
///   `cell_width(a) + cell_width(b) >= cell_width(a).max(cell_width(b))`, and
///   for any `a`, `b` that do not fuse into one cluster across the seam the
///   two sides are additive.
/// - **I2** ASCII identity: `cell_width(s) == s.chars().count()` for pure ASCII.
/// - **I3** combining-mark invariance: inserting a zero-advance mark
///   (e.g. U+0300..=U+036F, U+0E31) leaves `cell_width` unchanged.
/// - **I4** CJK exactness: `n` ideographs measure `2 * n`.
/// - **I5** no float, no font, no locale, no OS call anywhere in this module.
#[must_use]
pub fn cell_width(s: &str) -> usize {
    graphemes(s).map(cluster_cells).sum()
}

/// Cell width of one already-segmented grapheme cluster. [`cell_width`] is the
/// sum of this over `graphemes(s)`; exposed so callers can budget
/// incrementally.
///
/// The cluster's width is the width of its first scalar whose
/// `UnicodeWidthChar::width()` is not `Some(0)` (i.e. the base of the cluster),
/// or 0 if every scalar is zero-advance. Deliberately *not*
/// `UnicodeWidthStr::width(cluster)`, which sums scalars and would count a
/// base + spacing-matra pair as 2.
#[must_use]
pub fn cluster_cells(cluster: &str) -> usize {
    for c in cluster.chars() {
        match UnicodeWidthChar::width(c) {
            // Zero-advance combining mark — skip and keep looking for the base.
            Some(0) => continue,
            // Base scalar found: 1 (narrow/half/ambiguous) or 2 (wide/full).
            Some(w) => return w,
            // Control / unassigned: treat as zero-advance and keep scanning.
            None => continue,
        }
    }
    0
}

/// UAX #29 extended grapheme clusters of `s`.
pub fn graphemes(s: &str) -> impl Iterator<Item = &str> + '_ {
    UnicodeSegmentation::graphemes(s, true)
}

/// UAX #29 word boundaries of `s` (42 §6.5 tokenization tier 2). A thin
/// re-export of `unicode-segmentation` so callers whose crate does not depend
/// on it directly — e.g. `photonic-video`'s timing distribution — can
/// tokenize through `photonic-core` without pulling the segmentation crate
/// into a second `[dependencies]` table.
pub fn word_bounds(s: &str) -> impl Iterator<Item = &str> + '_ {
    UnicodeSegmentation::split_word_bounds(s)
}

/// True for scripts written without inter-word spaces — *scriptio continua*
/// (42 §6.5): Han, Hiragana, Katakana, Thai, Lao, Khmer, Myanmar. Timing is
/// distributed across grapheme clusters for these rather than fabricating
/// words, and a single U+0020 is never inserted between two such clusters.
#[must_use]
pub fn is_scriptio_continua(c: char) -> bool {
    matches!(c,
        // Han (CJK Unified Ideographs + Extension A + compatibility)
        '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}'
        // Hiragana / Katakana
        | '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}'
        // Thai / Lao
        | '\u{0E00}'..='\u{0E7F}' | '\u{0E80}'..='\u{0EFF}'
        // Khmer
        | '\u{1780}'..='\u{17FF}'
        // Myanmar
        | '\u{1000}'..='\u{109F}'
    )
}

/// True if `cluster` begins with a scriptio-continua scalar.
#[must_use]
pub fn is_scriptio_continua_cluster(cluster: &str) -> bool {
    cluster.chars().next().is_some_and(is_scriptio_continua)
}

/// 42 §6.5 sentence-terminator set:
/// `. ! ?` | `。 ！ ？ ．` | `؟ ۔` | `। ॥` | `\u{037E} ·` | `။ ၏`.
///
/// Note the Greek group uses **U+037E GREEK QUESTION MARK**, *not* ASCII `;` —
/// the two must not be conflated, an ASCII semicolon must never end a cue.
#[must_use]
pub fn is_sentence_terminator(c: char) -> bool {
    matches!(
        c,
        '.' | '!' | '?'
        // CJK full-width
        | '\u{3002}' | '\u{FF01}' | '\u{FF1F}' | '\u{FF0E}'
        // Arabic
        | '\u{061F}' | '\u{06D4}'
        // Devanagari danda / double danda
        | '\u{0964}' | '\u{0965}'
        // Greek question mark (U+037E, NOT ASCII ';') / ano teleia
        | '\u{037E}' | '\u{00B7}'
        // Myanmar section / genitive
        | '\u{104B}' | '\u{104F}'
    )
}

/// Strips trailing whitespace and trailing closing punctuation
/// (`" ' ) ] } » ” ’ › 」 』 ）`) so that e.g. `He said "stop."` tests as
/// sentence-ending on its `.` rather than its closing quote.
#[must_use]
pub fn strip_trailing_closing_punctuation(s: &str) -> &str {
    s.trim_end_matches(|c: char| c.is_whitespace() || is_closing_punctuation(c))
}

/// Closing punctuation stripped by [`strip_trailing_closing_punctuation`].
fn is_closing_punctuation(c: char) -> bool {
    matches!(
        c,
        '"' | '\''
            | ')'
            | ']'
            | '}'
            | '\u{00BB}'
            | '\u{201D}'
            | '\u{2019}'
            | '\u{203A}'
            | '\u{300D}'
            | '\u{300F}'
            | '\u{FF09}'
    )
}

/// True iff a single U+0020 separator belongs between `prev` and `next` when a
/// cue's words are re-joined for rendering/export (task 4). Returns `false`
/// only when at least one side begins with a scriptio-continua scalar — so a
/// per-cluster CJK/Thai token stream re-joins seamlessly, while ASCII words
/// keep their spaces (byte-identical to the old `join(" ")`).
#[must_use]
pub fn needs_separator(prev: &str, next: &str) -> bool {
    !(is_scriptio_continua_cluster(prev) || is_scriptio_continua_cluster(next))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ---- Task 1: cell_width / cluster tables ----

    #[test]
    fn cell_width_table() {
        // (input, expected cells)
        let cases: &[(&str, usize)] = &[
            ("", 0),
            ("The quick brown fox.", 20), // ASCII sentence == char count
            ("\u{65E5}\u{672C}\u{8A9E}", 6), // 日本語 — 3 wide ideographs
            ("\u{D55C}\u{AD6D}\u{C5B4}", 6), // 한국어 — 3 wide Hangul syllables
            ("AB\u{65E5}", 4),            // mixed: A(1) B(1) 日(2)
            (
                "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
                2,
            ), // 👨‍👩‍👧‍👦 ZWJ == one wide cluster
        ];
        for (s, want) in cases {
            assert_eq!(cell_width(s), *want, "cell_width({s:?})");
        }
    }

    #[test]
    fn combining_marks_do_not_add_width() {
        // A zero-advance combining mark added to a base does not change width.
        assert_eq!(cell_width("e\u{0301}"), cell_width("e")); // é == e
        assert_eq!(cell_width("\u{0E01}\u{0E34}"), cell_width("\u{0E01}")); // กิ == ก

        // Devanagari नमस्ते: the conjunct स्ते is a *single* grapheme cluster,
        // so the base counts once and the virama/matra contribute nothing — the
        // whole string is 3 cells (न, म, स्ते), not 6.
        let namaste = "\u{0928}\u{092E}\u{0938}\u{094D}\u{0924}\u{0947}";
        assert_eq!(cell_width(namaste), 3);

        // Thai เกิด — leading vowel + ก + zero-advance sara-i (ิ) + ด: the
        // combining sara-i adds nothing, so 3 cells.
        let koed = "\u{0E40}\u{0E01}\u{0E34}\u{0E14}";
        assert_eq!(cell_width(koed), 3);
    }

    #[test]
    fn terminator_table_all_fourteen() {
        for c in [
            '.', '!', '?', '\u{3002}', '\u{FF01}', '\u{FF1F}', '\u{FF0E}', '\u{061F}', '\u{06D4}',
            '\u{0964}', '\u{0965}', '\u{037E}', '\u{00B7}', '\u{104B}', '\u{104F}',
        ] {
            assert!(
                is_sentence_terminator(c),
                "expected terminator: U+{:04X}",
                c as u32
            );
        }
        // ASCII ';' is NOT a terminator (it is U+003B, not U+037E).
        assert!(!is_sentence_terminator(';'));
        assert!(!is_sentence_terminator('a'));
        assert!(!is_sentence_terminator(','));
    }

    #[test]
    fn strip_trailing_closing_punctuation_cases() {
        assert_eq!(strip_trailing_closing_punctuation("stop.\""), "stop.");
        assert_eq!(strip_trailing_closing_punctuation("(done!)"), "(done!");
        assert_eq!(
            strip_trailing_closing_punctuation("\u{7D42}\u{308F}\u{308A}\u{3002}\u{300D}"),
            "\u{7D42}\u{308F}\u{308A}\u{3002}"
        ); // 終わり。」 -> 終わり。
        assert_eq!(strip_trailing_closing_punctuation("plain"), "plain");
        assert_eq!(strip_trailing_closing_punctuation("done.  "), "done.");
    }

    #[test]
    fn needs_separator_rules() {
        assert!(needs_separator("Hello", "world"));
        assert!(!needs_separator("\u{3053}", "\u{3093}")); // こ / ん
        assert!(!needs_separator("Photonic", "\u{306F}")); // ASCII / は
        assert!(!needs_separator("\u{901F}", "Photonic")); // 速 / ASCII
    }

    #[test]
    fn scriptio_continua_membership() {
        for c in [
            '\u{4E00}', '\u{3042}', '\u{30A2}', '\u{0E01}', '\u{0EC0}', '\u{1780}', '\u{1000}',
        ] {
            assert!(is_scriptio_continua(c), "U+{:04X}", c as u32);
        }
        for c in ['A', '1', '\u{0623}', '\u{05D0}', ' '] {
            assert!(!is_scriptio_continua(c), "U+{:04X}", c as u32);
        }
    }

    // ---- Task 6: properties + cross-platform determinism ----

    /// Zero-advance Mn combining diacriticals used to probe I3 / P3.
    fn combining_mark(idx: usize) -> char {
        // U+0300..=U+036F are all zero-width nonspacing marks.
        char::from_u32(0x0300 + (idx as u32 % 0x70)).unwrap()
    }

    proptest! {
        /// P1: monotone under concatenation.
        #[test]
        fn p1_monotone_under_concat(a in ".{0,64}", b in ".{0,64}") {
            let concat = format!("{a}{b}");
            prop_assert!(cell_width(&concat) >= cell_width(&a).max(cell_width(&b)));
        }

        /// P2: ASCII identity — cell width equals scalar count for printable ASCII.
        #[test]
        fn p2_ascii_identity(s in "[ -~]{0,200}") {
            prop_assert_eq!(cell_width(&s), s.chars().count());
        }

        /// P3: inserting a zero-advance combining mark after a non-empty base
        /// cluster does not change the cell width.
        #[test]
        fn p3_combining_mark_invariance(s in "[A-Za-z\u{3040}-\u{30FF}\u{4E00}-\u{9FFF}]{1,32}", m in 0usize..0x70) {
            let before = cell_width(&s);
            // Append the mark to the final cluster (it fuses with the last base).
            let with_mark = format!("{s}{}", combining_mark(m));
            prop_assert_eq!(cell_width(&with_mark), before);
        }

        /// P4: n CJK ideographs measure exactly 2n.
        #[test]
        fn p4_cjk_exactness(chars in prop::collection::vec(0x4E00u32..=0x9FFF, 0..=64)) {
            let s: String = chars.iter().map(|&c| char::from_u32(c).unwrap()).collect();
            prop_assert_eq!(cell_width(&s), 2 * chars.len());
        }
    }

    /// 42 §9 test 5 (the most valuable): a frozen fixture table. Because
    /// `cell_width` touches no font, locale or float, equality against these
    /// hard-coded integers on any CI runner IS the cross-platform proof — no
    /// per-OS golden files are needed.
    #[test]
    fn determinism_frozen_fixture() {
        let cases: &[(&str, &str, usize)] = &[
            ("ja", "\u{3053}\u{3093}\u{306B}\u{3061}\u{306F}", 10), // こんにちは 5×2
            ("zh-Hans", "\u{4F60}\u{597D}\u{4E16}\u{754C}", 8),     // 你好世界 4×2
            ("ko", "\u{C548}\u{B155}\u{D558}\u{C138}\u{C694}", 10), // 안녕하세요 5×2
            ("ar", "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}", 5),  // مرحبا 5×1
            ("he", "\u{05E9}\u{05DC}\u{05D5}\u{05DD}", 4),          // שלום 4×1
            ("hi", "\u{0928}\u{092E}\u{0938}\u{094D}\u{0924}\u{0947}", 3), // नमस्ते (स्ते is one cluster)
            ("th", "\u{0E40}\u{0E01}\u{0E34}\u{0E14}", 3),                 // เกิด
            (
                "emoji-zwj",
                "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
                2,
            ), // 👨‍👩‍👧‍👦
        ];
        for (tag, s, want) in cases {
            assert_eq!(cell_width(s), *want, "determinism fixture {tag}");
        }
    }
}
