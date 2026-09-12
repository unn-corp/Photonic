use photonic_core::timeline::{FrameRate, SequenceId, Tick, TICKS_PER_SECOND};
use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::Xxh3;

use super::PreviewError;
use crate::graph::ir::ContentHash;
use crate::session::{PreviewQuality, ProxyMode};

const CACHE_VERSION: &[u8] = b"photonic-timeline-preview-v1";
/// Bounds manifests, planning work, and per-chunk decode history.
pub const MAX_CHUNK_FRAMES: u64 = 240;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PreviewCodec {
    #[default]
    IntraH264,
    IntraProResLike,
    /// VP9's lossless mode with its explicit alpha side-channel. The working
    /// frame still passes through the same 8-bit YUV conversion as export.
    Lossless,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PreviewScale {
    #[default]
    Full,
    Half,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PreviewProfile {
    pub codec: PreviewCodec,
    pub quality: u8,
    pub scale: PreviewScale,
}
impl Default for PreviewProfile {
    fn default() -> Self {
        Self {
            codec: PreviewCodec::IntraH264,
            quality: 18,
            scale: PreviewScale::Full,
        }
    }
}
impl PreviewProfile {
    pub fn extension(self) -> &'static str {
        match self.codec {
            PreviewCodec::IntraH264 => "mp4",
            PreviewCodec::IntraProResLike => "mov",
            PreviewCodec::Lossless => "webm",
        }
    }
    pub fn output_size(self, width: u32, height: u32) -> (u32, u32) {
        match self.scale {
            PreviewScale::Full => (width, height),
            PreviewScale::Half => ((width / 2).max(1), (height / 2).max(1)),
        }
    }
}

/// Exact rendering context. `source_signature` describes resolved dependencies
/// (source/proxy file identity, vector state and external provider inputs), not
/// a monotonic document revision: undo must restore the old key. Unrelated
/// sources must not enter this signature. The session computes it with the same
/// provider/media choices used to compile and render this chunk.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PreviewContext {
    pub sequence: SequenceId,
    pub format_index: usize,
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
    #[serde(with = "quality_serde")]
    pub quality: PreviewQuality,
    #[serde(with = "proxy_serde")]
    pub proxy_mode: ProxyMode,
    /// Actual compile/media choice, including Auto's current decision. Per-
    /// asset fallbacks and file revisions also enter `source_signature`.
    pub use_proxy: bool,
    #[serde(with = "hash_serde")]
    pub source_signature: ContentHash,
    pub profile: PreviewProfile,
}

/// Stable frame-grid boundaries. A chunk has ceil(sequence fps) frames and is
/// aligned from frame zero; NTSC 29.97 therefore uses 30 frames (~1.001 s).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChunkSpan {
    pub start: Tick,
    pub end: Tick,
    pub first_frame: i64,
    pub frame_count: u64,
}
impl ChunkSpan {
    pub fn covering(rate: FrameRate, tick: Tick) -> Result<Self, PreviewError> {
        if tick.0 < 0 {
            return Err(PreviewError::Invalid(
                "preview time must be nonnegative".into(),
            ));
        }
        let frames = chunk_frames(rate)?;
        let first = rate.frame_at(tick).div_euclid(frames as i64) * frames as i64;
        Self::at_frame(rate, first, frames)
    }
    fn at_frame(rate: FrameRate, first_frame: i64, frame_count: u64) -> Result<Self, PreviewError> {
        let end_frame = first_frame
            .checked_add(frame_count as i64)
            .ok_or_else(|| PreviewError::Invalid("preview range overflow".into()))?;
        let start = rate.frame_start(first_frame);
        let end = rate.frame_start(end_frame);
        if end <= start {
            return Err(PreviewError::Invalid("preview frame range is empty".into()));
        }
        Ok(Self {
            start,
            end,
            first_frame,
            frame_count,
        })
    }
    pub fn ticks(self, rate: FrameRate) -> impl ExactSizeIterator<Item = Tick> {
        (0..self.frame_count as usize)
            .map(move |index| rate.frame_start(self.first_frame + index as i64))
    }
    pub fn contains(self, tick: Tick) -> bool {
        self.start <= tick && tick < self.end
    }
}
fn chunk_frames(rate: FrameRate) -> Result<u64, PreviewError> {
    if rate.num == 0 || rate.den == 0 || rate.ticks_per_frame().0 <= 0 {
        return Err(PreviewError::Invalid(
            "preview frame rate must be positive and representable".into(),
        ));
    }
    let frames = u64::from(rate.num).div_ceil(u64::from(rate.den));
    if frames == 0 || frames > MAX_CHUNK_FRAMES {
        return Err(PreviewError::Invalid(format!(
            "preview chunks support at most {MAX_CHUNK_FRAMES} frames per second"
        )));
    }
    Ok(frames)
}

