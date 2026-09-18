//! Hosted provider adapter (06 §2.4) — targets the user's own hosted
//! transcription/TTS services.
//!
//! 06 §2.4's integration note: the hosted services' exact request/response
//! contract is not pinned yet ("the single sanctioned open item for this
//! doc"; implementation blocks on it, but "the trait/adapter boundary...
//! does not change shape once the contract is pinned; only the adapter's
//! internal HTTP glue does"). This adapter is therefore built against a
//! **configurable** contract instead of a hardcoded one, selected per
//! [`TranscriptionEndpointShape`] / [`TtsEndpointShape`]:
//!
//! - `OpenAiCompatible` — speaks the OpenAI Whisper/TTS wire shape directly,
//!   for a hosted service that already exposes (or proxies) that API.
//! - `GenericJson` — this module's own minimal JSON contract, documented on
//!   each shape's doc comment below. **This is the exact contract the user's
//!   hosted services need to satisfy if they don't speak the OpenAI shape.**
//!
//! Auth is a single configurable header (name + value) — never a trait
//! parameter (06 §2.2: "injected by the app-level provider registry").

use std::collections::HashMap;
use std::io::Read;
use std::time::Duration;

use base64::Engine as _;
use photonic_core::timeline::{Tick, TICKS_PER_SECOND};
use serde::{Deserialize, Serialize};

use super::proportional::distribute_words_proportionally;
use super::provider::{
    default_timeout, CancelToken, ParamKind, ParamSpec, ProgressSink, ProviderError,
    ProviderProgress, TranscribedWord, TranscriptionProvider, TranscriptionRequest,
    TranscriptionResult, TtsProvider, TtsRequest, TtsResult, VoiceDescriptor,
};
use super::wav;

fn seconds_to_tick(secs: f64) -> Tick {
    Tick((secs * TICKS_PER_SECOND as f64).round() as i64)
}

fn ms_to_tick(ms: i64) -> Tick {
    Tick(TICKS_PER_SECOND / 1000 * ms)
}

fn map_ureq_error(err: ureq::Error, cancel: &CancelToken) -> ProviderError {
    if cancel.is_cancelled() {
        return ProviderError::Cancelled;
    }
    match err {
        ureq::Error::Status(401, _) | ureq::Error::Status(403, _) => ProviderError::Unauthorized,
        ureq::Error::Status(429, _) => ProviderError::RateLimited,
        ureq::Error::Status(code @ 400..=499, response) => {
            let body = response.into_string().unwrap_or_default();
            ProviderError::InvalidRequest(format!("HTTP {code}: {body}"))
        }
        ureq::Error::Status(code, _) => ProviderError::Other(format!("HTTP {code}")),
        ureq::Error::Transport(t) => {
            let lower = t.to_string().to_lowercase();
            if lower.contains("timed out") || lower.contains("timeout") {
                ProviderError::Timeout
            } else {
                ProviderError::Unavailable
            }
        }
    }
}

fn apply_auth(mut req: ureq::Request, auth_header: &Option<(String, String)>) -> ureq::Request {
    if let Some((name, value)) = auth_header {
        req = req.set(name, value);
    }
    req
}

// ── Transcription ───────────────────────────────────────────────────────────

pub enum TranscriptionEndpointShape {
    /// `POST {base_url}{path}`, `multipart/form-data`: `file` (the WAV
    /// bytes), `model`, `response_format=verbose_json`,
    /// `timestamp_granularities[]=word`, optional `language`. Response:
    /// OpenAI's verbose_json shape — `words: [{word,start,end}]` (seconds)
    /// when word granularity was honored; falls back to `segments:
    /// [{text,start,end}]` (cue-level, §2.2 degraded path) when the response
    /// has no `words`.
    OpenAiCompatible { path: String },
    /// `POST {base_url}{path}`, `Content-Type: application/json`:
    /// ```json
    /// { "audio_base64": "<wav bytes>", "language_hint": "en", "model": "default" }
    /// ```
    /// Response (200):
    /// ```json
    /// { "words": [ {"text": "hi", "start_ms": 0, "end_ms": 340, "confidence": 0.98} ],
    ///   "language": "en", "degraded": false }
    /// ```
    /// or, for a cue-level-only backend (§2.2 degraded path — `words` wins
    /// if both are present):
    /// ```json
    /// { "segments": [ {"text": "hi there", "start_ms": 0, "end_ms": 900} ], "language": "en" }
    /// ```
    GenericJson { path: String },
}

