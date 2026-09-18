//! Deterministic, fixture-driven mock providers (06 §7, §8) — the only
//! providers CI-run tests are allowed to depend on. No network, no
//! nondeterminism; real-hosted-provider tests are a separate, network-gated
//! suite (06 §8 risk table) not built here.

use super::proportional::distribute_words_proportionally;
use super::provider::{
    CancelToken, ParamKind, ParamSpec, ProgressSink, ProviderError, ProviderProgress,
    TranscribedWord, TranscriptionProvider, TranscriptionRequest, TranscriptionResult, TtsProvider,
    TtsRequest, TtsResult, VoiceDescriptor,
};
use super::wav::silent_48k_mono_wav;

/// Returns a preset, deterministic transcript regardless of the audio file's
/// actual contents — the point of a mock is determinism, not recognition.
/// Fixture-driven: construct with [`MockTranscriptionProvider::fixture`] or
/// hand it words directly with [`MockTranscriptionProvider::new`].
#[derive(Clone, Debug)]
pub struct MockTranscriptionProvider {
    id: String,
    words: Vec<TranscribedWord>,
    language: Option<String>,
    degraded: bool,
}

impl MockTranscriptionProvider {
    pub fn new(words: Vec<TranscribedWord>) -> Self {
        MockTranscriptionProvider {
            id: "mock".to_string(),
            words,
            language: Some("en".to_string()),
            degraded: false,
        }
    }

    /// Build from a hand-labeled transcript fixture: plain text plus the
    /// audio span it covers, distributed proportionally by character count
    /// (the same fallback math as a degraded cue-only provider, §2.2) —
    /// convenient for tests that only have "this text spans this time range"
    /// rather than real per-word timestamps.
    pub fn fixture(
        transcript: &str,
        start: photonic_core::timeline::Tick,
        end: photonic_core::timeline::Tick,
    ) -> Self {
        MockTranscriptionProvider {
            id: "mock".to_string(),
            words: distribute_words_proportionally(transcript, start, end),
            language: Some("en".to_string()),
            degraded: false,
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }
}

impl TranscriptionProvider for MockTranscriptionProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn transcribe(
        &self,
        _req: TranscriptionRequest,
        progress: ProgressSink,
        cancel: CancelToken,
    ) -> Result<TranscriptionResult, ProviderError> {
        let _ = progress.send(ProviderProgress::Started);
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let _ = progress.send(ProviderProgress::Processing {
            percent: Some(50.0),
        });
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let result = TranscriptionResult {
            words: self.words.clone(),
            language: self.language.clone(),
            degraded: self.degraded,
        };
        let _ = progress.send(ProviderProgress::Done);
        Ok(result)
    }
}

/// Deterministic TTS mock: synthesizes silence of a length proportional to
/// the input text, with word timings distributed the same way (§2.2's
/// proportional fallback, reused rather than duplicated).
#[derive(Clone, Debug)]
pub struct MockTtsProvider {
    id: String,
    voices: Vec<VoiceDescriptor>,
    /// Milliseconds of synthesized audio per character of input text.
    ms_per_char: f64,
}

impl Default for MockTtsProvider {
    fn default() -> Self {
        MockTtsProvider {
            id: "mock".to_string(),
            voices: vec![VoiceDescriptor {
                id: "mock-voice".to_string(),
                name: "Mock Voice".to_string(),
                params: vec![ParamSpec {
                    key: "speed".to_string(),
                    label: "Speed".to_string(),
                    kind: ParamKind::Float,
                    range: Some((0.5, 2.0)),
                    default: 1.0,
                }],
            }],
            ms_per_char: 60.0,
        }
    }
}

impl MockTtsProvider {
    pub fn new(voices: Vec<VoiceDescriptor>) -> Self {
        MockTtsProvider {
            id: "mock".to_string(),
            voices,
            ms_per_char: 60.0,
        }
    }
}

