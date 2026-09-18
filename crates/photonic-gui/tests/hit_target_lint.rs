//! Lint: no local override may drop the interactive hit-target height below the
//! 24px WCAG 2.2 SC 2.5.8 floor (41 §5 R-9).
//!
//! `theme::apply_spacing` sets the global floor at startup, but a single
//! `ui.spacing_mut().interact_size = vec2(_, h)` with `h < 24.0` anywhere would
//! silently undercut it for that widget. This walks the whole GUI source and
//! fails on any such assignment.

use std::fs;
use std::path::{Path, PathBuf};

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
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

/// Extract the y component of an `interact_size = ...vec2(x, y)` assignment on a
/// line, if present.
fn interact_size_height(line: &str) -> Option<f32> {
    if !line.contains("interact_size") {
        return None;
    }
    let after = line.split_once("interact_size")?.1;
    // Only assignments (`= vec2(...)`), not reads.
    let after = after.trim_start();
    if !after.starts_with('=') {
        return None;
    }
    let open = after.find("vec2(")? + "vec2(".len();
    let close = after[open..].find(')')? + open;
    let args = &after[open..close];
    let (_, y) = args.split_once(',')?;
    y.trim().trim_end_matches("f32").trim().parse::<f32>().ok()
}

#[test]
fn no_interact_size_below_wcag_floor() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&root, &mut files);
    assert!(!files.is_empty(), "no sources under {}", root.display());

    let mut seen = 0usize;
    let mut violations = Vec::new();
    for path in &files {
        let src = fs::read_to_string(path).expect("read source");
        for (i, line) in src.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") || t.starts_with('*') {
                continue;
            }
            if let Some(h) = interact_size_height(line) {
                seen += 1;
                if h < 24.0 {
                    violations.push(format!(
                        "{}:{}: interact_size height {h} < 24.0 (WCAG SC 2.5.8)",
                        path.file_name().unwrap().to_string_lossy(),
                        i + 1
                    ));
                }
            }
        }
    }

    // The scanner must actually be matching something, or the guard is vacuous.
    assert!(
        seen > 0,
        "no `interact_size = vec2(..)` assignments found to lint"
    );
    assert!(
        violations.is_empty(),
        "interactive hit-target below the 24px floor (41 §5 R-9):\n  {}",
        violations.join("\n  ")
    );
}
