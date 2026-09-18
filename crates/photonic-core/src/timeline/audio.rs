//! Audio mixer data model (09 §2, normative).
//!
//! Audio params are ordinary [`PropSet`](super::anim::PropSet) targets addressed
//! by [`PropPath`](super::anim::PropPath) and evaluated by the same
//! [`AnimProps`](super::anim::AnimProps) machinery as clip transforms and
//! effects — no parallel automation system. DSP (EQ/compressor/gate/limiter,
//! loudness) lives in `photonic-video`; this module is pure data + undo plumbing
//! (09 §1: types land P3, DSP/UI additive in P8, never a schema break).

use super::anim::{AnimProps, PropSet};
use super::effect_kind::EffectParams;
use super::ids::TrackId;
use super::prop_registry::PropTargetKind;
use super::time::Tick;
use super::unknown::UnknownTag;
use serde::{Deserialize, Serialize};

/// Per-track audio: fader/pan automation, mute/solo, and a pre-fader fx chain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackAudio {
    pub params: AnimProps<TrackAudioParams>,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    /// Ordered, pre-fader (09 §4). Empty until P8 adds DSP + UI.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fx_chain: Vec<AudioFxUnit>,
}

impl TrackAudio {
    pub fn new() -> Self {
        TrackAudio {
            params: AnimProps::new(TrackAudioParams::default()),
            mute: false,
            solo: false,
            fx_chain: Vec::new(),
        }
    }
}

impl Default for TrackAudio {
    fn default() -> Self {
        TrackAudio::new()
    }
}

/// `volume_db`: `-inf` (mute floor)..`+12`, default 0.0. `pan`: -1.0 (L)..1.0 (R),
/// default 0.0.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackAudioParams {
    pub volume_db: f64,
    pub pan: f64,
}

impl Default for TrackAudioParams {
    fn default() -> Self {
        TrackAudioParams {
            volume_db: 0.0,
            pan: 0.0,
        }
    }
}

impl PropSet for TrackAudioParams {
    const TARGET_KIND: PropTargetKind = PropTargetKind::TrackAudioParams;
}

/// Per-clip audio: gain trim, fades, channel mapping.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipAudio {
    pub params: AnimProps<ClipAudioParams>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_in: Option<AudioFade>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_out: Option<AudioFade>,
    #[serde(default)]
    pub channel_map: ChannelMap,
    /// K-D3: which demuxed audio stream of a multi-stream source to use
    /// (`None` = first / default). Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<u32>,
    /// K-D3: sync offset applied to the *source read* — positive delays audio
    /// relative to picture (camera lag fix). Additive; omitted = zero.
    #[serde(default, skip_serializing_if = "is_zero_tick")]
    pub offset: super::time::Tick,
}

fn is_zero_tick(t: &super::time::Tick) -> bool {
    t.0 == 0
}

impl ClipAudio {
    pub fn new() -> Self {
        ClipAudio {
            params: AnimProps::new(ClipAudioParams::default()),
            fade_in: None,
            fade_out: None,
            channel_map: ChannelMap::AsSource,
            stream: None,
            offset: super::time::Tick::ZERO,
        }
    }
}

impl Default for ClipAudio {
    fn default() -> Self {
        ClipAudio::new()
    }
}

/// `gain_db`: default 0.0 — baseline clip trim, distinct from the track fader.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipAudioParams {
    pub gain_db: f64,
}

impl Default for ClipAudioParams {
    fn default() -> Self {
        ClipAudioParams { gain_db: 0.0 }
    }
}

impl PropSet for ClipAudioParams {
    const TARGET_KIND: PropTargetKind = PropTargetKind::ClipAudioParams;
}

/// A fade envelope at a clip edge.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioFade {
    pub duration: Tick,
    pub shape: FadeShape,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FadeShape {
    Linear,
    /// Constant perceived loudness through crossfade-style fades (default).
    #[default]
    EqualPower,
    Log,
    SCurve,
}

/// Source→bus channel mapping. v1: mono/stereo sources only (surround is a SPEC
/// non-goal); custom per-pair remap deferred. K-D3 adds explicit swap / mono
/// downmix controls for dual-mono and reverse-polarity cameras.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelMap {
    #[default]
    AsSource,
    MonoDownmix,
    StereoLR,
    ChannelSwap,
    /// K-D3: force left channel only (duplicate to both bus channels).
    LeftOnly,
    /// K-D3: force right channel only.
    RightOnly,
}