/// Lazy range partitioning keeps even long marked ranges out of large queues.
pub struct ChunkSpans {
    rate: FrameRate,
    next_frame: i64,
    end: Tick,
    frames: u64,
}
impl Iterator for ChunkSpans {
    type Item = ChunkSpan;
    fn next(&mut self) -> Option<Self::Item> {
        let span = ChunkSpan::at_frame(self.rate, self.next_frame, self.frames).ok()?;
        if span.start >= self.end {
            return None;
        }
        self.next_frame = self.next_frame.checked_add(self.frames as i64)?;
        Some(span)
    }
}
pub fn chunks_for_range(rate: FrameRate, range: (Tick, Tick)) -> Result<ChunkSpans, PreviewError> {
    if range.0 .0 < 0 || range.1 <= range.0 {
        return Err(PreviewError::Invalid(
            "preview range must be nonempty and nonnegative".into(),
        ));
    }
    let first = ChunkSpan::covering(rate, range.0)?;
    Ok(ChunkSpans {
        rate,
        next_frame: first.first_frame,
        end: range.1,
        frames: first.frame_count,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChunkKey {
    pub sequence: SequenceId,
    pub format_index: usize,
    pub start: Tick,
    #[serde(with = "hash_serde")]
    pub hash: ContentHash,
}

/// The key folds every frame, including its position, and the full context.
/// Per-frame hashes are retained in the tiny manifest to support auditing;
/// validity still compares the complete 128-bit fold, never a sampled tick.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkSignature {
    pub context: PreviewContext,
    pub span: ChunkSpan,
    #[serde(with = "hashes_serde")]
    pub frame_hashes: Vec<ContentHash>,
    pub key: ChunkKey,
}
impl ChunkSignature {
    pub fn new(
        context: PreviewContext,
        span: ChunkSpan,
        frame_hashes: Vec<ContentHash>,
    ) -> Result<Self, PreviewError> {
        if context.width == 0 || context.height == 0 || context.profile.quality > 51 {
            return Err(PreviewError::Invalid(
                "preview dimensions must be positive and quality must be 0..51".into(),
            ));
        }
        let expected = ChunkSpan::covering(context.frame_rate, span.start)?;
        if span != expected || frame_hashes.len() as u64 != span.frame_count {
            return Err(PreviewError::Invalid(
                "preview signature must contain every frame of an aligned chunk".into(),
            ));
        }
        let mut hash = Xxh3::new();
        hash.update(CACHE_VERSION);
        hash.update(context.sequence.0.as_bytes());
        hash.update(&(context.format_index as u64).to_le_bytes());
        hash.update(&context.width.to_le_bytes());
        hash.update(&context.height.to_le_bytes());
        hash.update(&context.frame_rate.num.to_le_bytes());
        hash.update(&context.frame_rate.den.to_le_bytes());
        hash.update(&[
            quality_tag(context.quality),
            proxy_tag(context.proxy_mode),
            u8::from(context.use_proxy),
        ]);
        hash.update(&context.source_signature.0.to_le_bytes());
        hash.update(&[
            match context.profile.codec {
                PreviewCodec::IntraH264 => 0,
                PreviewCodec::IntraProResLike => 1,
                PreviewCodec::Lossless => 2,
            },
            context.profile.quality,
            match context.profile.scale {
                PreviewScale::Full => 0,
                PreviewScale::Half => 1,
            },
        ]);
        hash.update(&span.first_frame.to_le_bytes());
        hash.update(&span.frame_count.to_le_bytes());
        hash.update(&span.start.0.to_le_bytes());
        hash.update(&span.end.0.to_le_bytes());
        hash.update(&TICKS_PER_SECOND.to_le_bytes());
        for (tick, frame) in span.ticks(context.frame_rate).zip(&frame_hashes) {
            hash.update(&tick.0.to_le_bytes());
            hash.update(&frame.0.to_le_bytes());
        }
        let key = ChunkKey {
            sequence: context.sequence,
            format_index: context.format_index,
            start: span.start,
            hash: ContentHash(hash.digest128()),
        };
        Ok(Self {
            context,
            span,
            frame_hashes,
            key,
        })
    }
    /// Reject a tampered, obsolete, or internally inconsistent disk manifest.
    pub fn validate(&self) -> Result<(), PreviewError> {
        let expected = Self::new(self.context.clone(), self.span, self.frame_hashes.clone())?;
        if self.key != expected.key {
            return Err(PreviewError::Invalid(
                "preview key does not match its frame manifest".into(),
            ));
        }
        Ok(())
    }
}

fn quality_tag(quality: PreviewQuality) -> u8 {
    match quality {
        PreviewQuality::Draft => 0,
        PreviewQuality::Full => 1,
    }
}
fn proxy_tag(proxy: ProxyMode) -> u8 {
    match proxy {
        ProxyMode::Auto => 0,
        ProxyMode::ForceOriginal => 1,
        ProxyMode::ForceProxy => 2,
    }
}
mod hash_serde {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        hash: &ContentHash,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{:032x}", hash.0))
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<ContentHash, D::Error> {
        let text = String::deserialize(deserializer)?;
        u128::from_str_radix(&text, 16)
            .map(ContentHash)
            .map_err(serde::de::Error::custom)
    }
}
mod hashes_serde {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        hashes: &[ContentHash],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        hashes
            .iter()
            .map(|hash| format!("{:032x}", hash.0))
            .collect::<Vec<_>>()
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<ContentHash>, D::Error> {
        Vec::<String>::deserialize(deserializer)?
            .into_iter()
            .map(|text| {
                u128::from_str_radix(&text, 16)
                    .map(ContentHash)
                    .map_err(serde::de::Error::custom)
            })
            .collect()
    }
}
mod quality_serde {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        quality: &PreviewQuality,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(quality_tag(*quality))
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<PreviewQuality, D::Error> {
        match u8::deserialize(deserializer)? {
            0 => Ok(PreviewQuality::Draft),
            1 => Ok(PreviewQuality::Full),
            _ => Err(serde::de::Error::custom("invalid preview quality")),
        }
    }
}
mod proxy_serde {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        proxy: &ProxyMode,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(proxy_tag(*proxy))
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<ProxyMode, D::Error> {
        match u8::deserialize(deserializer)? {
            0 => Ok(ProxyMode::Auto),
            1 => Ok(ProxyMode::ForceOriginal),
            2 => Ok(ProxyMode::ForceProxy),
            _ => Err(serde::de::Error::custom("invalid preview proxy mode")),
        }
    }
}