pub struct HostedTranscriptionConfig {
    pub base_url: String,
    pub auth_header: Option<(String, String)>,
    pub shape: TranscriptionEndpointShape,
    /// Added on top of 06 §2.2's `2 × source duration + 30s` default budget.
    pub extra_timeout: Duration,
}

pub struct HostedTranscriptionProvider {
    config: HostedTranscriptionConfig,
    agent: ureq::Agent,
}

impl HostedTranscriptionProvider {
    pub fn new(config: HostedTranscriptionConfig) -> Self {
        HostedTranscriptionProvider {
            config,
            agent: ureq::AgentBuilder::new().build(),
        }
    }

    fn transcribe_openai(
        &self,
        path: &str,
        audio: &[u8],
        req: &TranscriptionRequest,
        budget: Duration,
        cancel: &CancelToken,
    ) -> Result<TranscriptionResult, ProviderError> {
        const BOUNDARY: &str = "----photonicCaptionsBoundary7c3f9a";
        let mut body = Vec::new();
        push_multipart_file(&mut body, BOUNDARY, "file", "audio.wav", "audio/wav", audio);
        push_multipart_field(
            &mut body,
            BOUNDARY,
            "model",
            req.model.as_deref().unwrap_or("whisper-1"),
        );
        push_multipart_field(&mut body, BOUNDARY, "response_format", "verbose_json");
        push_multipart_field(&mut body, BOUNDARY, "timestamp_granularities[]", "word");
        if let Some(lang) = &req.language_hint {
            push_multipart_field(&mut body, BOUNDARY, "language", lang);
        }
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());

        let url = format!("{}{}", self.config.base_url, path);
        let request = apply_auth(
            self.agent
                .post(&url)
                .set(
                    "Content-Type",
                    &format!("multipart/form-data; boundary={BOUNDARY}"),
                )
                .timeout(budget),
            &self.config.auth_header,
        );
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let response = request
            .send_bytes(&body)
            .map_err(|e| map_ureq_error(e, cancel))?;
        let parsed: OpenAiTranscription = response
            .into_json()
            .map_err(|e| ProviderError::Other(format!("invalid JSON response: {e}")))?;
        Ok(parsed.into_result())
    }

    fn transcribe_generic(
        &self,
        path: &str,
        audio: &[u8],
        req: &TranscriptionRequest,
        budget: Duration,
        cancel: &CancelToken,
    ) -> Result<TranscriptionResult, ProviderError> {
        let body = GenericTranscribeRequest {
            audio_base64: base64::engine::general_purpose::STANDARD.encode(audio),
            language_hint: req.language_hint.as_deref(),
            model: req.model.as_deref(),
        };
        let url = format!("{}{}", self.config.base_url, path);
        let request = apply_auth(
            self.agent.post(&url).timeout(budget),
            &self.config.auth_header,
        );
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let response = request
            .send_json(body)
            .map_err(|e| map_ureq_error(e, cancel))?;
        let parsed: GenericTranscribeResponse = response
            .into_json()
            .map_err(|e| ProviderError::Other(format!("invalid JSON response: {e}")))?;

        if !parsed.words.is_empty() {
            let words = parsed
                .words
                .into_iter()
                .map(|w| TranscribedWord {
                    text: w.text,
                    start: ms_to_tick(w.start_ms),
                    end: ms_to_tick(w.end_ms),
                    confidence: w.confidence,
                })
                .collect();
            Ok(TranscriptionResult {
                words,
                language: parsed.language,
                degraded: parsed.degraded,
            })
        } else {
            let mut words = Vec::new();
            for seg in parsed.segments {
                words.extend(distribute_words_proportionally(
                    &seg.text,
                    ms_to_tick(seg.start_ms),
                    ms_to_tick(seg.end_ms),
                ));
            }
            Ok(TranscriptionResult {
                words,
                language: parsed.language,
                degraded: true,
            })
        }
    }
}

impl TranscriptionProvider for HostedTranscriptionProvider {
    fn id(&self) -> &str {
        "hosted"
    }

    fn transcribe(
        &self,
        req: TranscriptionRequest,
        progress: ProgressSink,
        cancel: CancelToken,
    ) -> Result<TranscriptionResult, ProviderError> {
        let _ = progress.send(ProviderProgress::Started);
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }

