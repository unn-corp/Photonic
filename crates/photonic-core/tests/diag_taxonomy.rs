//! Behavioural tests for the diagnostic taxonomy and coalescing store (36 §3,
//! §4.1, §7.9), exercised entirely through the public API.
//!
//! Kept as an integration test (its own binary) rather than a `#[cfg(test)]`
//! module so it compiles against only the crate's public surface — the same
//! surface every emit site and every GUI/MCP surface will use.

use photonic_core::diag::{
    support_bundle, DiagCode, DiagFamily, Diagnostic, DiagnosticLog, Severity, Subject,
};
use photonic_core::diagnostics::CrashReport;
use photonic_core::timeline::{AssetId, ClipId, TrackId};

// Invariant (d): `Diagnostic` (and the log) are `Send + Sync + 'static`.
fn assert_send_sync<T: Send + Sync + 'static>() {}

#[test]
fn diagnostic_is_send_sync_static() {
    assert_send_sync::<Diagnostic>();
    assert_send_sync::<DiagnosticLog>();
}

#[test]
fn wire_strings_round_trip_and_are_unique() {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    for &code in DiagCode::ALL {
        let s = code.as_str();
        assert!(!s.is_empty());
        // Round-trip: as_str is the exact inverse of FromStr.
        assert_eq!(s.parse::<DiagCode>().unwrap(), code);
        // Wire strings are the coalescing/serialize vocabulary — no dupes.
        assert!(seen.insert(s), "duplicate wire string {s}");
    }
    assert!("NotACode".parse::<DiagCode>().is_err());
}

#[test]
fn every_code_has_a_nonempty_consequence() {
    for &code in DiagCode::ALL {
        assert!(
            !code.consequence().is_empty(),
            "{} has an empty consequence",
            code.as_str()
        );
    }
}

#[test]
fn default_severity_matches_spec_table() {
    assert_eq!(DiagCode::MediaInterlaced.default_severity(), Severity::Info);
    assert_eq!(
        DiagCode::DecodeFrameDropped.default_severity(),
        Severity::Warning
    );
    assert_eq!(
        DiagCode::CompileUnsupportedBlendMode.default_severity(),
        Severity::Warning
    );
    assert_eq!(
        DiagCode::RenderDeviceLost.default_severity(),
        Severity::Fatal
    );
    assert_eq!(
        DiagCode::ProjectMigrationFailed.default_severity(),
        Severity::Fatal
    );
    // The catch-all is Error.
    assert_eq!(DiagCode::MediaNotFound.default_severity(), Severity::Error);
}

#[test]
fn new_fills_severity_and_consequence_from_code() {
    let d = Diagnostic::new(
        DiagCode::MediaNotFound,
        Subject::Asset(AssetId::nil()),
        "clip.mov is offline",
    );
    assert_eq!(d.severity, DiagCode::MediaNotFound.default_severity());
    assert_eq!(d.consequence, DiagCode::MediaNotFound.consequence());
    assert_eq!(d.remedy, None);
    assert_eq!(d.detail, None);
}

#[test]
fn display_excludes_detail() {
    let d = Diagnostic::new(
        DiagCode::DecodeSidecarCrashed,
        Subject::Clip(ClipId::nil()),
        "decoder died",
    )
    .with_detail("ffmpeg: signal 11 (SIGSEGV)\nlast stderr line");
    let shown = d.to_string();
    assert!(shown.contains("decoder died"));
    assert!(shown.contains(DiagCode::DecodeSidecarCrashed.consequence()));
    // §4.2: technical detail never appears in the primary presentation.
    assert!(!shown.contains("SIGSEGV"));
    assert!(!shown.contains("ffmpeg"));
}

#[test]
fn families_partition_all_codes() {
    // Every code maps to a family, and the per-family counts sum to ALL.
    let total: usize = [
        DiagFamily::Media,
        DiagFamily::Decode,
        DiagFamily::Compile,
        DiagFamily::Render,
        DiagFamily::Export,
        DiagFamily::Audio,
        DiagFamily::Project,
        DiagFamily::Security,
        DiagFamily::Interchange,
        DiagFamily::Caption,
    ]
    .iter()
    .map(|&fam| DiagCode::ALL.iter().filter(|c| c.family() == fam).count())
    .sum();
    assert_eq!(total, DiagCode::ALL.len());
}

fn diag(code: DiagCode, subject: Subject) -> Diagnostic {
    Diagnostic::new(code, subject, "x")
}

#[test]
fn coalesces_on_code_and_subject() {
    let mut log = DiagnosticLog::new(16);
    let subj = Subject::Clip(ClipId::nil());
    // First occurrence is the only `true` — the toast trigger.
    assert!(log.record(diag(DiagCode::DecodeFrameDropped, subj)));
    let mut trues = 0;
    for _ in 0..399 {
        if log.record(diag(DiagCode::DecodeFrameDropped, subj)) {
            trues += 1;
        }
    }
    assert_eq!(trues, 0, "a decode storm must fire exactly one toast");
    assert_eq!(log.entries().len(), 1);
    assert_eq!(log.entries()[0].count, 400);
}

#[test]
fn stored_diagnostic_is_the_first_occurrence() {
    let mut log = DiagnosticLog::new(16);
    let subj = Subject::Clip(ClipId::nil());
    log.record(Diagnostic::new(DiagCode::DecodeFrameDropped, subj, "first"));
    log.record(Diagnostic::new(
        DiagCode::DecodeFrameDropped,
        subj,
        "second",
    ));
    // The message never churns under the user.
    assert_eq!(log.entries()[0].diagnostic.message, "first");
}

