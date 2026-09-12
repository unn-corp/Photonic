//! Proves ACC-40-06-01: the extractor emits a deterministic structural index
//! whose records match the real workspace source. Runs the built binary over
//! the actual repo root and asserts on ground-truth items.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// Repo root = two levels above this crate (tools/spec-extract/../..).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("resolve repo root")
}

/// Run `spec-extract --root <repo>` and return its stdout.
fn run_extract() -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_spec-extract"))
        .arg("--root")
        .arg(repo_root())
        .output()
        .expect("run spec-extract");
    assert!(
        out.status.success(),
        "spec-extract failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8 stdout")
}

fn items(json: &Value) -> &Vec<Value> {
    assert_eq!(json["schema"], 1, "schema tag");
    json["items"].as_array().expect("items array")
}

/// Ground truth for a `pub const` in `photonic-core/src/document.rs`, read
/// straight from the source: `(1-based line, value literal)`.
///
/// Deliberately a dumb text scan rather than a second parser — if it agreed
/// with the extractor by sharing its logic it would prove nothing.
fn const_ground_truth(name: &str) -> (usize, String) {
    let path = repo_root()
        .join("crates")
        .join("photonic-core")
        .join("src")
        .join("document.rs");
    let src = std::fs::read_to_string(&path).expect("read document.rs");
    let needle = format!("pub const {name}");
    for (i, line) in src.lines().enumerate() {
        let Some(rest) = line.trim_start().strip_prefix(&needle) else {
            continue;
        };
        let value = rest
            .split('=')
            .nth(1)
            .expect("const has an initialiser")
            .trim()
            .trim_end_matches(';')
            .trim()
            .to_string();
        return (i + 1, value);
    }
    panic!("no `pub const {name}` in {}", path.display());
}

fn find<'a>(items: &'a [Value], file: &str, name: &str) -> &'a Value {
    items
        .iter()
        .find(|it| it["file"] == file && it["name"] == name)
        .unwrap_or_else(|| panic!("no item {name} in {file}"))
}

#[test]
fn indexes_const_enum_and_fields_in_order() {
    let raw = run_extract();
    let json: Value = serde_json::from_str(&raw).expect("parse index json");
    let items = items(&json);

    // (a) CURRENT_FORMAT_VERSION const in document.rs.
    let ver = find(
        items,
        "crates/photonic-core/src/document.rs",
        "CURRENT_FORMAT_VERSION",
    );
    // This test proves the *extractor* reports the const faithfully, so both
    // the value and the line are derived from the source rather than pinned to
    // literals. Hardcoding them made this test stale twice over — the constant
    // moved to a new line and bumped v4→v5, and because CI never ran on the
    // feature branch nobody saw it go red. Pinning the literal version number
    // is the job of 01-data-model.md's `spec-assert`, which check-spec-drift.py
    // evaluates; duplicating that pin here only doubles the maintenance.
    let (want_line, want_value) = const_ground_truth("CURRENT_FORMAT_VERSION");
    assert_eq!(ver["kind"], "Const", "kind of CURRENT_FORMAT_VERSION");
    assert_eq!(ver["value"], want_value, "value of CURRENT_FORMAT_VERSION");
    assert_eq!(ver["line"], want_line, "line of CURRENT_FORMAT_VERSION");

    // (b) EngineCmd enum in session.rs, variants in exact declaration order.
    let cmd = find(items, "crates/photonic-video/src/session.rs", "EngineCmd");
    assert_eq!(cmd["kind"], "Enum", "kind of EngineCmd");
    let variant_names: Vec<String> = cmd["variants"]
        .as_array()
        .expect("variants array")
        .iter()
        .map(|v| v["name"].as_str().expect("variant name").to_string())
        .collect();
    assert_eq!(
        variant_names,
        vec![
            // Cached timeline-preview controls are declared first so the
            // engine command surface keeps the workflow family together.
            "SetPreviewCacheDir",
            "SetPreviewProfile",
            "RenderPreview",
            "CancelPreview",
            "ClearPreview",
            "InspectFrame",
            "AuditionSource",
            "StopSourceAudition",
            "Play",
            "Pause",
            "Seek",
            "ScrubSeek",
            "Step",
            "SetLoop",
            "SetActiveSequence",
            "SetProxyMode",
            "SetPreviewTarget",
            "SetPreviewQuality",
            // K-E2 scope tap — view state, sent beside the other Set* view
            // commands. The literal list caught this addition, which is exactly
            // the notice-me behaviour the note below asks of it.
            "SetScopeTap",
            // K-B5 compare-effect mode — view state too, declared next to
            // `SetScopeTap` for that reason. The literal list caught this one
            // as well, on the same terms as the note below.
            "AnalyzeStabilization",
            "SetCompareEffects",
            "SeekSource",
            "InvalidateRange",
            "Export",
            // Landed with K-0.1's export progress/cancel wiring. Kept as a
            // literal list on purpose: unlike the const's line number, the
            // variant set only changes when someone edits the enum, and that
            // is exactly the event this assertion should make them notice.
            "CancelExport",
            "Probe",
            "Shutdown",
        ],
        "EngineCmd variants must match source declaration order"
    );

    // (c) SeekSource struct-variant field names in order.
    let seek_source = cmd["variants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "SeekSource")
        .expect("SeekSource variant");
    let fields: Vec<String> = seek_source["fields"]
        .as_array()
        .expect("fields array")
        .iter()
        .map(|f| f.as_str().unwrap().to_string())
        .collect();
    assert_eq!(fields, vec!["asset", "time"], "SeekSource fields");
}

/// Run `spec-extract --root <root>` and return its stdout.
fn run_extract_root(root: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_spec-extract"))
        .arg("--root")
        .arg(root)
        .output()
        .expect("run spec-extract");
    assert!(
        out.status.success(),
        "spec-extract failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8 stdout")
}

#[test]
fn output_is_byte_identical_across_runs() {
    // (d) determinism: two independent runs over the SAME tree must produce
    // identical bytes. We run over a private fixture workspace rather than the
    // real repo root — sibling agents mutate the live tree concurrently, which
    // would make a repo-root comparison flaky for reasons unrelated to the tool.
    let dir = std::env::temp_dir().join(format!(
        "spec-extract-determinism-{}-{}",
        std::process::id(),
        env!("CARGO_PKG_NAME"),
    ));
    let src = dir.join("crates").join("mini").join("src");
    std::fs::create_dir_all(&src).expect("create fixture dirs");
    std::fs::write(
        src.join("lib.rs"),
        "pub const N: u32 = 4;\n\
         pub enum E { A, B(u8), C { x: i32, y: i32 } }\n\
         pub struct S { pub a: u8, b: u16 }\n\
         mod inner { pub fn f(a: u8) -> u8 { a } }\n",
    )
    .expect("write fixture source");

    let a = run_extract_root(&dir);
    let b = run_extract_root(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(a, b, "spec-extract output must be deterministic");
}