        let audio_bytes = std::fs::read(&req.audio_path).map_err(|e| {
            ProviderError::Other(format!("failed to read {:?}: {e}", req.audio_path))
        })?;
        let duration_secs = wav::read_wav_info(&audio_bytes)
            .map(|i| i.duration_secs())
            .unwrap_or(0.0);
        let budget = default_timeout(Duration::from_secs_f64(duration_secs.max(0.0)))
            + self.config.extra_timeout;

        let _ = progress.send(ProviderProgress::Uploading {
            sent: 0,
            total: Some(audio_bytes.len() as u64),
        });
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }

        let result = match &self.config.shape {
            TranscriptionEndpointShape::OpenAiCompatible { path } => {
                self.transcribe_openai(path, &audio_bytes, &req, budget, &cancel)?
            }
            TranscriptionEndpointShape::GenericJson { path } => {
                self.transcribe_generic(path, &audio_bytes, &req, budget, &cancel)?
            }
        };

        let _ = progress.send(ProviderProgress::Done);
        Ok(result)
    }
}

fn push_multipart_field(body: &mut Vec<u8>, boundary: &str, name: &str, value: &str) {
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(value.as_bytes());
    body.extend_from_slice(b"\r\n");
}

fn push_multipart_file(
    body: &mut Vec<u8>,
    boundary: &str,
    name: &str,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
) {
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(b"\r\n");
}

#[derive(Deserialize)]
struct OpenAiTranscription {
    language: Option<String>,
    #[serde(default)]
    words: Vec<OpenAiWord>,
    #[serde(default)]
    segments: Vec<OpenAiSegment>,
}

#[derive(Deserialize)]
struct OpenAiWord {
    word: String,
    start: f64,
    end: f64,
}

#[derive(Deserialize)]
struct OpenAiSegment {
    text: String,
    start: f64,
    end: f64,
}

impl OpenAiTranscription {
    fn into_result(self) -> TranscriptionResult {
        if !self.words.is_empty() {
            let words = self
                .words
                .into_iter()
                .map(|w| TranscribedWord {
                    text: w.word,
                    start: seconds_to_tick(w.start),
                    end: seconds_to_tick(w.end),
                    confidence: None,
                })
                .collect();
            TranscriptionResult {
                words,
                language: self.language,
                degraded: false,
            }
        } else {
            let mut words = Vec::new();
            for seg in self.segments {
                words.extend(distribute_words_proportionally(
                    &seg.text,
                    seconds_to_tick(seg.start),
                    seconds_to_tick(seg.end),
                ));
            }
            TranscriptionResult {
                words,
                language: self.language,
                degraded: true,
            }
        }
    }
}

#[derive(Serialize)]
struct GenericTranscribeRequest<'a> {
    audio_base64: String,
    language_hint: Option<&'a str>,
    model: Option<&'a str>,
}

#[derive(Deserialize)]
struct GenericTranscribeResponse {
    #[serde(default)]
    words: Vec<GenericWord>,
    #[serde(default)]
    segments: Vec<GenericSegment>,
    language: Option<String>,
    #[serde(default)]
    degraded: bool,
}

#[derive(Deserialize)]
struct GenericWord {
    text: String,
    start_ms: i64,
    end_ms: i64,
    #[serde(default)]
    confidence: Option<f32>,
}

#[derive(Deserialize)]
struct GenericSegment {
    text: String,
    start_ms: i64,
    end_ms: i64,
}

// ── TTS ──────────────────────────────────────────────────────────────────

pub enum TtsEndpointShape {
    /// `POST {base_url}{path}`, JSON `{ "input": text, "voice": voice,
    /// "response_format": "wav" }` (OpenAI's `/v1/audio/speech` shape).
    /// Response body **is** the raw audio bytes (`Content-Type: audio/wav`),
    /// no JSON envelope, no word timings — matches OpenAI's real API, which
    /// has none (06 §2.2: "If absent, a generated clip gets no
    /// auto-captions").
    OpenAiCompatible { path: String },
    /// `POST {base_url}{path}`, JSON `{ "text": ..., "voice": ..., "params":
    /// {...} }`. Response (200):
    /// ```json
    /// { "audio_base64": "...", "sample_rate": 24000, "channels": 1,
    ///   "word_timings": [ {"text":"hi","start_ms":0,"end_ms":200,"confidence":null} ] }
    /// ```
    /// (`word_timings` optional/nullable.)
    GenericJson { path: String },
}

