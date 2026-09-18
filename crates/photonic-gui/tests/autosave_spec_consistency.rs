//! Acceptance test 10 (37 §2.5): keep SPEC.md CAP-022 and the autosave default
//! from drifting apart.
//!
//! CAP-022 promises "at most N minutes of work lost", where N is the autosave
//! interval. If someone changes the default without updating the prose (or vice
//! versa) the two facts diverge silently. This test reads the number out of
//! CAP-022 and asserts it equals `autosave_interval_secs / 60`.

use photonic_gui::AppPreferences;

fn spec_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/specs/video-editor/SPEC.md")
}

/// Extract the integer minute count from the CAP-022 line, which reads
/// "...at most two minutes of work lost...". We map the spelled-out number
/// words we expect to appear, so the test fails loudly if the prose is reworded
/// into a form it no longer recognises.
fn cap022_minutes(line: &str) -> Option<f64> {
    const WORDS: &[(&str, f64)] = &[
        ("one minute", 1.0),
        ("two minutes", 2.0),
        ("three minutes", 3.0),
        ("four minutes", 4.0),
        ("five minutes", 5.0),
    ];
    let lower = line.to_lowercase();
    WORDS
        .iter()
        .find(|(w, _)| lower.contains(w))
        .map(|(_, m)| *m)
}

#[test]
fn cap022_minutes_match_autosave_default() {
    let spec = std::fs::read_to_string(spec_path()).expect("SPEC.md must be readable");
    let cap = spec
        .lines()
        .find(|l| l.contains("CAP-022"))
        .expect("SPEC.md must contain a CAP-022 line");

    let stated = cap022_minutes(cap).unwrap_or_else(|| {
        panic!("CAP-022 line does not state a recognised minute count: {cap:?}")
    });

    let default_minutes = AppPreferences::default().autosave_interval_secs / 60.0;
    assert_eq!(
        stated, default_minutes,
        "CAP-022 promises {stated} minutes but the autosave default is {default_minutes} minutes"
    );
}
