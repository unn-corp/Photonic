//! Caption/TTS provider abstraction + subtitle interchange (06 §2-§7).
//!
//! Scope of this module (P5-CAPTIONS-CORE): the provider trait boundary
//! ([`provider`]), the pure words → cues grouping algorithm ([`grouping`]),
//! deterministic mock providers for CI ([`mock`]), the hosted-service
//! adapter ([`hosted`]), audio extraction ([`extract`]) and TTS caching
//! ([`cache`]) helpers, and SRT/VTT/ASS interchange ([`interchange`]).
//! Turning a completed job into a document mutation (`CaptionCmd`/`TtsCmd`,
//! already defined in `photonic_core::timeline::commands`) and the caption
//! timeline-panel UI are later stories (06 §4, §6 UI chrome) — out of scope
//! here.

pub mod cache;
pub mod extract;
pub mod grouping;
pub mod hosted;
pub mod interchange;
pub mod mock;
pub mod proportional;
pub mod provider;
pub mod wav;

pub use grouping::{group_words_into_cues, GroupingParams};
pub use hosted::{
    HostedTranscriptionConfig, HostedTranscriptionProvider, HostedTtsConfig, HostedTtsProvider,
    TranscriptionEndpointShape, TtsEndpointShape,
};
pub use mock::{MockTranscriptionProvider, MockTtsProvider};
pub use provider::{
    CancelToken, ParamKind, ParamSpec, ProgressSink, ProviderError, ProviderProgress,
    TranscribedWord, TranscriptionProvider, TranscriptionRequest, TranscriptionResult, TtsProvider,
    TtsRequest, TtsResult, VoiceDescriptor,
};
