//! The single first-class diagnostic taxonomy (36 §3).
//!
//! Every error and every silent degradation in the video editor reports to one
//! type — [`Diagnostic`] — instead of a per-subsystem `String`, a `tracing`
//! line, or a bespoke enum. A `Diagnostic` carries a stable machine [`DiagCode`]
//! (the MCP wire vocabulary and the drift-gate source of truth), a [`Severity`],
//! the [`Subject`] it is about, a human `message`, the `consequence` the user
//! will see, an optional [`Remedy`], and an optional technical `detail` (36 §4.2
//! — the ffmpeg stderr tail / encoder exit status, kept out of the primary
//! message and shown only in the log, support bundle, and on demand).
//!
//! This is a *taxonomy* module: it defines the types, the catalogue, and the
//! coalescing store ([`DiagnosticLog`], 36 §4.1). Wiring the emit sites
//! (compile, engine, decode, export, MCP, GUI) is done by the subsystems that
//! own those files.
//!
//! Not to be confused with [`crate::diagnostics`], which is the opt-in crash
//! reporter (#59). The two share the same privacy contract (see
//! [`support_bundle`]); they do not share a type.
//!
//! ## Invariants
//! - [`DiagCode::as_str`] is **stable wire vocabulary**: renaming a variant is a
//!   breaking change. [`DiagCode::ALL`] is the exhaustive catalogue that the
//!   completeness gate (`tests/diag_catalogue.rs`) checks against.
//! - [`DiagCode::consequence`] is never empty.
//! - [`Diagnostic`]'s `detail` never appears in its [`Display`](std::fmt::Display).
//! - [`Diagnostic`] is `Send + Sync + 'static`.

use crate::diagnostics::CrashReport;
use crate::timeline::ids::{AssetId, ClipId, GraphId, GraphNodeId, SequenceId, TrackId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Severity of a [`Diagnostic`] (36 §3.1).
///
/// Ordered `Info < Warning < Error < Fatal`, so the derived [`Ord`] picks the
/// worst diagnostic in a set (used by [`DiagnosticLog::worst`]).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Severity {
    /// Informational; the operation succeeded but the user should know.
    Info,
    /// Degraded, but the operation continued (e.g. a silent-passthrough effect).
    Warning,
    /// The operation for this subject failed; the rest of the session is fine.
    Error,
    /// Unrecoverable for the session; the app must offer to save (36 §4).
    Fatal,
}

/// A settings screen a [`Remedy::OpenSettings`] can deep-link to.
///
/// The spec (36) leaves this open; the smallest correct set is one page per
/// family that can be *misconfigured* by the user.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SettingsPage {
    /// Playback / preview configuration.
    Playback,
    /// Audio device and routing.
    Audio,
    /// Export presets and encoders.
    Export,
    /// Media handling (proxies, permitted roots).
    Media,
    /// GPU adapter / capability selection.
    Gpu,
}

/// The entity a [`Diagnostic`] is about (36 §3.1).
///
/// `Subject` is a coalescing key ([`DiagnosticLog`]), so it is `Copy + Eq +
/// Hash` and therefore holds only the `Copy` id newtypes from
/// [`crate::timeline::ids`] — never a name, path, or other borrowed data.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Subject {
    /// A media-pool asset.
    Asset(AssetId),
    /// A clip on a track.
    Clip(ClipId),
    /// A track in a sequence.
    Track(TrackId),
    /// A sequence in the project.
    Sequence(SequenceId),
    /// A node inside a specific graph.
    GraphNode {
        /// The graph the node lives in.
        graph: GraphId,
        /// The faulting node.
        node: GraphNodeId,
    },
    /// The `index`-th effect on a clip. `u32` (not `usize`) so the `Hash`/serde
    /// representation is stable across targets.
    Effect {
        /// The clip the effect is attached to.
        clip: ClipId,
        /// Zero-based position in the clip's effect stack.
        index: u32,
    },
    /// An export job. Carries the raw `Uuid` because `JobId` lives in
    /// `photonic-mcp` and `photonic-core` must not depend on it.
    Export(uuid::Uuid),
    /// The project as a whole (load, migration, validation).
    Project,
    /// The render/playback engine itself.
    Engine,
}