/// Sequence master bus (09 §4). Seeded with a `Limiter` at creation (§6.5).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MasterBus {
    pub params: AnimProps<MasterBusParams>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fx_chain: Vec<AudioFxUnit>,
    /// Export-time normalization; `None` = raw mix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loudness_target: Option<LoudnessTarget>,
}

impl MasterBus {
    /// A master bus with the default limiter (09 §6.5), used at sequence creation.
    pub fn new() -> Self {
        MasterBus {
            params: AnimProps::new(MasterBusParams::default()),
            fx_chain: vec![AudioFxUnit::new(AudioFxKind::Limiter)],
            loudness_target: None,
        }
    }
}

impl Default for MasterBus {
    fn default() -> Self {
        MasterBus::new()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MasterBusParams {
    pub volume_db: f64,
}

impl Default for MasterBusParams {
    fn default() -> Self {
        MasterBusParams { volume_db: 0.0 }
    }
}

impl PropSet for MasterBusParams {
    const TARGET_KIND: PropTargetKind = PropTargetKind::MasterBusParams;
}

/// Export loudness normalization target. Presets (09 §2): Streaming/Social
/// = -14 LUFS / -1 dBTP; Broadcast = -23 LUFS / -1 dBTP.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoudnessTarget {
    pub integrated_lufs: f32,
    pub true_peak_dbtp: f32,
}

impl LoudnessTarget {
    pub fn streaming() -> Self {
        LoudnessTarget {
            integrated_lufs: -14.0,
            true_peak_dbtp: -1.0,
        }
    }
    pub fn broadcast() -> Self {
        LoudnessTarget {
            integrated_lufs: -23.0,
            true_peak_dbtp: -1.0,
        }
    }
}

/// An audio effect unit — reuses `ClipEffect`'s exact `{kind, enabled, params}`
/// shape (09 §2, 01 §6.3), same registry and animation infra.
///
/// **Deviation from 09 §2 (documented):** the compressor's `sidechain` is an
/// explicit `Option<TrackId>` field here rather than a `params` entry, because
/// 09 §2 states it is "a non-animated ref, not a `PropValue`" — an ordered
/// `PropPath → PropValue` map has no place to hold a `TrackId`. `None` for every
/// non-compressor kind and for a compressor without ducking.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioFxUnit {
    pub kind: AudioFxKind,
    #[serde(default = "super::grade::default_true")]
    pub enabled: bool,
    pub params: AnimProps<EffectParams>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidechain: Option<TrackId>,
}

