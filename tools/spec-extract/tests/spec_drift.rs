//! Historical-drift regression suite for `tools/check-spec-drift.py`
//! (40-spec-verification.md §3.2, §6). Proves ACC-40-06-04, -05, -06.
//!
//! The checker is stdlib-only Python (tools/README.md forbids adding Python
//! test infrastructure), so this Rust integration test shells out to `python3`
//! and rides the existing `cargo test --workspace --locked --all-features`
//! step. `python3` is already assumed present by CI's lint job.
//!
//! Two kinds of case:
//!   * `tests/cases/sd-*/` — frozen miniature reproductions of drifts that
//!     ACTUALLY happened, asserted byte-for-byte against `expected.txt`.
//!   * live-workspace forms (`dep-*`, `feature-absent`, `if X then Y`) driven
//!     against the real spec-extract index, because their ground truth is the
//!     repo itself.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("resolve repo root")
}

fn checker() -> PathBuf {
    repo_root().join("tools").join("check-spec-drift.py")
}

fn gen_acceptance() -> PathBuf {
    repo_root().join("tools").join("gen-acceptance-index.py")
}

/// Build the real workspace index once and reuse it across cases.
fn real_index() -> &'static Path {
    static IDX: OnceLock<PathBuf> = OnceLock::new();
    IDX.get_or_init(|| {
        let out = std::env::temp_dir().join(format!("spec-index-{}.json", std::process::id()));
        let status = Command::new(env!("CARGO_BIN_EXE_spec-extract"))
            .arg("--root")
            .arg(repo_root())
            .arg("--out")
            .arg(&out)
            .status()
            .expect("run spec-extract");
        assert!(status.success(), "spec-extract failed to build the index");
        out
    })
    .as_path()
}

/// Run the checker, returning (exit code, stdout).
fn run(index: &Path, docs: &Path, root: &Path) -> (i32, String) {
    let out = Command::new("python3")
        .arg(checker())
        .arg("--index")
        .arg(index)
        .arg("--docs")
        .arg(docs)
        .arg("--root")
        .arg(root)
        .output()
        .expect("run check-spec-drift.py");
    let code = out.status.code().unwrap_or(-1);
    (code, String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Write a throwaway docs dir carrying one `spec-assert` per line.
fn temp_docs(asserts: &[&str]) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("drift-docs-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir temp docs");
    let body: String = asserts
        .iter()
        .map(|a| format!("<!-- spec-assert: {a} -->\n"))
        .collect();
    std::fs::write(dir.join("t.md"), body).expect("write temp doc");
    dir
}

fn case_dirs() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cases");
    let mut out = Vec::new();
    for e in std::fs::read_dir(&root).expect("read cases dir").flatten() {
        let p = e.path();
        if p.is_dir()
            && p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("sd-"))
        {
            out.push(p);
        }
    }
    out.sort();
    out
}

#[test]
fn each_historical_drift_is_caught() {
    let cases = case_dirs();
    assert!(!cases.is_empty(), "no sd-* cases found");
    for case in cases {
        let index = case.join("index.json");
        let expected =
            std::fs::read_to_string(case.join("expected.txt")).expect("read expected.txt");
        let (code, stdout) = run(&index, &case, &case);
        assert_eq!(code, 1, "{}: expected drift exit 1", case.display());
        assert_eq!(
            stdout.replace("\r\n", "\n").replace('\\', "/"),
            expected.replace("\r\n", "\n").replace('\\', "/"),
            "{}: checker output must match expected.txt byte-for-byte",
            case.display()
        );
    }
}

