//! Lint: keyboard handling must not be gated on pointer position or on global
//! focus-emptiness.
//!
//! Three real defects motivated this, all sharing a signature — a keyboard path
//! written correctly and then disabled by its own guard:
//!
//! * the node canvas gated `Tab`/arrows/`Delete` on `rect_contains_pointer`, so
//!   every shortcut required the mouse to hover the canvas;
//! * the curve editor gated arrow-nudge on `!keyboard_captured(ui)`, which means
//!   *nothing anywhere* has focus — so the nudge died the moment the plot itself
//!   was focused;
//! * the colour-page `D` bypass reused that same `keyboard_captured` helper, and
//!   the first version of this lint only matched the inlined `focused().is_none()`
//!   / `focused().is_some()` — never the helper — so the third violation stayed
//!   invisible and the lint read green over a codebase that still had the defect.
//!
//! None is visible to a structural spec-drift checker: the symbols exist, the
//! signatures match, nothing is stale. Only a pattern lint finds this class.
//! See `docs/specs/video-editor/41-accessibility.md` §3 R-5 and §8 item 3.

use std::fs;
use std::path::Path;

/// Substrings that must not appear inside a block that also handles key input.
const BANNED: &[(&str, &str)] = &[
    (
        "rect_contains_pointer",
        "gates keyboard handling on pointer position — use `Response::has_focus()`",
    ),
    (
        "focused().is_none()",
        "gates keyboard handling on global focus-emptiness — use `Response::has_focus()`",
    ),
    (
        "focused().is_some()",
        "gates keyboard handling on global focus-emptiness — use `Response::has_focus()`",
    ),
    (
        "keyboard_captured(",
        "gates keyboard handling on global focus-emptiness via a helper — use `Response::has_focus()`",
    ),
];

/// Key-input calls that mark a region as keyboard handling.
const KEY_MARKERS: &[&str] = &["key_pressed(", "key_down(", "consume_key(", "key_released("];

/// A banned call within `WINDOW` lines *above* a key-input call — i.e. plausibly
/// guarding it. The helper-call/key-read distance is larger than the inlined
/// case, so this is wider than the two-line gap of the original defect.
const WINDOW: usize = 16;

fn is_comment(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//") || t.starts_with("/*") || t.starts_with('*')
}

fn rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Scan one source string. Returns `(line_1_indexed, banned_pattern, why)` for
/// every banned call that guards a key-input call within `WINDOW` lines.
fn scan_source(src: &str) -> Vec<(usize, &'static str, &'static str)> {
    let lines: Vec<&str> = src.lines().collect();
    let mut hits = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        // Scan code, not prose. A comment *describing* a banned pattern —
        // including the ones documenting why these gates were replaced — is not a
        // violation, and a lint that fires on prose gets disabled.
        if is_comment(line) {
            continue;
        }
        let Some((pat, why)) = BANNED.iter().find(|(p, _)| line.contains(*p)) else {
            continue;
        };
        let end = (i + WINDOW).min(lines.len());
        let guards_keys = lines[i..end]
            .iter()
            .filter(|l| !is_comment(l))
            .any(|l| KEY_MARKERS.iter().any(|m| l.contains(m)));
        if guards_keys {
            hits.push((i + 1, *pat, *why));
        }
    }
    hits
}

#[test]
fn keyboard_handling_is_not_gated_on_pointer_or_global_focus() {
    // Widened from `src/panels/video` to all of `src`: the only other
    // `rect_contains_pointer` in the crate (`app/timeline/mod.rs`, scroll/zoom)
    // gates scroll, not key input, so KEY_MARKERS ignores it — widening costs
    // nothing today and covers the rest of the GUI.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&root, &mut files);
    assert!(
        !files.is_empty(),
        "no sources found under {}",
        root.display()
    );

    let mut violations = Vec::new();
    for path in &files {
        let src = fs::read_to_string(path).expect("read source");
        for (line, pat, why) in scan_source(&src) {
            violations.push(format!(
                "{}:{}: `{}` {}",
                path.file_name().unwrap().to_string_lossy(),
                line,
                pat,
                why
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "keyboard handling gated on pointer position or global focus \
         (41 §3 R-5):\n  {}",
        violations.join("\n  ")
    );
}

#[test]
fn lint_catches_helper_indirection() {
    // The helper-indirection hole itself, as a fixture: a lint with no self-test
    // is exactly how the first hole got there.
    let fixture = "\
        let captured = keyboard_captured(ui);\n\
        let d_pressed = !captured && ui.input(|i| i.key_pressed(egui::Key::D));\n";
    let hits = scan_source(fixture);
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one violation, got {hits:?}"
    );
    assert_eq!(hits[0].1, "keyboard_captured(");
}

#[test]
fn lint_catches_inlined_global_focus() {
    let fixture = "\
        if ui.memory(|m| m.focused().is_none()) {\n\
            let d = ui.input(|i| i.key_pressed(egui::Key::D));\n\
        }\n";
    assert_eq!(scan_source(fixture).len(), 1);
}

#[test]
fn lint_ignores_pointer_gate_far_from_keys() {
    // `rect_contains_pointer` guarding *scroll* (no key marker nearby) is fine.
    let fixture = "\
        if !ui.rect_contains_pointer(full) { return; }\n\
        let scroll = ui.input(|i| i.raw_scroll_delta.y);\n";
    assert!(scan_source(fixture).is_empty());
}
