//! AI provider trait abstraction: transcription + TTS (06 §2.1, verbatim
//! shape).
//!
//! `photonic-video` is thread-based, not `async`/`await` (02 §1). Provider
//! calls follow the same model: a blocking trait method runs on a provider
//! worker, progress streams out over a `crossbeam_channel`, cancellation is a
//! shared flag ([`CancelToken`]). No document mutation happens in this
//! module — a caller (the engine job runner, out of this story's scope)
//! turns a completed [`TranscriptionResult`]/[`TtsResult`] into a
//! `CaptionCmd`/`TtsCmd` only on success (06 §2.1, §3.7).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use photonic_core::timeline::Tick;

/// A shared cancellation flag for one provider job. Cheap to clone — every
/// clone observes the same underlying flag.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        CancelToken(Arc::new(AtomicBool::new(false)))
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Progress channel a provider call streams updates over (06 §2.1). Jobs
/// share the engine's job-id space (02 §1); this story only defines the
/// provider-facing contract, not the job runner itself.
pub type ProgressSink = crossbeam_channel::Sender<ProviderProgress>;

#[derive(Clone, Debug)]
pub enum ProviderProgress {
    Started,
    Uploading {
        sent: u64,
        total: Option<u64>,
    },
    /// Provider-reported percent; `None` if the provider gives no signal.
    Processing {
        percent: Option<f32>,
    },
    /// Streamed partial words (transcription only, if the provider supports
    /// it). GUI may preview-populate the caption track before completion.
    Partial(TranscriptionResult),
    Done,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ProviderError {
    #[error("provider unavailable (no network / service unreachable)")]
    Unavailable,
    #[error("unauthorized (auth token missing, expired, or rejected)")]
    Unauthorized,
    #[error("rate limited")]
    RateLimited,
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("timed out")]
    Timeout,
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

/// A blocking transcription provider (06 §2.1). Implementors run on a
/// provider worker thread; `transcribe` blocks until done, cancelled, or
/// failed.
pub trait TranscriptionProvider: Send + Sync {
    /// Registry key, e.g. `"hosted"`, `"whisper-local"`, `"mock"`.
    fn id(&self) -> &str;

    fn transcribe(
        &self,
        req: TranscriptionRequest,
        progress: ProgressSink,
        cancel: CancelToken,
    ) -> Result<TranscriptionResult, ProviderError>;
}

#[derive(Clone, Debug)]
pub struct TranscriptionRequest {
    /// 48 kHz mono WAV, sidecar cache dir (06 §3.2, [`crate::captions::extract`]).
    pub audio_path: PathBuf,
    pub language_hint: Option<String>,
    /// Provider-specific model id, optional.
    pub model: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TranscriptionResult {
    pub words: Vec<TranscribedWord>,
    pub language: Option<String>,
    /// `true` if the adapter approximated word timing from a cue-level
    /// source (06 §2.2) rather than receiving real word timestamps.
    pub degraded: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TranscribedWord {
    pub text: String,
    /// Sequence-relative after 06 §3.4's offset mapping (an adapter itself
    /// returns audio-relative ticks; the offset mapping is the engine's job,
    /// out of this story's scope).
    pub start: Tick,
    pub end: Tick,
    pub confidence: Option<f32>,
}

/// A blocking TTS provider (06 §2.1).
pub trait TtsProvider: Send + Sync {
    fn id(&self) -> &str;

    /// Must enumerate usable voices — the UI never hardcodes a voice list
    /// (06 §2.2).
    fn voices(&self) -> Result<Vec<VoiceDescriptor>, ProviderError>;

    fn synthesize(
        &self,
        req: TtsRequest,
        progress: ProgressSink,
        cancel: CancelToken,
    ) -> Result<TtsResult, ProviderError>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct VoiceDescriptor {
    pub id: String,
    pub name: String,
    pub params: Vec<ParamSpec>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParamSpec {
    pub key: String,
    pub label: String,
    pub kind: ParamKind,
    pub range: Option<(f32, f32)>,
    pub default: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParamKind {
    Float,
    Enum(Vec<String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TtsRequest {
    pub text: String,
    pub voice: String,
    pub params: HashMap<String, f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TtsResult {
    /// PCM/WAV bytes.
    pub audio: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u16,
    /// `Some` if the provider aligns words to its own generated audio
    /// (06 §2.2: optional; absent means no auto-captions from this clip).
    pub word_timings: Option<Vec<TranscribedWord>>,
}

/// Default per-request timeout budget (06 §2.2): `2 × source audio duration
/// + 30s`. Adapter concern — configurable per provider on top of this.
pub fn default_timeout(source_audio_duration: Duration) -> Duration {
    source_audio_duration.saturating_mul(2) + Duration::from_secs(30)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_token_shares_state_across_clones() {
        let token = CancelToken::new();
        let clone = token.clone();
        assert!(!token.is_cancelled());
        assert!(!clone.is_cancelled());
        clone.cancel();
        assert!(token.is_cancelled());
        assert!(clone.is_cancelled());
    }

    #[test]
    fn default_timeout_matches_budget_formula() {
        assert_eq!(
            default_timeout(Duration::from_secs(10)),
            Duration::from_secs(50)
        );
        assert_eq!(default_timeout(Duration::ZERO), Duration::from_secs(30));
    }

    #[test]
    fn provider_error_messages_are_non_empty() {
        let errs = [
            ProviderError::Unavailable,
            ProviderError::Unauthorized,
            ProviderError::RateLimited,
            ProviderError::InvalidRequest("bad field".into()),
            ProviderError::Timeout,
            ProviderError::Cancelled,
            ProviderError::Other("boom".into()),
        ];
        for e in errs {
            assert!(!e.to_string().is_empty());
        }
    }
}
