//! DESIGN.md is the single source of truth for the theme palette; this test
//! keeps it honest against WCAG 2.1 (41 §5 R-10/R-11).
//!
//! It hand-parses the `colors:` frontmatter and a fenced ```contrast block (no
//! serde_yaml, no new deps — the crate must not gain one), evaluates every
//! declared `foreground | background | role` pair, and — the point of R-11 —
//! asserts every colour token appears as a foreground in at least one pair or in
//! a named `exempt:<reason>` row. A token added without a pair row fails the
//! build, so the palette can't quietly regress the way `#50506E` did.
//!
//! Thresholds (R-10): body-text 4.5:1, large-text / boundary / graphic /
//! focus-ring 3:1. `exempt:` rows are skipped but their reason must be non-empty.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

fn design_md() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../DESIGN.md")
        .canonicalize()
        .expect("resolve DESIGN.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The text between the first two `---` fence lines.
fn frontmatter(src: &str) -> &str {
    let after_open = src.strip_prefix("---").expect("opening frontmatter fence");
    let end = after_open.find("\n---").expect("closing frontmatter fence");
    &after_open[..end]
}

fn hex_to_rgb(h: &str) -> [u8; 3] {
    let h = h.trim_start_matches('#');
    [
        u8::from_str_radix(&h[0..2], 16).unwrap(),
        u8::from_str_radix(&h[2..4], 16).unwrap(),
        u8::from_str_radix(&h[4..6], 16).unwrap(),
    ]
}

/// Parse the `colors:` block of the frontmatter into `name -> rgb`.
fn colors(fm: &str) -> BTreeMap<String, [u8; 3]> {
    let mut out = BTreeMap::new();
    let mut in_colors = false;
    for line in fm.lines() {
        if line.starts_with("colors:") {
            in_colors = true;
            continue;
        }
        if !in_colors {
            continue;
        }
        // A non-blank line that is not indented ends the block (e.g. `typography:`).
        if !line.is_empty() && !line.starts_with(' ') {
            break;
        }
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue; // blank or full-line comment
        }
        let Some((key, rest)) = trimmed.split_once(':') else {
            continue;
        };
        // Value is a quoted `"#RRGGBB"`; ignore any trailing inline comment.
        let Some(hash) = rest.find('#') else { continue };
        let hex = &rest[hash..];
        if hex.len() < 7 || !hex[1..7].bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        out.insert(key.trim().to_string(), hex_to_rgb(&hex[..7]));
    }
    out
}

/// A `foreground | background | role` row from the ```contrast block.
struct Row {
    fg: String,
    bg: String,
    role: String,
}

fn contrast_rows(src: &str) -> Vec<Row> {
    let start = src.find("```contrast").expect("```contrast block");
    let body = &src[start + "```contrast".len()..];
    let end = body.find("```").expect("closing ``` for contrast block");
    let mut rows = Vec::new();
    for line in body[..end].lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, '|').map(|p| p.trim()).collect();
        assert_eq!(parts.len(), 3, "malformed contrast row: {line:?}");
        rows.push(Row {
            fg: parts[0].to_string(),
            bg: parts[1].to_string(),
            role: parts[2].to_string(),
        });
    }
    rows
}