/// An id-free discriminant of [`Remedy`] (36 §3.1).
///
/// [`DiagCode::remedy_kind`] returns this because a `DiagCode` alone cannot know
/// *which* asset to relink or convert; the emit site binds the id when it builds
/// the concrete [`Remedy`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RemedyKind {
    /// Point a missing/moved asset at a new file.
    Relink,
    /// Open a settings page.
    OpenSettings(SettingsPage),
    /// Retry the operation.
    Retry,
    /// Transcode the asset to a supported form.
    ConvertMedia,
}

/// A concrete, actionable remedy the UI can present (36 §3.1).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Remedy {
    /// Relink the given asset.
    Relink(AssetId),
    /// Open the given settings page.
    OpenSettings(SettingsPage),
    /// Retry the operation.
    Retry,
    /// Transcode the given asset to a supported form.
    ConvertMedia(AssetId),
    /// No automated remedy; the message explains what to do.
    None,
}

/// The ten error families (36 §3.2).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DiagFamily {
    /// Media discovery / probing.
    Media,
    /// Decode-path (sidecar) faults.
    Decode,
    /// Graph compilation.
    Compile,
    /// GPU render.
    Render,
    /// Export / encode.
    Export,
    /// Audio engine.
    Audio,
    /// Project load / migration / validation.
    Project,
    /// Sandbox / authentication.
    Security,
    /// Import / export interchange formats.
    Interchange,
    /// Caption / subtitle handling.
    Caption,
}

/// Declares [`DiagCode`] together with the two impls that must stay in lockstep
/// with the variant set: [`DiagCode::ALL`] and the [`DiagCode::as_str`] /
/// [`FromStr`](std::str::FromStr) round-trip. Generating all three from one
/// variant list makes drift impossible: `as_str` is the variant name and
/// `from_str` its exact inverse.
macro_rules! diag_codes {
    ($( $(#[$m:meta])* $variant:ident ),+ $(,)?) => {
        /// The stable machine code for a diagnostic (36 §3.2) — one flat enum,
        /// family-prefixed, so `Hash`/serde/match are cheap and the MCP wire
        /// string is a single [`as_str`](DiagCode::as_str).
        ///
        /// Seeded with exactly the codes 36 §3.2 lists as *owned now*; codes that
        /// later specs register (unknown-variant preservation, interchange,
        /// localization, …) are intentionally absent.
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum DiagCode {
            $( $(#[$m])* $variant ),+
        }

        impl DiagCode {
            /// Every code, in declaration order. The catalogue the drift gate and
            /// the completeness test (36 §7.1) enumerate.
            pub const ALL: &'static [DiagCode] = &[ $( DiagCode::$variant ),+ ];

            /// The stable MCP wire string (the variant name verbatim).
            pub fn as_str(self) -> &'static str {
                match self { $( DiagCode::$variant => stringify!($variant) ),+ }
            }
        }

        impl std::str::FromStr for DiagCode {
            type Err = UnknownDiagCode;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $( stringify!($variant) => Ok(DiagCode::$variant), )+
                    _ => Err(UnknownDiagCode(s.to_string())),
                }
            }
        }
    };
}

diag_codes! {
    // Media (36 §3.2).
    MediaNotFound,
    MediaUnreadable,
    MediaUnsupportedCodec,
    MediaProbeFailed,
    MediaVariableFrameRate,
    MediaInterlaced,
    MediaNonSeekable,
    // Decode.
    DecodeSidecarCrashed,
    DecodeSidecarTimeout,
    DecodeSeekFailed,
    DecodeFrameDropped,
    // Compile.
    CompilePortTypeMismatch,
    CompileGraphCycle,
    CompileUnknownEffect,
    CompileEffectUnavailableAtScope,
    CompileParamOutOfRange,
    CompileTimeOffsetBudgetExceeded,
    CompileUnsupportedBlendMode,
    // Render.
    RenderDeviceLost,
    RenderOutOfMemory,
    RenderTextureTooLarge,
    RenderAdapterCapabilityMissing,
    // Export.
    ExportEncoderUnavailable,
    ExportEncoderFailed,
    ExportDiskFull,
    ExportPresetInvalid,
    ExportLoudnessCeilingBreached,
    // Audio.
    AudioDeviceUnavailable,
    AudioXrun,
    AudioSampleRateMismatch,
    AudioLatencyBudgetExceeded,
    // Project.
    ProjectVersionTooNew,
    ProjectMigrationFailed,
    ProjectValidationFailed,
    // Security.
    SecurityPathNotPermitted,
    SecurityUnauthenticated,
}

/// Error returned when a string does not name a known [`DiagCode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownDiagCode(pub String);

impl fmt::Display for UnknownDiagCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown diagnostic code: {}", self.0)
    }
}

