//! Catalogue completeness gate for the diagnostic taxonomy (36 §7.1).
//!
//! 36 §7.1 requires that *every* [`DiagCode`] has a message, a consequence, and
//! a test that produces it. The full emit-site coverage half of that gate lands
//! with the subsystems that own the emit sites (compile, engine, decode, export,
//! MCP, GUI). This file locks down the *taxonomy* half that lives entirely in
//! `photonic-core`:
//!
//! - the catalogue [`DiagCode::ALL`] is exhaustive and duplicate-free,
//! - every code carries a non-empty consequence and a stable, unique wire
//!   string that round-trips through `FromStr`,
//! - [`DiagCode::family`] partitions the whole catalogue,
//! - the seeded set is exactly the codes 36 §3.2 lists as owned *now* — codes
//!   that later specs register (unknown-variant preservation, interchange,
//!   localization, sequence semantics, …) are deliberately absent, so adding one
//!   early trips this gate.
//!
//! Reverting any of `diag.rs`'s inherent impls, or dropping a variant from
//! `ALL`, fails this test — it is not vacuous.

use std::collections::HashSet;

use photonic_core::diag::{DiagCode, DiagFamily};

/// The exact catalogue 36 §3.2 seeds as owned-now. Kept here as the independent
/// second source: if `diag.rs` and this list disagree, the taxonomy drifted.
const EXPECTED_WIRE_CODES: &[&str] = &[
    "MediaNotFound",
    "MediaUnreadable",
    "MediaUnsupportedCodec",
    "MediaProbeFailed",
    "MediaVariableFrameRate",
    "MediaInterlaced",
    "MediaNonSeekable",
    "DecodeSidecarCrashed",
    "DecodeSidecarTimeout",
    "DecodeSeekFailed",
    "DecodeFrameDropped",
    "CompilePortTypeMismatch",
    "CompileGraphCycle",
    "CompileUnknownEffect",
    "CompileEffectUnavailableAtScope",
    "CompileParamOutOfRange",
    "CompileTimeOffsetBudgetExceeded",
    "CompileUnsupportedBlendMode",
    "RenderDeviceLost",
    "RenderOutOfMemory",
    "RenderTextureTooLarge",
    "RenderAdapterCapabilityMissing",
    "ExportEncoderUnavailable",
    "ExportEncoderFailed",
    "ExportDiskFull",
    "ExportPresetInvalid",
    "ExportLoudnessCeilingBreached",
    "AudioDeviceUnavailable",
    "AudioXrun",
    "AudioSampleRateMismatch",
    "AudioLatencyBudgetExceeded",
    "ProjectVersionTooNew",
    "ProjectMigrationFailed",
    "ProjectValidationFailed",
    "SecurityPathNotPermitted",
    "SecurityUnauthenticated",
];

#[test]
fn catalogue_matches_the_owned_now_set_exactly() {
    let actual: HashSet<&str> = DiagCode::ALL.iter().map(|c| c.as_str()).collect();
    let expected: HashSet<&str> = EXPECTED_WIRE_CODES.iter().copied().collect();

    let added: Vec<&str> = actual.difference(&expected).copied().collect();
    let missing: Vec<&str> = expected.difference(&actual).copied().collect();

    assert!(
        added.is_empty(),
        "DiagCode has codes not in 36 §3.2's owned-now set (register them with \
         their spec, or add here deliberately): {added:?}"
    );
    assert!(
        missing.is_empty(),
        "DiagCode is missing owned-now codes from 36 §3.2: {missing:?}"
    );
    // No duplicate wire strings, and ALL has no accidental repeats.
    assert_eq!(actual.len(), DiagCode::ALL.len(), "duplicate code in ALL");
    assert_eq!(DiagCode::ALL.len(), EXPECTED_WIRE_CODES.len());
}

#[test]
fn every_code_round_trips_and_has_a_consequence() {
    for &code in DiagCode::ALL {
        let wire = code.as_str();
        assert!(!wire.is_empty(), "empty wire string");
        assert_eq!(
            wire.parse::<DiagCode>().expect("wire string must parse"),
            code,
            "as_str/FromStr are not inverses for {wire}"
        );
        assert!(
            !code.consequence().is_empty(),
            "{wire} has no consequence (36 §2.2 requires one)"
        );
    }
}

#[test]
fn family_partitions_the_catalogue() {
    const FAMILIES: &[DiagFamily] = &[
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
    ];
    let counted: usize = FAMILIES
        .iter()
        .map(|&fam| DiagCode::ALL.iter().filter(|c| c.family() == fam).count())
        .sum();
    assert_eq!(
        counted,
        DiagCode::ALL.len(),
        "every code must map to exactly one family"
    );
}
