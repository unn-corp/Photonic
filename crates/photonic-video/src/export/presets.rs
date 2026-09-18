//! Export preset schema (05-import-export.md §3.1), the built-in catalog
//! (§3.5), the §3.4 alpha allow-list validation, and app-level persistence for
//! user-defined presets (§3.6).

use std::path::{Path, PathBuf};

use photonic_core::timeline::FrameRate;
use serde::{Deserialize, Serialize};

// ── Schema (05 §3.1) ─────────────────────────────────────────────────────────

/// 05 §3.1. Field shapes match the spec's Rust sketch exactly. `Container`
/// carries two variants beyond the sketch's illustrative comment list
/// (`// Mp4, Mov, WebM, Gif, ImageSequence`) because two *other* normative
/// parts of the same doc require them: `Mkv` (§3.5's "Master AV1 High" built-in)
/// and `Apng` (§3.4's alpha allow-list explicitly names APNG as alpha-capable,
/// even though no built-in preset ships one in v1). The comment is descriptive
/// of the common case, not an exhaustive enum declaration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportPreset {
    pub name: String,
    pub container: Container,
    /// `None` for audio-only export.
    pub video: Option<VideoEncodeSpec>,
    pub audio: Option<AudioEncodeSpec>,
    pub resolution: ResolutionSpec,
    pub frame_rate: FrameRatePolicy,
    /// Requires an alpha-capable `container`+`codec` combination — see
    /// [`validate`] / §3.4.
    pub alpha: bool,
    /// MP4/MOV: moov atom at front, web-streamable.
    pub faststart: bool,
    /// LUFS target, per 09-audio-mixer.md's normalization step.
    pub loudness_target: Option<LoudnessTarget>,
    /// K-D4: also write one audio file per sequence audio track (stems).
    /// Additive default false; older presets deserialize as full-mix only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stems: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Container {
    Mp4,
    Mov,
    WebM,
    /// "Master AV1 High" (§3.5) — not in §3.1's comment list, required by the
    /// catalog table.
    Mkv,
    Gif,
    /// A folder of numbered frames, not a single file (§1.2's detection
    /// pattern on re-import).
    ImageSequence,
    /// §3.4's alpha allow-list; no built-in preset uses this in v1.
    Apng,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoEncodeSpec {
    pub codec: VideoCodec,
    pub quality: QualityMode,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VideoCodec {
    H264,
    Av1,
    Vp9,
    /// ProRes 4444 via ffmpeg's `prores_ks` encoder (§3.4).
    ProResLikeMezzanine,
    Gif,
    /// PNG sequence (one file per frame).
    Png,
    /// Animated PNG, single file (§3.4's alpha allow-list).
    Apng,
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum QualityMode {
    Crf(f32),
    Bitrate { target_kbps: u32, max_kbps: u32 },
    Lossless,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioEncodeSpec {
    pub codec: AudioCodec,
    pub bitrate_kbps: Option<u32>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AudioCodec {
    Aac,
    Opus,
    Pcm,
}

/// "`SourceFormat`" = the active `SequenceFormat`'s w/h (01 §4).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ResolutionSpec {
    SourceFormat,
    Explicit { w: u32, h: u32 },
    Scale(f32),
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FrameRatePolicy {
    MatchSequence,
    Explicit(FrameRate),
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoudnessTarget {
    pub integrated_lufs: f32,
    pub true_peak_dbtp: f32,
}

// ── Validation (§3.4's alpha allow-list + structural sanity) ────────────────

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PresetValidationError {
    #[error("preset name must not be empty")]
    EmptyName,
    #[error("preset has neither video nor audio — nothing to encode")]
    NothingToEncode,
    #[error(
        "alpha is enabled but {container:?} + {codec:?} is not in the §3.4 alpha-capable \
         allow-list (WebM+VP9, MOV+ProRes4444, APNG, PNG sequence)"
    )]
    AlphaNotAllowed {
        container: Container,
        codec: Option<VideoCodec>,
    },
    #[error("container {0:?} has no audio slot but the preset sets one")]
    ContainerHasNoAudioSlot(Container),
    #[error("faststart only applies to Mp4/Mov, got {0:?}")]
    FaststartNotApplicable(Container),
    #[error("Scale resolution factor must be > 0, got {0}")]
    NonPositiveScale(f32),
    #[error("Explicit resolution must be non-zero, got {0}x{1}")]
    ZeroExplicitResolution(u32, u32),
}

/// §3.4's alpha allow-list, as a predicate over (container, codec) — the
/// dialog "greys out incompatible combinations rather than allowing an
/// invalid preset to be built"; this is that same rule enforced at the data
/// layer so a hand-authored/imported custom preset (§3.6) can't smuggle an
/// invalid combination past the UI.
fn alpha_combo_allowed(container: Container, codec: Option<VideoCodec>) -> bool {
    matches!(
        (container, codec),
        (Container::WebM, Some(VideoCodec::Vp9))
            | (Container::Mov, Some(VideoCodec::ProResLikeMezzanine))
            | (Container::Apng, Some(VideoCodec::Apng))
            | (Container::ImageSequence, Some(VideoCodec::Png))
    )
}

/// Structural validation (05 §3.4's allow-list plus the sanity checks an
/// export-dialog "Save as preset…" step would want before persisting).
pub fn validate(preset: &ExportPreset) -> Result<(), PresetValidationError> {
    if preset.name.trim().is_empty() {
        return Err(PresetValidationError::EmptyName);
    }
    if preset.video.is_none() && preset.audio.is_none() {
        return Err(PresetValidationError::NothingToEncode);
    }
    if preset.alpha {
        let codec = preset.video.as_ref().map(|v| v.codec);
        if !alpha_combo_allowed(preset.container, codec) {
            return Err(PresetValidationError::AlphaNotAllowed {
                container: preset.container,
                codec,
            });
        }
    }
    if matches!(
        preset.container,
        Container::Gif | Container::ImageSequence | Container::Apng
    ) && preset.audio.is_some()
    {
        return Err(PresetValidationError::ContainerHasNoAudioSlot(
            preset.container,
        ));
    }
    if preset.faststart && !matches!(preset.container, Container::Mp4 | Container::Mov) {
        return Err(PresetValidationError::FaststartNotApplicable(
            preset.container,
        ));
    }
    match preset.resolution {
        // `s.is_nan() || s <= 0.0`, not `!(s > 0.0)` (clippy::neg_cmp_op_on_partial_ord):
        // negating a partial-order comparison silently drops NaN, and NaN is exactly
        // the case this check must still reject as a non-positive scale.
        ResolutionSpec::Scale(s) if s.is_nan() || s <= 0.0 => {
            return Err(PresetValidationError::NonPositiveScale(s));
        }
        ResolutionSpec::Explicit { w, h } if w == 0 || h == 0 => {
            return Err(PresetValidationError::ZeroExplicitResolution(w, h));
        }
        _ => {}
    }
    Ok(())
}

// ── Built-in catalog (§3.5) ──────────────────────────────────────────────────

/// The "Social" family's shared loudness target: -14 LUFS integrated /
/// -1.0 dBTP true peak. Not spelled out per-row in §3.5's catalog table, but
/// §3.1's worked JSON instance is explicitly captioned "the 'Social 9:16'
/// built-in, serialized shape" and gives these exact numbers, and §3.8's
/// loudness dropdown lists "-14 LUFS streaming" as the named preset value
/// social delivery would pick — applied uniformly to all three Social
/// presets (same family, same audio spec, per the table's own "same family"
/// language for 1:1/16:9).
const SOCIAL_LOUDNESS: LoudnessTarget = LoudnessTarget {
    integrated_lufs: -14.0,
    true_peak_dbtp: -1.0,
};

fn social(name: &str) -> ExportPreset {
    ExportPreset {
        name: name.to_string(),
        container: Container::Mp4,
        video: Some(VideoEncodeSpec {
            codec: VideoCodec::H264,
            quality: QualityMode::Crf(20.0),
        }),
        audio: Some(AudioEncodeSpec {
            codec: AudioCodec::Aac,
            bitrate_kbps: Some(128),
        }),
        resolution: ResolutionSpec::SourceFormat,
        frame_rate: FrameRatePolicy::MatchSequence,
        alpha: false,
        faststart: true,
        loudness_target: Some(SOCIAL_LOUDNESS),
        stems: false,
    }
}

fn master_av1_high() -> ExportPreset {
    ExportPreset {
        name: "Master AV1 High".to_string(),
        container: Container::Mkv,
        // "preset speed 4" is an encoder invocation detail (encoder.rs), not
        // part of the schema's QualityMode.
        video: Some(VideoEncodeSpec {
            codec: VideoCodec::Av1,
            quality: QualityMode::Crf(20.0),
        }),
        audio: Some(AudioEncodeSpec {
            codec: AudioCodec::Opus,
            bitrate_kbps: Some(192),
        }),
        resolution: ResolutionSpec::SourceFormat,
        frame_rate: FrameRatePolicy::MatchSequence,
        alpha: false,
        faststart: false, // MKV has no moov atom / faststart concept.
        // Archival/mezzanine master: not loudness-normalized by default.
        loudness_target: None,
        stems: false,
    }
}

fn web_h264() -> ExportPreset {
    ExportPreset {
        name: "Web H.264".to_string(),
        container: Container::Mp4,
        // "target-bitrate ladder (1080p ~6 Mbps)" — v1 ships a single
        // target/max pair (no multi-rendition ladder yet); max is a 1.5x VBV
        // headroom convention over the target, not a spec'd number.
        video: Some(VideoEncodeSpec {
            codec: VideoCodec::H264,
            quality: QualityMode::Bitrate {
                target_kbps: 6000,
                max_kbps: 9000,
            },
        }),
        audio: Some(AudioEncodeSpec {
            codec: AudioCodec::Aac,
            bitrate_kbps: Some(128),
        }),
        resolution: ResolutionSpec::SourceFormat,
        frame_rate: FrameRatePolicy::MatchSequence,
        alpha: false,
        faststart: true,
        loudness_target: None,
        stems: false,
    }
}

fn webm_vp9_alpha() -> ExportPreset {
    ExportPreset {
        name: "WebM VP9 Alpha".to_string(),
        container: Container::WebM,
        video: Some(VideoEncodeSpec {
            codec: VideoCodec::Vp9,
            quality: QualityMode::Crf(24.0),
        }),
        audio: Some(AudioEncodeSpec {
            codec: AudioCodec::Opus,
            bitrate_kbps: Some(128),
        }),
        resolution: ResolutionSpec::SourceFormat,
        frame_rate: FrameRatePolicy::MatchSequence,
        alpha: true,
        faststart: false,
        loudness_target: None,
        stems: false,
    }
}

fn prores_mezzanine() -> ExportPreset {
    ExportPreset {
        name: "ProRes Mezzanine".to_string(),
        container: Container::Mov,
        // ProRes has no CRF-style knob in this catalog; `Lossless` is the
        // sentinel meaning "encoder.rs applies the fixed 4444 profile,
        // no rate control choice exposed."
        video: Some(VideoEncodeSpec {
            codec: VideoCodec::ProResLikeMezzanine,
            quality: QualityMode::Lossless,
        }),
        audio: Some(AudioEncodeSpec {
            codec: AudioCodec::Pcm,
            bitrate_kbps: None,
        }),
        resolution: ResolutionSpec::SourceFormat,
        frame_rate: FrameRatePolicy::MatchSequence,
        alpha: true, // "alpha on by default" per §3.5.
        faststart: false,
        loudness_target: None,
        stems: false,
    }
}

fn gif() -> ExportPreset {
    ExportPreset {
        name: "GIF".to_string(),
        container: Container::Gif,
        video: Some(VideoEncodeSpec {
            codec: VideoCodec::Gif,
            quality: QualityMode::Lossless, // paletted+dithered has no CRF axis.
        }),
        audio: None,
        resolution: ResolutionSpec::SourceFormat,
        // §3.5's "frame-rate capped 15-24fps" is a dialog UI hint (a
        // file-size guardrail), not a structural schema constraint — the
        // preset itself still follows the sequence rate by default.
        frame_rate: FrameRatePolicy::MatchSequence,
        alpha: false, // GIF explicitly excluded from §3.4's alpha allow-list.
        faststart: false,
        loudness_target: None,
        stems: false,
    }
}

fn png_sequence() -> ExportPreset {
    ExportPreset {
        name: "PNG Sequence".to_string(),
        container: Container::ImageSequence,
        video: Some(VideoEncodeSpec {
            codec: VideoCodec::Png,
            quality: QualityMode::Lossless,
        }),
        audio: None,
        resolution: ResolutionSpec::SourceFormat,
        frame_rate: FrameRatePolicy::MatchSequence,
        alpha: true, // "alpha always on" per §3.5.
        faststart: false,
        loudness_target: None,
        stems: false,
    }
}

/// The nine §3.5 built-ins, in the catalog table's order. Built-ins are
/// read-only in the app (§3.6 — "shown with a lock icon, 'Duplicate to
/// edit'"); this function is the single source of truth for their content.
pub fn built_in_presets() -> Vec<ExportPreset> {
    vec![
        social("Social 9:16"),
        social("Social 1:1"),
        social("Social 16:9"),
        master_av1_high(),
        web_h264(),
        webm_vp9_alpha(),
        prores_mezzanine(),
        gif(),
        png_sequence(),
    ]
}

// ── App-level persistence (§3.6) ─────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum PresetStoreError {
    #[error("could not resolve the app config directory")]
    NoConfigDir,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Cross-platform Photonic config directory — the *same* directory family as
/// other app-level prefs (§3.6: "same directory family as other app-level
/// prefs, not the project file"). Reuses `photonic_core::crash_dir` (the
/// crate's single source of truth for this path, already used by
/// `photonic-gui`'s preferences/recent-docs stores) rather than introducing a
/// second directory-resolution implementation or a new `dirs`-crate
/// dependency.
pub fn config_dir() -> Option<PathBuf> {
    photonic_core::crash_dir()
}

/// `~/.config/Photonic/export_presets.json` (Linux/macOS) or the
/// platform-equivalent under [`config_dir`].
pub fn presets_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("export_presets.json"))
}

/// Load user-defined presets from the resolved config path. A missing file
/// (first run, or `save_custom_presets` never called) is not an error — it
/// just means "no custom presets yet."
pub fn load_custom_presets() -> Result<Vec<ExportPreset>, PresetStoreError> {
    let path = presets_path().ok_or(PresetStoreError::NoConfigDir)?;
    load_custom_presets_from(&path)
}

/// Save user-defined presets to the resolved config path, creating parent
/// directories as needed.
pub fn save_custom_presets(presets: &[ExportPreset]) -> Result<(), PresetStoreError> {
    let path = presets_path().ok_or(PresetStoreError::NoConfigDir)?;
    save_custom_presets_to(&path, presets)
}

/// Path-parameterized load, for tests (and any future "import presets from a
/// specific file" UI action) without touching the real user config dir.
pub fn load_custom_presets_from(path: &Path) -> Result<Vec<ExportPreset>, PresetStoreError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

/// Path-parameterized save, for tests (see [`load_custom_presets_from`]).
pub fn save_custom_presets_to(
    path: &Path,
    presets: &[ExportPreset],
) -> Result<(), PresetStoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(presets)?;
    std::fs::write(path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Catalog shape (§3.5's table, row by row) ─────────────────────────────

    #[test]
    fn catalog_has_exactly_nine_built_ins_in_table_order() {
        let names: Vec<String> = built_in_presets().into_iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec![
                "Social 9:16",
                "Social 1:1",
                "Social 16:9",
                "Master AV1 High",
                "Web H.264",
                "WebM VP9 Alpha",
                "ProRes Mezzanine",
                "GIF",
                "PNG Sequence",
            ]
        );
    }

    #[test]
    fn all_built_ins_pass_validation() {
        for p in built_in_presets() {
            assert!(validate(&p).is_ok(), "{}: {:?}", p.name, validate(&p));
        }
    }

    #[test]
    fn social_family_is_mp4_h264_aac_faststart_no_alpha() {
        for name in ["Social 9:16", "Social 1:1", "Social 16:9"] {
            let p = built_in_presets()
                .into_iter()
                .find(|p| p.name == name)
                .unwrap();
            assert_eq!(p.container, Container::Mp4);
            assert_eq!(p.video.as_ref().unwrap().codec, VideoCodec::H264);
            assert_eq!(p.video.as_ref().unwrap().quality, QualityMode::Crf(20.0));
            assert_eq!(p.audio.as_ref().unwrap().codec, AudioCodec::Aac);
            assert_eq!(p.audio.as_ref().unwrap().bitrate_kbps, Some(128));
            assert!(p.faststart);
            assert!(!p.alpha);
            assert_eq!(p.resolution, ResolutionSpec::SourceFormat);
        }
    }

    #[test]
    fn master_av1_high_is_mkv_av1_opus() {
        let p = built_in_presets()
            .into_iter()
            .find(|p| p.name == "Master AV1 High")
            .unwrap();
        assert_eq!(p.container, Container::Mkv);
        assert_eq!(p.video.as_ref().unwrap().codec, VideoCodec::Av1);
        assert_eq!(p.audio.as_ref().unwrap().codec, AudioCodec::Opus);
        assert_eq!(p.audio.as_ref().unwrap().bitrate_kbps, Some(192));
        assert!(!p.alpha);
    }

    #[test]
    fn web_h264_is_bitrate_mode_not_crf() {
        let p = built_in_presets()
            .into_iter()
            .find(|p| p.name == "Web H.264")
            .unwrap();
        assert!(matches!(
            p.video.as_ref().unwrap().quality,
            QualityMode::Bitrate {
                target_kbps: 6000,
                ..
            }
        ));
        assert!(p.faststart);
    }

    #[test]
    fn webm_vp9_alpha_is_alpha_true_in_allow_list() {
        let p = built_in_presets()
            .into_iter()
            .find(|p| p.name == "WebM VP9 Alpha")
            .unwrap();
        assert!(p.alpha);
        assert_eq!(p.container, Container::WebM);
        assert_eq!(p.video.as_ref().unwrap().codec, VideoCodec::Vp9);
        assert!(validate(&p).is_ok());
    }

    #[test]
    fn prores_mezzanine_is_mov_alpha_pcm() {
        let p = built_in_presets()
            .into_iter()
            .find(|p| p.name == "ProRes Mezzanine")
            .unwrap();
        assert_eq!(p.container, Container::Mov);
        assert_eq!(
            p.video.as_ref().unwrap().codec,
            VideoCodec::ProResLikeMezzanine
        );
        assert_eq!(p.audio.as_ref().unwrap().codec, AudioCodec::Pcm);
        assert!(p.alpha);
    }

    #[test]
    fn gif_has_no_audio_and_no_alpha() {
        let p = built_in_presets()
            .into_iter()
            .find(|p| p.name == "GIF")
            .unwrap();
        assert!(p.audio.is_none());
        assert!(!p.alpha);
        assert_eq!(p.container, Container::Gif);
    }

    #[test]
    fn png_sequence_is_image_sequence_alpha_always_on_no_audio() {
        let p = built_in_presets()
            .into_iter()
            .find(|p| p.name == "PNG Sequence")
            .unwrap();
        assert_eq!(p.container, Container::ImageSequence);
        assert!(p.alpha);
        assert!(p.audio.is_none());
        assert_eq!(p.video.as_ref().unwrap().codec, VideoCodec::Png);
    }

    // ── §3.4 alpha allow-list ─────────────────────────────────────────────────

    #[test]
    fn alpha_allow_list_accepts_the_four_documented_combinations() {
        assert!(alpha_combo_allowed(Container::WebM, Some(VideoCodec::Vp9)));
        assert!(alpha_combo_allowed(
            Container::Mov,
            Some(VideoCodec::ProResLikeMezzanine)
        ));
        assert!(alpha_combo_allowed(Container::Apng, Some(VideoCodec::Apng)));
        assert!(alpha_combo_allowed(
            Container::ImageSequence,
            Some(VideoCodec::Png)
        ));
    }

    #[test]
    fn alpha_allow_list_rejects_h264_av1_gif_per_35_note() {
        // "H.264/AV1/GIF: alpha toggle disabled" (§3.4).
        assert!(!alpha_combo_allowed(Container::Mp4, Some(VideoCodec::H264)));
        assert!(!alpha_combo_allowed(Container::Mkv, Some(VideoCodec::Av1)));
        assert!(!alpha_combo_allowed(Container::Gif, Some(VideoCodec::Gif)));
    }

    #[test]
    fn validate_rejects_alpha_on_a_disallowed_combo() {
        let mut p = social("Custom");
        p.alpha = true; // Mp4 + H264 + alpha=true is not in the allow-list.
        assert_eq!(
            validate(&p),
            Err(PresetValidationError::AlphaNotAllowed {
                container: Container::Mp4,
                codec: Some(VideoCodec::H264),
            })
        );
    }

    #[test]
    fn validate_rejects_audio_on_gif_and_image_sequence() {
        let mut p = gif();
        p.audio = Some(AudioEncodeSpec {
            codec: AudioCodec::Aac,
            bitrate_kbps: Some(128),
        });
        assert_eq!(
            validate(&p),
            Err(PresetValidationError::ContainerHasNoAudioSlot(
                Container::Gif
            ))
        );
    }

    #[test]
    fn validate_rejects_faststart_on_non_mp4_mov() {
        let mut p = master_av1_high();
        p.faststart = true;
        assert_eq!(
            validate(&p),
            Err(PresetValidationError::FaststartNotApplicable(
                Container::Mkv
            ))
        );
    }

    #[test]
    fn validate_rejects_empty_name_and_non_positive_scale() {
        let mut p = social("");
        assert_eq!(validate(&p), Err(PresetValidationError::EmptyName));
        p.name = "x".into();
        p.resolution = ResolutionSpec::Scale(0.0);
        assert_eq!(
            validate(&p),
            Err(PresetValidationError::NonPositiveScale(0.0))
        );
    }

    #[test]
    fn validate_rejects_nothing_to_encode() {
        let mut p = social("x");
        p.video = None;
        p.audio = None;
        assert_eq!(validate(&p), Err(PresetValidationError::NothingToEncode));
    }

    // ── Serde round-trip ─────────────────────────────────────────────────────

    #[test]
    fn every_built_in_round_trips_through_json() {
        for p in built_in_presets() {
            let json = serde_json::to_string(&p).unwrap();
            let back: ExportPreset = serde_json::from_str(&json).unwrap();
            assert_eq!(p, back, "{} did not round-trip", p.name);
        }
    }

    /// The exact worked JSON instance from 05 §3.1 (the "Social 9:16" shape).
    #[test]
    fn spec_worked_json_instance_parses_to_expected_shape() {
        let json = r#"
        {
          "name": "Social 9:16",
          "container": "Mp4",
          "video": { "codec": "H264", "quality": { "Crf": 20.0 } },
          "audio": { "codec": "Aac", "bitrate_kbps": 128 },
          "resolution": "SourceFormat",
          "frame_rate": "MatchSequence",
          "alpha": false,
          "faststart": true,
          "loudness_target": { "integrated_lufs": -14.0, "true_peak_dbtp": -1.0 }
        }
        "#;
        let parsed: ExportPreset = serde_json::from_str(json).expect("spec JSON parses");
        assert_eq!(parsed, social("Social 9:16"));
    }

    #[test]
    fn preset_store_round_trips_through_a_temp_path() {
        let dir = std::env::temp_dir().join(format!(
            "photonic-export-preset-test-{}",
            std::process::id()
        ));
        let path = dir.join("export_presets.json");
        let _ = std::fs::remove_dir_all(&dir);

        // Missing file -> empty, not an error.
        assert_eq!(load_custom_presets_from(&path).unwrap(), Vec::new());

        let custom = vec![social("My Custom Preset")];
        save_custom_presets_to(&path, &custom).unwrap();
        let loaded = load_custom_presets_from(&path).unwrap();
        assert_eq!(loaded, custom);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