#[test]
fn anchored_blocks_are_compared_structurally() {
    // §3.1: a ```rust block whose first line is `// spec-source: <ref>` is
    // parsed via `spec-extract --stdin-fragment` and structurally compared to
    // the index item — field/variant names + order, with `...` suppressing the
    // completeness check but not existence or relative order. Proves
    // ACC-40-06-02 and ACC-40-06-03.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cases/anchored");
    let bin = env!("CARGO_BIN_EXE_spec-extract");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("read anchored dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    assert!(
        dirs.len() >= 6,
        "expected >= 6 anchored cases, found {}",
        dirs.len()
    );

    for case in dirs {
        let name = case.file_name().unwrap().to_string_lossy().to_string();
        let expected = std::fs::read_to_string(case.join("expected.txt")).expect("expected.txt");
        let out = Command::new("python3")
            .arg(checker())
            .arg("--index")
            .arg(case.join("index.json"))
            .arg("--docs")
            .arg(&case)
            .arg("--root")
            .arg(&case)
            .arg("--spec-extract")
            .arg(bin)
            .output()
            .expect("run check-spec-drift.py");
        let code = out.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&out.stdout)
            .replace("\r\n", "\n")
            .replace('\\', "/");
        let stderr = String::from_utf8_lossy(&out.stderr);
        if expected.trim().is_empty() {
            assert_eq!(
                code, 0,
                "{name}: expected a passing block (exit 0)\n{stderr}"
            );
            assert!(
                stdout.trim().is_empty(),
                "{name}: expected no output, got:\n{stdout}"
            );
        } else {
            assert_eq!(
                code, 1,
                "{name}: expected structural drift (exit 1)\n{stderr}"
            );
            assert_eq!(
                stdout,
                expected.replace("\r\n", "\n").replace('\\', "/"),
                "{name}: checker output must match expected.txt byte-for-byte"
            );
        }
    }
}

#[test]
fn malformed_assertion_is_exit_2() {
    let case = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cases/malformed");
    let (code, _) = run(&case.join("index.json"), &case, &case);
    assert_eq!(
        code, 2,
        "an unknown assertion form must be exit 2, not a silent pass"
    );
}

#[test]
fn live_forms_pass_against_the_real_tree() {
    // dep/absent, dep/present (workspace-inherited AND crate-direct), a
    // missing-features-table feature-absent, and both conditional branches.
    let docs = temp_docs(&[
        "dep-absent insta",
        "dep-present proptest",
        "dep-present criterion",
        "feature-absent photonic-core/video-p1-contract",
        "if symbol-exists crates/photonic-video/src/session.rs::EngineCmd \
         then feature-absent photonic-core/video",
        "if symbol-exists crates/photonic-nope.rs::Nope then dep-present nonexistent",
    ]);
    let (code, stdout) = run(real_index(), &docs, &repo_root());
    assert_eq!(code, 0, "live-tree assertions should be clean:\n{stdout}");
}

#[test]
fn present_dep_asserted_absent_is_drift() {
    // proptest IS present, so `dep-absent proptest` must fail.
    let docs = temp_docs(&["dep-absent proptest"]);
    let (code, _) = run(real_index(), &docs, &repo_root());
    assert_eq!(code, 1, "an absent-claim on a present dep must be drift");
}

#[test]
fn acceptance_index_enforces_covers() {
    // ACC-40-06-07: gen-acceptance-index.py is the enforcement gate for §4.3's
    // hard failures. Each corpus is a minimal doc(+src) that must pass (0) or
    // fail (nonzero) validation.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cases/acceptance");
    let expect: [(&str, bool); 6] = [
        ("covered-with-test", true),
        ("waived-with-reason", true),
        ("covered-no-test", false),
        ("waived-no-reason", false),
        ("unknown-id", false),
        ("duplicate-id", false),
    ];
    for (name, should_pass) in expect {
        let case = root.join(name);
        let out = Command::new("python3")
            .env("PYTHONUTF8", "1")
            .arg(gen_acceptance())
            .arg("--check")
            .arg("--docs")
            .arg(case.join("docs"))
            .arg("--src")
            .arg(case.join("src"))
            .output()
            .expect("run gen-acceptance-index.py");
        let code = out.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if should_pass {
            assert_eq!(code, 0, "{name}: expected a clean index\n{stderr}");
        } else {
            assert_ne!(code, 0, "{name}: expected a hard failure, but it passed");
        }
    }
}