pub struct HostedTtsConfig {
    pub base_url: String,
    pub auth_header: Option<(String, String)>,
    pub synthesize_shape: TtsEndpointShape,
    /// `GET {base_url}{voices_path}` — shared across both synthesize shapes
    /// (voice discovery is independent of the synthesis wire shape).
    /// Response (200):
    /// ```json
    /// { "voices": [ { "id": "v1", "name": "Voice One",
    ///     "params": [ {"key":"speed","label":"Speed","kind":"float","range":[0.5,2.0],"default":1.0},
    ///                 {"key":"style","label":"Style","kind":{"enum":{"values":["neutral","cheerful"]}},"default":0.0} ] } ] }
    /// ```
    pub voices_path: String,
    pub extra_timeout: Duration,
}

pub struct HostedTtsProvider {
    config: HostedTtsConfig,
    agent: ureq::Agent,
}

impl HostedTtsProvider {
    pub fn new(config: HostedTtsConfig) -> Self {
        HostedTtsProvider {
            config,
            agent: ureq::AgentBuilder::new().build(),
        }
    }

    fn synthesize_openai(
        &self,
        path: &str,
        req: &TtsRequest,
        budget: Duration,
        cancel: &CancelToken,
    ) -> Result<TtsResult, ProviderError> {
        #[derive(Serialize)]
        struct OpenAiTtsRequest<'a> {
            input: &'a str,
            voice: &'a str,
            response_format: &'a str,
        }
        let body = OpenAiTtsRequest {
            input: &req.text,
            voice: &req.voice,
            response_format: "wav",
        };
        let url = format!("{}{}", self.config.base_url, path);
        let request = apply_auth(
            self.agent.post(&url).timeout(budget),
            &self.config.auth_header,
        );
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let response = request
            .send_json(body)
            .map_err(|e| map_ureq_error(e, cancel))?;
        let mut audio = Vec::new();
        response
            .into_reader()
            .read_to_end(&mut audio)
            .map_err(|e| ProviderError::Other(format!("failed to read audio response: {e}")))?;
        let (sample_rate, channels) = wav::read_wav_info(&audio)
            .map(|i| (i.sample_rate, i.channels))
            .unwrap_or((24_000, 1));
        Ok(TtsResult {
            audio,
            sample_rate,
            channels,
            word_timings: None,
        })
    }

    fn synthesize_generic(
        &self,
        path: &str,
        req: &TtsRequest,
        budget: Duration,
        cancel: &CancelToken,
    ) -> Result<TtsResult, ProviderError> {
        #[derive(Serialize)]
        struct GenericTtsRequest<'a> {
            text: &'a str,
            voice: &'a str,
            params: &'a HashMap<String, f32>,
        }
        let body = GenericTtsRequest {
            text: &req.text,
            voice: &req.voice,
            params: &req.params,
        };
        let url = format!("{}{}", self.config.base_url, path);
        let request = apply_auth(
            self.agent.post(&url).timeout(budget),
            &self.config.auth_header,
        );
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let response = request
            .send_json(body)
            .map_err(|e| map_ureq_error(e, cancel))?;
        let parsed: GenericTtsResponse = response
            .into_json()
            .map_err(|e| ProviderError::Other(format!("invalid JSON response: {e}")))?;
        let audio = base64::engine::general_purpose::STANDARD
            .decode(&parsed.audio_base64)
            .map_err(|e| ProviderError::Other(format!("invalid base64 audio: {e}")))?;
        let word_timings = parsed.word_timings.map(|ws| {
            ws.into_iter()
                .map(|w| TranscribedWord {
                    text: w.text,
                    start: ms_to_tick(w.start_ms),
                    end: ms_to_tick(w.end_ms),
                    confidence: w.confidence,
                })
                .collect()
        });
        Ok(TtsResult {
            audio,
            sample_rate: parsed.sample_rate,
            channels: parsed.channels,
            word_timings,
        })
    }
}

impl TtsProvider for HostedTtsProvider {
    fn id(&self) -> &str {
        "hosted"
    }

