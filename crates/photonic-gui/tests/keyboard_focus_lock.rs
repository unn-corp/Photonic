//! Lint: a custom widget that reads arrow/Tab navigation keys *while focused*
//! must lock those keys to itself with `set_focus_lock_filter` (41 §3 R-4/R-5).
//!
//! egui 0.29's focus navigation scans raw events before any panel code runs and,
//! unless the focused widget declared an `EventFilter`, converts the first
//! Arrow/Tab into a focus *move* — stealing focus off the widget on that same
//! keypress. So a `has_focus()`-gated arrow-nudge or Tab-cycle fires exactly once
//! and then dies, silently, the same failure mode 41 §1 describes. The structural
//! guard: any file that reads arrow/Tab keys under a `has_focus()` gate must also
//! call `set_focus_lock_filter`.
//!
//! No `egui_kittest` in the tree, so this asserts structurally, not behaviourally.

use std::fs;
use std::path::Path;

/// Navigation keys that egui's focus system will hijack without an EventFilter.
const NAV_KEYS: &[&str] = &[
    "Key::Tab",
    "Key::ArrowLeft",
    "Key::ArrowRight",
    "Key::ArrowUp",
    "Key::ArrowDown",
];

/// Files with the same latent shape but owned by other spec slices, not 41 §9
/// step 1 — exempted here so this gate stays green on the code it owns.
/// `audio_mixer.rs`'s `pan_knob` reads ArrowLeft/Right under `has_focus()` and is
/// tracked under specs 39/26, not here. Remove from this list when it is fixed.
const EXEMPT: &[&str] = &["audio_mixer.rs"];

fn is_comment(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//") || t.starts_with("/*") || t.starts_with('*')
}

/// Non-comment source of a file, joined back into one string.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !is_comment(l))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn focus_nav_widgets_lock_their_keys() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/panels/video");
    let mut files = Vec::new();
    for e in fs::read_dir(&root)
        .expect("read video panels dir")
        .flatten()
    {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "rs") {
            files.push(p);
        }
    }
    assert!(!files.is_empty(), "no sources under {}", root.display());

    let mut checked = 0usize;
    let mut violations = Vec::new();
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if EXEMPT.contains(&name.as_str()) {
            continue;
        }
        let code = code_only(&fs::read_to_string(path).expect("read source"));
        let focus_gated = code.contains("has_focus()");
        let reads_nav = NAV_KEYS.iter().any(|k| code.contains(k));
        if focus_gated && reads_nav {
            checked += 1;
            if !code.contains("set_focus_lock_filter") {
                violations.push(format!(
                    "{name}: reads arrow/Tab keys under `has_focus()` but never calls \
                     `set_focus_lock_filter` — egui will steal focus after the first \
                     press (41 §3 R-4/R-5)"
                ));
            }
        }
    }

    // Both brief-owned widgets (curve plot, node canvas) must be exercised, or the
    // scan root moved and the guard went vacuous.
    assert!(
        checked >= 2,
        "expected at least the curve editor and node canvas to be checked, got {checked}"
    );
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}