impl AudioFxUnit {
    /// A unit seeded with its kind's concrete default params (09 §6).
    pub fn new(kind: AudioFxKind) -> Self {
        let mut params = EffectParams::seed(PropTargetKind::AudioFx(kind));
        apply_fx_defaults(kind, &mut params);
        AudioFxUnit {
            kind,
            enabled: true,
            params: AnimProps::new(params),
            sidechain: None,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AudioFxKind {
    Eq,
    Compressor,
    Limiter,
    Gate,
    /// Forward-compat (39 §2.2): a variant this build does not know. The
    /// original serialized tag is preserved verbatim and re-emitted on save.
    /// Declared last so serde tries the known snake_case tags first.
    #[serde(untagged)]
    Unknown(UnknownTag),
}

impl AudioFxKind {
    /// The preserved tag if this is an unknown (forward-compat) variant.
    pub fn unknown_tag(self) -> Option<UnknownTag> {
        match self {
            AudioFxKind::Unknown(t) => Some(t),
            _ => None,
        }
    }

    /// True if this is a forward-compat variant this build does not understand.
    pub fn is_unknown(self) -> bool {
        matches!(self, AudioFxKind::Unknown(_))
    }
}

/// Overwrite a freshly seeded param bag with the concrete per-kind defaults
/// from 09 §6 (EQ band frequencies, compressor/gate/limiter values).
fn apply_fx_defaults(kind: AudioFxKind, params: &mut EffectParams) {
    use super::anim::PropValue::Float;
    let mut set = |p: &str, v: f64| params.set(p, Float(v));
    match kind {
        AudioFxKind::Eq => {
            set("params.low_shelf.freq_hz", 120.0);
            set("params.band1.freq_hz", 500.0);
            set("params.band1.q", 1.0);
            set("params.band2.freq_hz", 2000.0);
            set("params.band2.q", 1.0);
            set("params.band3.freq_hz", 8000.0);
            set("params.band3.q", 1.0);
            set("params.high_shelf.freq_hz", 10000.0);
        }
        AudioFxKind::Compressor => {
            set("params.threshold_db", -24.0);
            set("params.ratio", 4.0);
            set("params.attack_ms", 5.0);
            set("params.release_ms", 250.0);
            set("params.makeup_db", 0.0);
        }
        AudioFxKind::Gate => {
            set("params.threshold_db", -40.0);
            set("params.attack_ms", 1.0);
            set("params.hold_ms", 10.0);
            set("params.release_ms", 100.0);
            set("params.range_db", 60.0);
        }
        AudioFxKind::Limiter => {
            set("params.ceiling_db", -1.0);
            set("params.release_ms", 50.0);
        }
        // Forward-compat (39 §2.2): no known defaults for a variant this build
        // does not understand; the retained tag renders inert.
        AudioFxKind::Unknown(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_fx_kind_unknown_preserves_tag() {
        let k: AudioFxKind = serde_json::from_str("\"exciter\"").unwrap();
        assert!(k.is_unknown());
        assert_eq!(k.unknown_tag().unwrap().as_str(), "exciter");
        assert_eq!(serde_json::to_string(&k).unwrap(), "\"exciter\"");
        for (k, tag) in [
            (AudioFxKind::Eq, "\"eq\""),
            (AudioFxKind::Compressor, "\"compressor\""),
            (AudioFxKind::Limiter, "\"limiter\""),
            (AudioFxKind::Gate, "\"gate\""),
        ] {
            assert_eq!(serde_json::to_string(&k).unwrap(), tag);
            let back: AudioFxKind = serde_json::from_str(tag).unwrap();
            assert_eq!(back, k);
            assert!(!back.is_unknown());
        }
    }

    #[test]
    fn master_bus_seeds_limiter() {
        let mb = MasterBus::new();
        assert_eq!(mb.fx_chain.len(), 1);
        assert_eq!(mb.fx_chain[0].kind, AudioFxKind::Limiter);
        // Default limiter ceiling is -1 dBTP (09 §6.5).
        assert_eq!(
            mb.fx_chain[0].params.base.get("params.ceiling_db"),
            Some(&super::super::anim::PropValue::Float(-1.0))
        );
    }

    #[test]
    fn clip_audio_stream_and_offset_default_and_serde() {
        let a = ClipAudio::new();
        assert!(a.stream.is_none());
        assert_eq!(a.offset, super::super::time::Tick::ZERO);
        let j = serde_json::to_string(&a).unwrap();
        // Additive: zero offset and None stream omitted from JSON.
        assert!(!j.contains("stream"));
        assert!(!j.contains("offset"));
        let mut b = ClipAudio::new();
        b.stream = Some(1);
        b.offset = super::super::time::Tick(1500); // 50 ms at 30k ticks/s? ticks are flicks-scale
        let j2 = serde_json::to_string(&b).unwrap();
        let back: ClipAudio = serde_json::from_str(&j2).unwrap();
        assert_eq!(back.stream, Some(1));
        assert_eq!(back.offset.0, 1500);
        // LeftOnly / RightOnly are first-class channel maps (K-D3).
        assert_ne!(ChannelMap::LeftOnly, ChannelMap::AsSource);
    }

    #[test]
    fn defaults_match_spec() {
        assert_eq!(TrackAudioParams::default().volume_db, 0.0);
        assert_eq!(ChannelMap::default(), ChannelMap::AsSource);
        assert_eq!(FadeShape::default(), FadeShape::EqualPower);
        assert_eq!(LoudnessTarget::streaming().integrated_lufs, -14.0);
    }

    #[test]
    fn audio_types_roundtrip_serde() {
        let ta = TrackAudio {
            params: AnimProps::new(TrackAudioParams {
                volume_db: -3.0,
                pan: 0.5,
            }),
            mute: true,
            solo: false,
            fx_chain: vec![AudioFxUnit::new(AudioFxKind::Compressor)],
        };
        let j = serde_json::to_string(&ta).unwrap();
        let back: TrackAudio = serde_json::from_str(&j).unwrap();
        assert_eq!(ta, back);
    }
}