#[test]
fn acceptance_index_generation_is_deterministic() {
    // ACC-40-06-07: two runs over the same tree are byte-identical, and the
    // output carries the do-not-edit banner mirroring gen-mcp-docs.py.
    let case =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cases/acceptance/covered-with-test");
    let run = || {
        let out = Command::new("python3")
            .arg(gen_acceptance())
            .arg("--docs")
            .arg(case.join("docs"))
            .arg("--src")
            .arg(case.join("src"))
            .output()
            .expect("run gen-acceptance-index.py");
        assert!(out.status.success(), "generator failed");
        String::from_utf8(out.stdout).expect("utf8")
    };
    let a = run();
    let b = run();
    assert_eq!(a, b, "acceptance index generation must be deterministic");
    assert!(
        a.contains("GENERATED FILE — do not edit by hand."),
        "generated index must carry the do-not-edit banner"
    );
}

/// Recursively collect `*.md` files under `dir`.
fn md_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            md_files(&p, out);
        } else if p.extension().and_then(|x| x.to_str()) == Some("md") {
            out.push(p);
        }
    }
}

#[test]
fn the_real_docs_tree_is_clean() {
    // Task 3 smoke test: the live docs carry the SD-* `spec-assert` annotations
    // (>= 15 of them) and the checker is green over the whole real tree. A
    // reverted doc fix, or a code regression away from an annotated fact, reds
    // this. Proves the drift gate is wired to real claims, not just fixtures.
    let docs = repo_root().join("docs");
    let (code, stdout) = run(real_index(), &docs, &repo_root());
    assert_eq!(code, 0, "real docs tree must be drift-clean:\n{stdout}");

    let mut mds = Vec::new();
    md_files(&docs, &mut mds);
    let mut count = 0usize;
    for md in &mds {
        let text = std::fs::read_to_string(md).unwrap_or_default();
        for line in text.lines() {
            // Count real annotations, not doc 40's `<!-- spec-assert: <body> -->`
            // syntax template.
            if line.contains("<!-- spec-assert:") && !line.contains("spec-assert: <") {
                count += 1;
            }
        }
    }
    assert!(
        count >= 15,
        "expected >= 15 live spec-assert annotations across docs/, found {count}"
    );
}

#[test]
fn ci_wires_the_drift_gate() {
    // Task 6 (ACC-40-06-08): the blocking drift gate lives in CI beside the MCP
    // doc gate. Deleting it fails this test. (The acceptance-index half of §6 is
    // gated on the ACC-* id assignment of task 7 and is not wired here.)
    let ci =
        std::fs::read_to_string(repo_root().join(".github/workflows/ci.yml")).expect("read ci.yml");
    assert!(
        ci.contains("check-spec-drift.py"),
        "ci.yml must run the spec-drift checker"
    );
    assert!(
        ci.contains("spec-extract"),
        "ci.yml must build the workspace API index"
    );
}

#[test]
fn checker_runs_under_ten_seconds() {
    // ACC-40-06-06: extract + check over the whole workspace stays under budget.
    let start = Instant::now();
    let idx = std::env::temp_dir().join(format!("spec-index-timed-{}.json", std::process::id()));
    let status = Command::new(env!("CARGO_BIN_EXE_spec-extract"))
        .arg("--root")
        .arg(repo_root())
        .arg("--out")
        .arg(&idx)
        .status()
        .expect("run spec-extract");
    assert!(status.success());
    let docs = repo_root().join("docs");
    let (code, _) = run(&idx, &docs, &repo_root());
    assert!(code == 0 || code == 1, "checker exit was {code}");
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_secs() < 10,
        "extract+check took {elapsed:?}, over the 10 s budget"
    );
}