impl std::error::Error for UnknownDiagCode {}

impl DiagCode {
    /// The family this code belongs to (36 §3.2).
    pub fn family(self) -> DiagFamily {
        use DiagCode::*;
        match self {
            MediaNotFound
            | MediaUnreadable
            | MediaUnsupportedCodec
            | MediaProbeFailed
            | MediaVariableFrameRate
            | MediaInterlaced
            | MediaNonSeekable => DiagFamily::Media,
            DecodeSidecarCrashed | DecodeSidecarTimeout | DecodeSeekFailed | DecodeFrameDropped => {
                DiagFamily::Decode
            }
            CompilePortTypeMismatch
            | CompileGraphCycle
            | CompileUnknownEffect
            | CompileEffectUnavailableAtScope
            | CompileParamOutOfRange
            | CompileTimeOffsetBudgetExceeded
            | CompileUnsupportedBlendMode => DiagFamily::Compile,
            RenderDeviceLost
            | RenderOutOfMemory
            | RenderTextureTooLarge
            | RenderAdapterCapabilityMissing => DiagFamily::Render,
            ExportEncoderUnavailable
            | ExportEncoderFailed
            | ExportDiskFull
            | ExportPresetInvalid
            | ExportLoudnessCeilingBreached => DiagFamily::Export,
            AudioDeviceUnavailable
            | AudioXrun
            | AudioSampleRateMismatch
            | AudioLatencyBudgetExceeded => DiagFamily::Audio,
            ProjectVersionTooNew | ProjectMigrationFailed | ProjectValidationFailed => {
                DiagFamily::Project
            }
            SecurityPathNotPermitted | SecurityUnauthenticated => DiagFamily::Security,
        }
    }

    /// The default severity for this code (36 §3.2). A [`Diagnostic::new`] takes
    /// this unless the emit site overrides it with
    /// [`with_severity`](Diagnostic::with_severity).
    pub fn default_severity(self) -> Severity {
        use DiagCode::*;
        match self {
            // Advisory: the media plays, but a property is worth surfacing.
            MediaVariableFrameRate | MediaInterlaced | MediaNonSeekable => Severity::Info,
            // Degraded but the operation continued.
            DecodeFrameDropped
            | AudioXrun
            | CompileUnsupportedBlendMode
            | CompileUnknownEffect
            | CompileTimeOffsetBudgetExceeded
            | CompileEffectUnavailableAtScope => Severity::Warning,
            // Unrecoverable for the session.
            RenderDeviceLost | ProjectMigrationFailed => Severity::Fatal,
            // Everything else is a plain error for its subject.
            _ => Severity::Error,
        }
    }