fn rel_luminance(c: [u8; 3]) -> f64 {
    let chan = |v: u8| {
        let s = v as f64 / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * chan(c[0]) + 0.7152 * chan(c[1]) + 0.0722 * chan(c[2])
}

fn ratio(fg: [u8; 3], bg: [u8; 3]) -> f64 {
    let (a, b) = (rel_luminance(fg), rel_luminance(bg));
    let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

fn threshold(role: &str) -> Option<f64> {
    match role {
        "body-text" => Some(4.5),
        "large-text" | "boundary" | "graphic" | "focus-ring" => Some(3.0),
        _ => None, // exempt:* — handled separately
    }
}

/// Tokens left uncovered by the pair/exempt table — the R-11 gate.
fn uncovered(colors: &BTreeMap<String, [u8; 3]>, rows: &[Row]) -> Vec<String> {
    let mut covered = BTreeSet::new();
    for r in rows {
        covered.insert(r.fg.clone());
    }
    colors
        .keys()
        .filter(|k| !covered.contains(*k))
        .cloned()
        .collect()
}

#[test]
fn every_declared_pair_meets_wcag_aa() {
    let src = design_md();
    let fm = frontmatter(&src);
    let cols = colors(fm);
    let rows = contrast_rows(&src);
    assert!(
        !cols.is_empty(),
        "no colours parsed from DESIGN.md frontmatter"
    );
    assert!(!rows.is_empty(), "no contrast rows parsed from DESIGN.md");

    let mut failures = Vec::new();
    for r in &rows {
        let fg = cols
            .get(&r.fg)
            .unwrap_or_else(|| panic!("contrast row names unknown token {:?}", r.fg));
        // Exempt rows carry no background requirement but must justify themselves.
        if let Some(reason) = r.role.strip_prefix("exempt:") {
            assert!(
                !reason.trim().is_empty(),
                "exempt token {:?} has an empty reason",
                r.fg
            );
            continue;
        }
        let bg = cols
            .get(&r.bg)
            .unwrap_or_else(|| panic!("contrast row names unknown background {:?}", r.bg));
        let want = threshold(&r.role)
            .unwrap_or_else(|| panic!("unknown role {:?} in row for {:?}", r.role, r.fg));
        let got = ratio(*fg, *bg);
        if got + 0.005 < want {
            failures.push(format!(
                "{} on {} = {got:.2}:1 (< {want} for {})",
                r.fg, r.bg, r.role
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "DESIGN.md contrast pairs below WCAG AA (41 §5 R-10):\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn every_token_is_covered() {
    let src = design_md();
    let cols = colors(frontmatter(&src));
    let rows = contrast_rows(&src);
    let missing = uncovered(&cols, &rows);
    assert!(
        missing.is_empty(),
        "colour tokens with no contrast pair or exempt row (41 §5 R-11): {missing:?}"
    );
}

#[test]
fn both_themes_are_represented() {
    // Guard against a theme silently having zero rows (which would let a whole
    // palette pass vacuously). Both the dark (unprefixed) and light (`light-*`)
    // palettes must contribute at least one evaluated foreground.
    let src = design_md();
    let rows = contrast_rows(&src);
    let non_exempt = |r: &&Row| !r.role.starts_with("exempt:");
    let has_light = rows
        .iter()
        .filter(non_exempt)
        .any(|r| r.fg.starts_with("light-"));
    let has_dark = rows
        .iter()
        .filter(non_exempt)
        .any(|r| !r.fg.starts_with("light-"));
    assert!(has_dark, "no dark-theme contrast rows");
    assert!(has_light, "no light-theme contrast rows");
}

#[test]
fn known_ratios() {
    // Pin hand-computed values so a bug in `rel_luminance` cannot make the whole
    // suite vacuously green.
    let cases = [
        ("#8A8AA8", "#13131F", 5.51),
        ("#E8E8F2", "#0C0C15", 15.99),
        ("#6E56CF", "#0C0C15", 3.61),
    ];
    for (fg, bg, want) in cases {
        let got = ratio(hex_to_rgb(fg), hex_to_rgb(bg));
        assert!(
            (got - want).abs() <= 0.02,
            "{fg} on {bg}: got {got:.2}, expected {want}"
        );
    }
}

#[test]
fn uncovered_token_fails() {
    // The R-11 coverage check must actually reject an unpaired token — a gate with
    // no self-test is how the first hole got in.
    let mut cols = BTreeMap::new();
    cols.insert("on-surface".to_string(), [232, 232, 242]);
    cols.insert("ghost".to_string(), [1, 2, 3]); // declared, never paired
    let rows = vec![Row {
        fg: "on-surface".to_string(),
        bg: "surface".to_string(),
        role: "body-text".to_string(),
    }];
    let missing = uncovered(&cols, &rows);
    assert_eq!(missing, vec!["ghost".to_string()]);
}