impl TtsProvider for MockTtsProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn voices(&self) -> Result<Vec<VoiceDescriptor>, ProviderError> {
        Ok(self.voices.clone())
    }

    fn synthesize(
        &self,
        req: TtsRequest,
        progress: ProgressSink,
        cancel: CancelToken,
    ) -> Result<TtsResult, ProviderError> {
        let _ = progress.send(ProviderProgress::Started);
        if !self.voices.iter().any(|v| v.id == req.voice) {
            return Err(ProviderError::InvalidRequest(format!(
                "unknown voice `{}`",
                req.voice
            )));
        }
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let chars = req.text.chars().count().max(1) as f64;
        let duration_secs = (chars * self.ms_per_char / 1000.0).max(0.1);
        let audio = silent_48k_mono_wav(duration_secs);

        let word_timings = Some(distribute_words_proportionally(
            &req.text,
            photonic_core::timeline::Tick::ZERO,
            photonic_core::timeline::Tick::from_seconds(duration_secs.round() as i64)
                .max(photonic_core::timeline::Tick(1)),
        ));

        let _ = progress.send(ProviderProgress::Done);
        Ok(TtsResult {
            audio,
            sample_rate: 48_000,
            channels: 1,
            word_timings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captions::grouping::{group_words_into_cues, GroupingParams};
    use crate::captions::wav::read_wav_info;
    use photonic_core::timeline::Tick;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn mock_transcription_e2e_wav_in_cues_out() {
        // "Fixture audio" — the mock never reads it, but a real audio_path is
        // still part of the request shape, so exercise that too.
        let audio_path = PathBuf::from("/does/not/need/to/exist.wav");
        let provider = MockTranscriptionProvider::fixture(
            "Hello there. This is a test transcript.",
            Tick::ZERO,
            Tick::from_seconds(4),
        );
        let (tx, rx) = crossbeam_channel::unbounded();
        let cancel = CancelToken::new();
        let result = provider
            .transcribe(
                TranscriptionRequest {
                    audio_path,
                    language_hint: None,
                    model: None,
                },
                tx,
                cancel,
            )
            .expect("mock transcription should succeed");

        assert!(!result.words.is_empty());
        assert!(!result.degraded);
        let progress: Vec<_> = rx.try_iter().collect();
        assert!(matches!(progress.first(), Some(ProviderProgress::Started)));
        assert!(matches!(progress.last(), Some(ProviderProgress::Done)));

        let cues = group_words_into_cues(&result.words, &GroupingParams::default());
        assert!(!cues.is_empty());
        // Word text survives the full mock -> grouping round trip.
        let flat_text: String = cues.iter().map(|c| c.text()).collect::<Vec<_>>().join(" ");
        assert!(flat_text.contains("Hello"));
        assert!(flat_text.contains("transcript."));
    }

    #[test]
    fn mock_transcription_respects_cancellation() {
        let provider = MockTranscriptionProvider::new(vec![]);
        let (tx, _rx) = crossbeam_channel::unbounded();
        let cancel = CancelToken::new();
        cancel.cancel();
        let result = provider.transcribe(
            TranscriptionRequest {
                audio_path: PathBuf::from("x.wav"),
                language_hint: None,
                model: None,
            },
            tx,
            cancel,
        );
        assert!(matches!(result, Err(ProviderError::Cancelled)));
    }

    #[test]
    fn mock_tts_never_hardcodes_a_voice_list_shape() {
        let provider = MockTtsProvider::default();
        let voices = provider.voices().unwrap();
        assert!(!voices.is_empty());
        assert!(voices[0].params.iter().any(|p| p.key == "speed"));
    }

    #[test]
    fn mock_tts_produces_valid_wav_and_word_timings() {
        let provider = MockTtsProvider::default();
        let mut params = HashMap::new();
        params.insert("speed".to_string(), 1.0);
        let (tx, _rx) = crossbeam_channel::unbounded();
        let result = provider
            .synthesize(
                TtsRequest {
                    text: "hello world".to_string(),
                    voice: "mock-voice".to_string(),
                    params,
                },
                tx,
                CancelToken::new(),
            )
            .expect("mock synth should succeed");

        let info = read_wav_info(&result.audio).expect("valid wav");
        assert_eq!(info.sample_rate, 48_000);
        assert_eq!(info.channels, 1);
        let timings = result.word_timings.expect("mock always aligns words");
        assert_eq!(timings.len(), 2);
        assert_eq!(timings[0].text, "hello");
        assert_eq!(timings[1].text, "world");
    }

    #[test]
    fn mock_tts_rejects_unknown_voice() {
        let provider = MockTtsProvider::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let result = provider.synthesize(
            TtsRequest {
                text: "hi".to_string(),
                voice: "nonexistent".to_string(),
                params: HashMap::new(),
            },
            tx,
            CancelToken::new(),
        );
        assert!(matches!(result, Err(ProviderError::InvalidRequest(_))));
    }
}