    /// The "what the user will see" line (36 §2.2). Never empty.
    pub fn consequence(self) -> &'static str {
        use DiagCode::*;
        match self {
            MediaNotFound => "The clip shows offline media until you relink it.",
            MediaUnreadable => "The file exists but cannot be read; the clip shows offline media.",
            MediaUnsupportedCodec => "This codec is not supported; convert the media to use it.",
            MediaProbeFailed => "The media could not be inspected; its clip may misbehave.",
            MediaVariableFrameRate => {
                "Variable frame rate detected; timing is conformed to a constant rate."
            }
            MediaInterlaced => {
                "Interlaced media detected; fields are handled with a default deinterlacer."
            }
            MediaNonSeekable => "This media cannot be seeked; scrubbing and trimming may be slow.",
            DecodeSidecarCrashed => "The decoder crashed; affected frames render as black.",
            DecodeSidecarTimeout => "The decoder timed out; affected frames render as black.",
            DecodeSeekFailed => "Seeking failed; playback resumes from the nearest keyframe.",
            DecodeFrameDropped => "A frame was dropped to keep up; the preview may stutter.",
            CompilePortTypeMismatch => {
                "Incompatible node connection; the graph falls back to a passthrough."
            }
            CompileGraphCycle => {
                "The graph contains a cycle; the cyclic branch renders transparent."
            }
            CompileUnknownEffect => "This effect is not available; the clip renders without it.",
            CompileEffectUnavailableAtScope => "This effect is not valid here; it is skipped.",
            CompileParamOutOfRange => "A parameter was out of range and was clamped.",
            CompileTimeOffsetBudgetExceeded => {
                "The time-offset budget was exceeded; the offset was capped."
            }
            CompileUnsupportedBlendMode => {
                "This blend mode is not supported on this path; Normal is used."
            }
            RenderDeviceLost => {
                "The GPU device was lost; the session cannot continue and must be saved."
            }
            RenderOutOfMemory => {
                "The GPU ran out of memory; try a lower resolution or fewer effects."
            }
            RenderTextureTooLarge => "A texture exceeds the device limit; reduce the resolution.",
            RenderAdapterCapabilityMissing => {
                "The GPU lacks a required capability; this feature is disabled."
            }
            ExportEncoderUnavailable => {
                "The chosen encoder is unavailable; pick another in Export settings."
            }
            ExportEncoderFailed => "The encoder failed; the export did not complete.",
            ExportDiskFull => {
                "The disk filled up; the export did not complete and no partial file remains."
            }
            ExportPresetInvalid => "The export preset is invalid; fix it in Export settings.",
            ExportLoudnessCeilingBreached => {
                "The mix exceeds the loudness ceiling; audio was limited."
            }
            AudioDeviceUnavailable => "No audio device is available; playback is silent.",
            AudioXrun => "An audio buffer under-ran; you may hear a brief glitch.",
            AudioSampleRateMismatch => "The device sample rate differs; audio is resampled.",
            AudioLatencyBudgetExceeded => "Audio latency exceeded its budget; sync may drift.",
            ProjectVersionTooNew => {
                "This project was saved by a newer version; open it there or upgrade."
            }
            ProjectMigrationFailed => {
                "The project could not be migrated and cannot be opened safely."
            }
            ProjectValidationFailed => "The project failed validation; some data may be ignored.",
            SecurityPathNotPermitted => "That path is outside the permitted roots and was refused.",
            SecurityUnauthenticated => "The request was not authenticated and was refused.",
        }
    }

    /// The kind of remedy the UI should offer, if any (36 §3.1). The concrete
    /// [`Remedy`] (with its id) is bound by the emit site.
    pub fn remedy_kind(self) -> Option<RemedyKind> {
        use DiagCode::*;
        Some(match self {
            MediaNotFound | MediaUnreadable => RemedyKind::Relink,
            MediaUnsupportedCodec | MediaNonSeekable | MediaProbeFailed => RemedyKind::ConvertMedia,
            DecodeSidecarCrashed | DecodeSidecarTimeout | DecodeSeekFailed
            | ExportEncoderFailed | RenderDeviceLost => RemedyKind::Retry,
            RenderOutOfMemory | RenderTextureTooLarge | RenderAdapterCapabilityMissing => {
                RemedyKind::OpenSettings(SettingsPage::Gpu)
            }
            ExportEncoderUnavailable | ExportPresetInvalid => {
                RemedyKind::OpenSettings(SettingsPage::Export)
            }
            ExportLoudnessCeilingBreached
            | AudioDeviceUnavailable
            | AudioXrun
            | AudioSampleRateMismatch
            | AudioLatencyBudgetExceeded => RemedyKind::OpenSettings(SettingsPage::Audio),
            SecurityPathNotPermitted => RemedyKind::OpenSettings(SettingsPage::Media),
            // No automated remedy: informational codes, compile fallbacks the
            // user cannot act on directly, disk-full, and project/auth faults.
            _ => return None,
        })
    }
}