    fn voices(&self) -> Result<Vec<VoiceDescriptor>, ProviderError> {
        let url = format!("{}{}", self.config.base_url, self.config.voices_path);
        let request = apply_auth(
            self.agent
                .get(&url)
                .timeout(Duration::from_secs(30) + self.config.extra_timeout),
            &self.config.auth_header,
        );
        let cancel = CancelToken::new();
        let response = request.call().map_err(|e| map_ureq_error(e, &cancel))?;
        let parsed: VoicesResponse = response
            .into_json()
            .map_err(|e| ProviderError::Other(format!("invalid JSON response: {e}")))?;
        Ok(parsed.voices.into_iter().map(Into::into).collect())
    }

    fn synthesize(
        &self,
        req: TtsRequest,
        progress: ProgressSink,
        cancel: CancelToken,
    ) -> Result<TtsResult, ProviderError> {
        let _ = progress.send(ProviderProgress::Started);
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        // No source-audio duration to derive 06 §2.2's budget formula from
        // (that formula is transcription-specific); a flat generous default
        // plus the configurable extra budget instead.
        let budget = Duration::from_secs(120) + self.config.extra_timeout;
        let result = match &self.config.synthesize_shape {
            TtsEndpointShape::OpenAiCompatible { path } => {
                self.synthesize_openai(path, &req, budget, &cancel)?
            }
            TtsEndpointShape::GenericJson { path } => {
                self.synthesize_generic(path, &req, budget, &cancel)?
            }
        };
        let _ = progress.send(ProviderProgress::Done);
        Ok(result)
    }
}

#[derive(Deserialize)]
struct GenericTtsResponse {
    audio_base64: String,
    sample_rate: u32,
    channels: u16,
    #[serde(default)]
    word_timings: Option<Vec<GenericWord>>,
}

#[derive(Deserialize)]
struct VoicesResponse {
    voices: Vec<VoiceJson>,
}

#[derive(Deserialize)]
struct VoiceJson {
    id: String,
    name: String,
    #[serde(default)]
    params: Vec<ParamSpecJson>,
}