#[test]
fn entries_are_ordered_by_first_seq() {
    let mut log = DiagnosticLog::new(16);
    let a = Subject::Clip(ClipId::nil());
    let b = Subject::Track(TrackId::nil());
    let c = Subject::Project;
    log.record(diag(DiagCode::DecodeFrameDropped, a));
    log.record(diag(DiagCode::CompileGraphCycle, b));
    log.record(diag(DiagCode::ProjectValidationFailed, c));
    // Re-touching the first entry must not reorder it.
    log.record(diag(DiagCode::DecodeFrameDropped, a));
    let seqs: Vec<u64> = log.entries().iter().map(|e| e.first_seq).collect();
    let mut sorted = seqs.clone();
    sorted.sort_unstable();
    assert_eq!(seqs, sorted);
    assert_eq!(
        log.entries()[0].diagnostic.code,
        DiagCode::DecodeFrameDropped
    );
}

#[test]
fn revision_bumps_on_a_count_bump() {
    let mut log = DiagnosticLog::new(16);
    let subj = Subject::Clip(ClipId::nil());
    log.record(diag(DiagCode::DecodeFrameDropped, subj));
    let r = log.revision();
    // A pure duplicate is still a state change.
    log.record(diag(DiagCode::DecodeFrameDropped, subj));
    assert!(log.revision() > r);
}

#[test]
fn worst_picks_highest_severity() {
    let mut log = DiagnosticLog::new(16);
    log.record(diag(
        DiagCode::DecodeFrameDropped,
        Subject::Clip(ClipId::nil()),
    )); // Warning
    log.record(diag(
        DiagCode::MediaNotFound,
        Subject::Asset(AssetId::nil()),
    )); // Error
    log.record(diag(DiagCode::RenderDeviceLost, Subject::Engine)); // Fatal
    assert_eq!(
        log.worst().unwrap().diagnostic.code,
        DiagCode::RenderDeviceLost
    );
}

#[test]
fn eviction_drops_lowest_severity_oldest_never_highest() {
    // Capacity 2. Insert a Fatal, then two Warnings on distinct subjects.
    let mut log = DiagnosticLog::new(2);
    log.record(diag(DiagCode::RenderDeviceLost, Subject::Engine)); // Fatal
    log.record(diag(
        DiagCode::DecodeFrameDropped,
        Subject::Clip(ClipId::nil()),
    )); // Warning
        // This overflows: the lowest-severity oldest (the Warning) is evicted,
        // never the Fatal.
    log.record(diag(DiagCode::AudioXrun, Subject::Track(TrackId::nil()))); // Warning
    let codes: Vec<DiagCode> = log.entries().iter().map(|e| e.diagnostic.code).collect();
    assert_eq!(log.entries().len(), 2);
    assert!(codes.contains(&DiagCode::RenderDeviceLost));
    assert!(!codes.contains(&DiagCode::DecodeFrameDropped));
    assert!(codes.contains(&DiagCode::AudioXrun));
    // The index stayed consistent: the surviving Warning still coalesces.
    assert!(!log.record(diag(DiagCode::AudioXrun, Subject::Track(TrackId::nil()))));
    assert_eq!(
        log.entries()
            .iter()
            .find(|e| e.diagnostic.code == DiagCode::AudioXrun)
            .unwrap()
            .count,
        2
    );
}

#[test]
fn clear_subject_drops_only_that_subject() {
    let mut log = DiagnosticLog::new(16);
    let clip = Subject::Clip(ClipId::nil());
    log.record(diag(DiagCode::DecodeFrameDropped, clip));
    log.record(diag(DiagCode::ProjectValidationFailed, Subject::Project));
    log.clear_subject(clip);
    let codes: Vec<DiagCode> = log.entries().iter().map(|e| e.diagnostic.code).collect();
    assert_eq!(codes, vec![DiagCode::ProjectValidationFailed]);
    // Re-recording the cleared subject is a fresh first occurrence.
    assert!(log.record(diag(DiagCode::DecodeFrameDropped, clip)));
}

#[test]
fn support_bundle_is_json_with_diagnostics_and_no_filenames() {
    let mut log = DiagnosticLog::new(16);
    log.record(
        Diagnostic::new(
            DiagCode::MediaNotFound,
            Subject::Asset(AssetId::nil()),
            "asset offline",
        )
        .with_detail("internal note"),
    );
    let reports = [CrashReport {
        version: "1.0.0".to_string(),
        timestamp: "t".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        panic_message: "boom".to_string(),
        location: None,
        backtrace: String::new(),
    }];
    let bundle = support_bundle(&log, &reports);
    // Valid JSON carrying both sections.
    let parsed: serde_json::Value = serde_json::from_str(&bundle).unwrap();
    assert!(parsed["diagnostics"].is_array());
    assert_eq!(parsed["diagnostics"].as_array().unwrap().len(), 1);
    assert_eq!(parsed["crash_reports"].as_array().unwrap().len(), 1);
    // The MediaNotFound wire code and the technical detail are present…
    assert!(bundle.contains("MediaNotFound"));
    assert!(bundle.contains("internal note"));
    // …but the asset is an opaque uuid, never a filename.
    assert!(bundle.contains(&AssetId::nil().0.to_string()));
}