/// A single first-class diagnostic (36 §3.1).
///
/// Build one with [`Diagnostic::new`] so `severity` and `consequence` are always
/// consistent with `code`; use the `with_*` builders for the exceptions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// The stable machine code.
    pub code: DiagCode,
    /// Severity; defaults to [`DiagCode::default_severity`].
    pub severity: Severity,
    /// The entity this is about.
    pub subject: Subject,
    /// The human-readable primary message.
    pub message: String,
    /// What the user will see as a result ([`DiagCode::consequence`]).
    pub consequence: String,
    /// An actionable remedy, if one exists.
    pub remedy: Option<Remedy>,
    /// §4.2 technical detail (ffmpeg stderr tail, encoder exit status). Never
    /// shown in the primary message; log + support bundle + on-demand only.
    pub detail: Option<String>,
}

impl Diagnostic {
    /// Build a diagnostic, taking `severity` from [`DiagCode::default_severity`]
    /// and `consequence` from [`DiagCode::consequence`], so no caller can
    /// produce a code/severity-inconsistent diagnostic by accident.
    pub fn new(code: DiagCode, subject: Subject, message: impl Into<String>) -> Self {
        Diagnostic {
            code,
            severity: code.default_severity(),
            subject,
            message: message.into(),
            consequence: code.consequence().to_string(),
            remedy: None,
            detail: None,
        }
    }

    /// Override the severity (the exception to [`DiagCode::default_severity`]).
    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }

    /// Attach a concrete remedy.
    pub fn with_remedy(mut self, remedy: Remedy) -> Self {
        self.remedy = Some(remedy);
        self
    }

    /// Attach the §4.2 technical detail (kept out of [`Display`](fmt::Display)).
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

impl fmt::Display for Diagnostic {
    /// Renders `"{message} — {consequence}"`. Deliberately excludes `detail`
    /// (36 §4.2): technical detail never rides the primary presentation.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} — {}", self.message, self.consequence)
    }
}

/// One coalesced entry in a [`DiagnosticLog`] (36 §4.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoalescedDiagnostic {
    /// The **first** occurrence of `(code, subject)` — its message and detail
    /// never churn as duplicates arrive.
    pub diagnostic: Diagnostic,
    /// How many times `(code, subject)` has been recorded.
    pub count: u64,
    /// The monotonic insert sequence of the first occurrence — the ordering key.
    pub first_seq: u64,
    /// The insert sequence of the most recent occurrence.
    pub last_seq: u64,
}

/// A coalescing diagnostic store keyed on `(code, subject)` (36 §4.1).
///
/// Mandatory for every surface: a decode storm of 400 failing frames becomes one
/// entry with `count == 400`, so the panel, badges, and a single toast stay
/// deterministic and allocation-free on the duplicate path.
#[derive(Clone, Debug, Default)]
pub struct DiagnosticLog {
    index: HashMap<(DiagCode, Subject), usize>,
    entries: Vec<CoalescedDiagnostic>,
    capacity: usize,
    next_seq: u64,
    revision: u64,
}

impl DiagnosticLog {
    /// A log holding at most `capacity` distinct `(code, subject)` entries
    /// (minimum 1). At capacity, [`record`](Self::record) evicts the
    /// lowest-severity oldest entry.
    pub fn new(capacity: usize) -> Self {
        DiagnosticLog {
            index: HashMap::new(),
            entries: Vec::new(),
            capacity: capacity.max(1),
            next_seq: 0,
            revision: 0,
        }
    }