#[derive(Deserialize)]
struct ParamSpecJson {
    key: String,
    label: String,
    kind: ParamKindJson,
    range: Option<(f32, f32)>,
    default: f32,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ParamKindJson {
    Float,
    Enum { values: Vec<String> },
}

impl From<ParamKindJson> for ParamKind {
    fn from(k: ParamKindJson) -> Self {
        match k {
            ParamKindJson::Float => ParamKind::Float,
            ParamKindJson::Enum { values } => ParamKind::Enum(values),
        }
    }
}

impl From<VoiceJson> for VoiceDescriptor {
    fn from(v: VoiceJson) -> Self {
        VoiceDescriptor {
            id: v.id,
            name: v.name,
            params: v
                .params
                .into_iter()
                .map(|p| ParamSpec {
                    key: p.key,
                    label: p.label,
                    kind: p.kind.into(),
                    range: p.range,
                    default: p.default,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captions::wav::silent_48k_mono_wav;
    use std::io::Write as _;
    use std::sync::mpsc;

    /// A one-shot local HTTP stub: captures the single request it receives
    /// (method, url, headers, body) and responds with a caller-supplied
    /// body/content-type. No live network dependency (06 §8's CI-safety
    /// requirement extends to these adapter tests too).
    struct StubServer {
        base_url: String,
        captured: mpsc::Receiver<CapturedRequest>,
        _handle: std::thread::JoinHandle<()>,
    }

    struct CapturedRequest {
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    fn start_stub(status: u16, content_type: &str, response_body: Vec<u8>) -> StubServer {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind stub server");
        let addr = match server.server_addr() {
            tiny_http::ListenAddr::IP(a) => a,
            #[allow(unreachable_patterns)]
            _ => panic!("expected a TCP listen address"),
        };
        let base_url = format!("http://127.0.0.1:{}", addr.port());
        let (tx, rx) = mpsc::channel();
        let content_type = content_type.to_string();

        let handle = std::thread::spawn(move || {
            if let Ok(mut request) = server.recv() {
                let method = request.method().to_string();
                let url = request.url().to_string();
                let headers: Vec<(String, String)> = request
                    .headers()
                    .iter()
                    .map(|h| {
                        (
                            h.field.as_str().as_str().to_string(),
                            h.value.as_str().to_string(),
                        )
                    })
                    .collect();
                let mut body = Vec::new();
                let _ = request.as_reader().read_to_end(&mut body);

                let response = tiny_http::Response::from_data(response_body)
                    .with_status_code(status)
                    .with_header(
                        tiny_http::Header::from_bytes(
                            &b"Content-Type"[..],
                            content_type.as_bytes(),
                        )
                        .unwrap(),
                    );
                let _ = request.respond(response);
                let _ = tx.send(CapturedRequest {
                    method,
                    url,
                    headers,
                    body,
                });
            }
        });

        StubServer {
            base_url,
            captured: rx,
            _handle: handle,
        }
    }

    impl StubServer {
        fn captured(&self) -> CapturedRequest {
            self.captured
                .recv_timeout(Duration::from_secs(5))
                .expect("stub never received a request")
        }
    }

    fn header<'a>(req: &'a CapturedRequest, name: &str) -> Option<&'a str> {
        req.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn transcription_openai_shape_sends_expected_multipart_request() {
        let response = br#"{"language":"en","text":"hello there","words":[{"word":"hello","start":0.0,"end":0.4},{"word":"there","start":0.4,"end":0.9}]}"#.to_vec();
        let stub = start_stub(200, "application/json", response);

        let provider = HostedTranscriptionProvider::new(HostedTranscriptionConfig {
            base_url: stub.base_url.clone(),
            auth_header: Some(("Authorization".to_string(), "Bearer test-token".to_string())),
            shape: TranscriptionEndpointShape::OpenAiCompatible {
                path: "/v1/audio/transcriptions".to_string(),
            },
            extra_timeout: Duration::ZERO,
        });

        let audio_path = write_temp_wav("openai_shape");
        let (tx, _rx) = crossbeam_channel::unbounded();
        let result = provider
            .transcribe(
                TranscriptionRequest {
                    audio_path,
                    language_hint: Some("en".to_string()),
                    model: None,
                },
                tx,
                CancelToken::new(),
            )
            .expect("transcription should succeed");

        assert_eq!(result.words.len(), 2);
        assert_eq!(result.words[0].text, "hello");
        assert!(!result.degraded);

        let req = stub.captured();
        assert_eq!(req.method, "POST");
        assert_eq!(req.url, "/v1/audio/transcriptions");
        assert_eq!(header(&req, "Authorization"), Some("Bearer test-token"));
        assert!(header(&req, "Content-Type")
            .unwrap()
            .starts_with("multipart/form-data"));
        let body_str = String::from_utf8_lossy(&req.body);
        assert!(body_str.contains("name=\"file\""));
        assert!(body_str.contains("response_format"));
        assert!(body_str.contains("verbose_json"));
        assert!(body_str.contains("timestamp_granularities[]"));
        assert!(body_str.contains("\"language\"") || body_str.contains("name=\"language\""));
    }

    #[test]
    fn transcription_openai_shape_falls_back_to_segments_when_no_words() {
        let response = br#"{"language":"en","text":"hi there","segments":[{"text":"hi there","start":0.0,"end":1.0}]}"#.to_vec();
        let stub = start_stub(200, "application/json", response);
        let provider = HostedTranscriptionProvider::new(HostedTranscriptionConfig {
            base_url: stub.base_url.clone(),
            auth_header: None,
            shape: TranscriptionEndpointShape::OpenAiCompatible {
                path: "/v1/audio/transcriptions".to_string(),
            },
            extra_timeout: Duration::ZERO,
        });
        let (tx, _rx) = crossbeam_channel::unbounded();
        let result = provider
            .transcribe(
                TranscriptionRequest {
                    audio_path: write_temp_wav("openai_degraded"),
                    language_hint: None,
                    model: None,
                },
                tx,
                CancelToken::new(),
            )
            .unwrap();
        assert!(result.degraded);
        assert_eq!(result.words.len(), 2); // "hi" + "there", proportionally split
        let _ = stub.captured();
    }

    #[test]
    fn transcription_generic_json_shape_sends_expected_request() {
        let response = br#"{"words":[{"text":"foo","start_ms":0,"end_ms":300,"confidence":0.9},{"text":"bar","start_ms":300,"end_ms":600}],"language":"en","degraded":false}"#.to_vec();
        let stub = start_stub(200, "application/json", response);
        let provider = HostedTranscriptionProvider::new(HostedTranscriptionConfig {
            base_url: stub.base_url.clone(),
            auth_header: Some(("X-Api-Key".to_string(), "secret".to_string())),
            shape: TranscriptionEndpointShape::GenericJson {
                path: "/transcribe".to_string(),
            },
            extra_timeout: Duration::ZERO,
        });
        let (tx, _rx) = crossbeam_channel::unbounded();
        let result = provider
            .transcribe(
                TranscriptionRequest {
                    audio_path: write_temp_wav("generic_shape"),
                    language_hint: Some("en".to_string()),
                    model: None,
                },
                tx,
                CancelToken::new(),
            )
            .unwrap();

        assert_eq!(result.words.len(), 2);
        assert_eq!(result.words[0].confidence, Some(0.9));
        assert!(!result.degraded);

        let req = stub.captured();
        assert_eq!(req.url, "/transcribe");
        assert_eq!(header(&req, "X-Api-Key"), Some("secret"));
        assert_eq!(header(&req, "Content-Type"), Some("application/json"));
        let parsed: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        assert!(parsed.get("audio_base64").is_some());
        assert_eq!(
            parsed.get("language_hint").and_then(|v| v.as_str()),
            Some("en")
        );
    }

    #[test]
    fn transcription_generic_json_shape_degraded_path_uses_segments() {
        let response = br#"{"segments":[{"text":"a longer segment here","start_ms":0,"end_ms":2000}],"language":null}"#.to_vec();
        let stub = start_stub(200, "application/json", response);
        let provider = HostedTranscriptionProvider::new(HostedTranscriptionConfig {
            base_url: stub.base_url.clone(),
            auth_header: None,
            shape: TranscriptionEndpointShape::GenericJson {
                path: "/transcribe".to_string(),
            },
            extra_timeout: Duration::ZERO,
        });
        let (tx, _rx) = crossbeam_channel::unbounded();
        let result = provider
            .transcribe(
                TranscriptionRequest {
                    audio_path: write_temp_wav("generic_degraded"),
                    language_hint: None,
                    model: None,
                },
                tx,
                CancelToken::new(),
            )
            .unwrap();
        assert!(result.degraded);
        assert_eq!(result.words.len(), 4); // "a longer segment here"
        let _ = stub.captured();
    }

    #[test]
    fn transcription_maps_401_to_unauthorized() {
        let stub = start_stub(
            401,
            "application/json",
            br#"{"error":"bad token"}"#.to_vec(),
        );
        let provider = HostedTranscriptionProvider::new(HostedTranscriptionConfig {
            base_url: stub.base_url.clone(),
            auth_header: None,
            shape: TranscriptionEndpointShape::GenericJson {
                path: "/transcribe".to_string(),
            },
            extra_timeout: Duration::ZERO,
        });
        let (tx, _rx) = crossbeam_channel::unbounded();
        let result = provider.transcribe(
            TranscriptionRequest {
                audio_path: write_temp_wav("unauthorized"),
                language_hint: None,
                model: None,
            },
            tx,
            CancelToken::new(),
        );
        assert!(matches!(result, Err(ProviderError::Unauthorized)));
        let _ = stub.captured();
    }

    #[test]
    fn transcription_maps_429_to_rate_limited() {
        let stub = start_stub(
            429,
            "application/json",
            br#"{"error":"slow down"}"#.to_vec(),
        );
        let provider = HostedTranscriptionProvider::new(HostedTranscriptionConfig {
            base_url: stub.base_url.clone(),
            auth_header: None,
            shape: TranscriptionEndpointShape::GenericJson {
                path: "/transcribe".to_string(),
            },
            extra_timeout: Duration::ZERO,
        });
        let (tx, _rx) = crossbeam_channel::unbounded();
        let result = provider.transcribe(
            TranscriptionRequest {
                audio_path: write_temp_wav("rate_limited"),
                language_hint: None,
                model: None,
            },
            tx,
            CancelToken::new(),
        );
        assert!(matches!(result, Err(ProviderError::RateLimited)));
        let _ = stub.captured();
    }

    #[test]
    fn transcription_precancelled_token_never_sends_a_request() {
        let provider = HostedTranscriptionProvider::new(HostedTranscriptionConfig {
            base_url: "http://127.0.0.1:1".to_string(), // unroutable — proves no attempt was made
            auth_header: None,
            shape: TranscriptionEndpointShape::GenericJson {
                path: "/transcribe".to_string(),
            },
            extra_timeout: Duration::ZERO,
        });
        let cancel = CancelToken::new();
        cancel.cancel();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let result = provider.transcribe(
            TranscriptionRequest {
                audio_path: write_temp_wav("precancelled"),
                language_hint: None,
                model: None,
            },
            tx,
            cancel,
        );
        assert!(matches!(result, Err(ProviderError::Cancelled)));
    }

    #[test]
    fn tts_generic_json_shape_round_trips_audio_and_word_timings() {
        let audio = silent_48k_mono_wav(0.5);
        let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&audio);
        let response = format!(
            r#"{{"audio_base64":"{audio_b64}","sample_rate":48000,"channels":1,"word_timings":[{{"text":"hi","start_ms":0,"end_ms":500,"confidence":null}}]}}"#
        )
        .into_bytes();
        let stub = start_stub(200, "application/json", response);
        let provider = HostedTtsProvider::new(HostedTtsConfig {
            base_url: stub.base_url.clone(),
            auth_header: None,
            synthesize_shape: TtsEndpointShape::GenericJson {
                path: "/tts".to_string(),
            },
            voices_path: "/voices".to_string(),
            extra_timeout: Duration::ZERO,
        });
        let (tx, _rx) = crossbeam_channel::unbounded();
        let result = provider
            .synthesize(
                TtsRequest {
                    text: "hi".to_string(),
                    voice: "v1".to_string(),
                    params: HashMap::new(),
                },
                tx,
                CancelToken::new(),
            )
            .unwrap();
        assert_eq!(result.audio, audio);
        assert_eq!(result.sample_rate, 48_000);
        let timings = result.word_timings.unwrap();
        assert_eq!(timings.len(), 1);
        assert_eq!(timings[0].text, "hi");

        let req = stub.captured();
        assert_eq!(req.url, "/tts");
        let parsed: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(parsed.get("text").and_then(|v| v.as_str()), Some("hi"));
        assert_eq!(parsed.get("voice").and_then(|v| v.as_str()), Some("v1"));
    }

    #[test]
    fn tts_openai_shape_returns_raw_audio_with_no_word_timings() {
        let audio = silent_48k_mono_wav(0.3);
        let stub = start_stub(200, "audio/wav", audio.clone());
        let provider = HostedTtsProvider::new(HostedTtsConfig {
            base_url: stub.base_url.clone(),
            auth_header: None,
            synthesize_shape: TtsEndpointShape::OpenAiCompatible {
                path: "/v1/audio/speech".to_string(),
            },
            voices_path: "/voices".to_string(),
            extra_timeout: Duration::ZERO,
        });
        let (tx, _rx) = crossbeam_channel::unbounded();
        let result = provider
            .synthesize(
                TtsRequest {
                    text: "hi".to_string(),
                    voice: "alloy".to_string(),
                    params: HashMap::new(),
                },
                tx,
                CancelToken::new(),
            )
            .unwrap();
        assert_eq!(result.audio, audio);
        assert!(result.word_timings.is_none());

        let req = stub.captured();
        assert_eq!(req.url, "/v1/audio/speech");
        let parsed: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(parsed.get("voice").and_then(|v| v.as_str()), Some("alloy"));
    }

    #[test]
    fn voices_endpoint_parses_float_and_enum_params() {
        let response = br#"{"voices":[{"id":"v1","name":"Voice One","params":[{"key":"speed","label":"Speed","kind":"float","range":[0.5,2.0],"default":1.0},{"key":"style","label":"Style","kind":{"enum":{"values":["neutral","cheerful"]}},"range":null,"default":0.0}]}]}"#.to_vec();
        let stub = start_stub(200, "application/json", response);
        let provider = HostedTtsProvider::new(HostedTtsConfig {
            base_url: stub.base_url.clone(),
            auth_header: None,
            synthesize_shape: TtsEndpointShape::GenericJson {
                path: "/tts".to_string(),
            },
            voices_path: "/voices".to_string(),
            extra_timeout: Duration::ZERO,
        });
        let voices = provider.voices().unwrap();
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "v1");
        assert_eq!(voices[0].params.len(), 2);
        assert!(matches!(voices[0].params[0].kind, ParamKind::Float));
        assert!(
            matches!(&voices[0].params[1].kind, ParamKind::Enum(v) if v == &vec!["neutral".to_string(), "cheerful".to_string()])
        );
        let _ = stub.captured();
    }

    fn write_temp_wav(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("photonic_hosted_test_{name}.wav"));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&silent_48k_mono_wav(0.2)).unwrap();
        path
    }
}
