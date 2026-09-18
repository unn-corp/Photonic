//! `spec-extract` — emit a JSON structural index of the workspace's public API
//! surface, consumed by `tools/check-spec-drift.py` (40-spec-verification.md §3).
//!
//! Usage:
//!   spec-extract [--root <workspace-root>] [--out <path>] [--stdin-fragment]
//!
//!   --root            Workspace root to scan. Default: this crate's
//!                     `CARGO_MANIFEST_DIR/../..` (i.e. the repo root).
//!   --out             Output file. Default: stdout.
//!   --stdin-fragment  Parse ONE Rust item from stdin and emit its single Item
//!                     record (used by the anchored-block drift checker, §3.1).
//!
//! Determinism: `items` are sorted by (file, line) and rendered with
//! `serde_json::to_string_pretty`, so repeated runs are byte-identical.

mod index;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use index::{Index, Item};

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
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

fn default_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn run() -> Result<(), String> {
    let mut root: Option<PathBuf> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut stdin_fragment = false;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--root" => {
                root = Some(PathBuf::from(args.next().ok_or("--root needs a value")?));
            }
            "--out" => {
                out_path = Some(PathBuf::from(args.next().ok_or("--out needs a value")?));
            }
            "--stdin-fragment" => stdin_fragment = true,
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    if stdin_fragment {
        use std::io::Read;
        let mut src = String::new();
        std::io::stdin()
            .read_to_string(&mut src)
            .map_err(|e| format!("stdin read error: {e}"))?;
        let item = index::index_fragment(&src)?;
        let json =
            serde_json::to_string_pretty(&item).map_err(|e| format!("serialize error: {e}"))?;
        emit(out_path.as_deref(), &json)?;
        return Ok(());
    }

    let root = root.unwrap_or_else(default_root);
    // Canonicalize so repo-relative paths in the index are stable regardless of
    // how `--root` was spelled (e.g. the `../..` default).
    let root = root
        .canonicalize()
        .map_err(|e| format!("cannot resolve root {}: {e}", root.display()))?;

    let mut files = Vec::new();
    let crates_dir = root.join("crates");
    let Ok(entries) = std::fs::read_dir(&crates_dir) else {
        return Err(format!("no crates/ under {}", root.display()));
    };
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        // `crates/<crate>/src/**/*.rs` and `crates/<crate>/tests/**/*.rs`.
        rs_files(&p.join("src"), &mut files);
        rs_files(&p.join("tests"), &mut files);
    }
    // Sort inputs for stable parse order (item sort is the real determinism
    // guard, but this keeps error reporting deterministic too).
    files.sort();

    let mut items: Vec<Item> = Vec::new();
    for f in &files {
        index::index_file(&root, f, &mut items)?;
    }

    let idx = Index::new(items);
    let json = serde_json::to_string_pretty(&idx).map_err(|e| format!("serialize error: {e}"))?;
    emit(out_path.as_deref(), &json)
}

fn emit(out_path: Option<&Path>, json: &str) -> Result<(), String> {
    match out_path {
        Some(p) => std::fs::write(p, format!("{json}\n"))
            .map_err(|e| format!("cannot write {}: {e}", p.display())),
        None => {
            println!("{json}");
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("spec-extract: {e}");
            ExitCode::FAILURE
        }
    }
}