    /// Record a diagnostic. Returns `true` iff this was the **first** occurrence
    /// of `(code, subject)` — the toast trigger. A duplicate only bumps
    /// `count`/`last_seq` and drops the incoming `Diagnostic` (no allocation).
    pub fn record(&mut self, d: Diagnostic) -> bool {
        let seq = self.next_seq;
        self.next_seq += 1;
        let key = (d.code, d.subject);

        if let Some(&idx) = self.index.get(&key) {
            let entry = &mut self.entries[idx];
            entry.count += 1;
            entry.last_seq = seq;
            self.revision += 1;
            return false;
        }

        if self.entries.len() >= self.capacity {
            self.evict_one();
        }

        self.entries.push(CoalescedDiagnostic {
            diagnostic: d,
            count: 1,
            first_seq: seq,
            last_seq: seq,
        });
        self.reindex();
        self.revision += 1;
        true
    }

    /// The coalesced entries, ordered by `first_seq` ascending (insertion order,
    /// never `HashMap` order) so goldens and the panel are deterministic.
    pub fn entries(&self) -> &[CoalescedDiagnostic] {
        &self.entries
    }

    /// A revision counter that bumps on every state change — including a count
    /// bump — so a status signature can cheaply detect churn.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Drop every entry for `subject` once its condition clears (badge lifetime,
    /// 36 §4).
    pub fn clear_subject(&mut self, subject: Subject) {
        let before = self.entries.len();
        self.entries.retain(|e| e.diagnostic.subject != subject);
        if self.entries.len() != before {
            self.reindex();
            self.revision += 1;
        }
    }

    /// Drop everything.
    pub fn clear(&mut self) {
        if !self.entries.is_empty() {
            self.entries.clear();
            self.index.clear();
            self.revision += 1;
        }
    }

    /// The worst entry: highest severity, then most recent (`last_seq`).
    pub fn worst(&self) -> Option<&CoalescedDiagnostic> {
        self.entries
            .iter()
            .max_by_key(|e| (e.diagnostic.severity, e.last_seq))
    }

    /// Evict the lowest-severity oldest entry. `entries` is ordered by
    /// `first_seq`, so the first entry of the minimum severity is the oldest of
    /// that severity; a highest-severity entry is never evicted.
    fn evict_one(&mut self) {
        if let Some((victim, _)) = self
            .entries
            .iter()
            .enumerate()
            .min_by_key(|(_, e)| (e.diagnostic.severity, e.first_seq))
        {
            self.entries.remove(victim);
            self.reindex();
        }
    }

    /// Rebuild the `(code, subject) -> position` index after the `entries`
    /// positions changed (a push after eviction, an eviction, or a clear).
    fn reindex(&mut self) {
        self.index.clear();
        for (i, e) in self.entries.iter().enumerate() {
            self.index
                .insert((e.diagnostic.code, e.diagnostic.subject), i);
        }
    }
}

/// A privacy-preserving support bundle (36 §7.9).
///
/// Emits JSON containing the recent coalesced diagnostics (including their
/// `detail`) and the crash reports — and **no media content, no file paths from
/// the user's project, and no document data**. This is the same privacy contract
/// documented for [`CrashReport`](crate::diagnostics::CrashReport): asset
/// subjects serialise as opaque uuids, never filenames, because [`Subject`]
/// holds only id newtypes.
pub fn support_bundle(log: &DiagnosticLog, crash_reports: &[CrashReport]) -> String {
    #[derive(Serialize)]
    struct Bundle<'a> {
        diagnostics: &'a [CoalescedDiagnostic],
        crash_reports: &'a [CrashReport],
    }
    let bundle = Bundle {
        diagnostics: log.entries(),
        crash_reports,
    };
    serde_json::to_string_pretty(&bundle).unwrap_or_else(|_| "{}".to_string())
}
