//! E-2 / 32 §2 — analysis-as-node foundation.
//!
//! Analysis nodes emit **typed metadata**, not pixels. Results are cached by
//! content hash so re-analysis after an undo is free. Consumers (scopes,
//! loudness-on-export, scene detect, …) read the cached result rather than
//! re-running the analysis on every frame.
//!
//! Pull-based contract (26 E-2): given a timeline position and an input frame,
//! synchronously return the analysis — stateless, order-independent, seek-correct.
//!
//! Both halves of E-2 live here: the image analyses (32 §2) and the audio ones
//! (31 §5), which share the cache and key scheme. The first shipped consumer is
//! loudness-on-export — [`analyze_loudness`] behind [`loudness_key`], read by
//! `export::offline_audio`.

use std::collections::HashMap;

use crate::audio::dsp::loudness::{integrated_lufs, true_peak_dbtp};
use crate::audio::CHANNELS;
use crate::graph::ir::ContentHash;
use crate::graph::ops::Image;

/// Typed analysis payload (32 §2). Extend with Motion/SceneCuts/Transform as
/// consumers land — never a string property bag.
#[derive(Clone, Debug, PartialEq)]
pub enum AnalysisResult {
    /// 256-bin Rec.709 luma histogram (counts sum to sampled pixels).
    Histogram { bins: [u32; 256], samples: u32 },
    /// Per-channel mean / peak (linear premultiplied space).
    Levels {
        mean: [f32; 4],
        peak: [f32; 4],
        samples: u32,
    },
    /// EBU R128 / BS.1770-4 measurement of one audio range — the audio half of
    /// E-2 (31 §5), consumed by loudness-on-export (31 §6.2).
    ///
    /// Widths mirror [`integrated_lufs`] (`f64`) and [`true_peak_dbtp`] (`f32`)
    /// exactly, so routing a measurement through the cache cannot perturb the
    /// gain arithmetic downstream by so much as a ULP. `integrated_lufs` is
    /// [`f64::NEG_INFINITY`] for silence / sub-block ranges, as the measurement
    /// itself is — the cache stores that verdict rather than hiding it.
    ///
    /// Loudness *range* (LRA) is deliberately absent: `dsp::loudness` does not
    /// measure it, and a field nothing computes is worse than no field.
    Loudness {
        integrated_lufs: f64,
        true_peak_dbtp: f32,
        /// Sample frames measured (`pcm.len() / CHANNELS`).
        frames: u32,
        sample_rate: u32,
    },
    /// Per-frame exposure measurements over a contiguous frame range — the
    /// image analogue of [`AnalysisResult::Loudness`], and like it a **job, not
    /// a node**: one pass measures the whole range, every later frame reads the
    /// cached verdict. This is what lets deflicker correct slow drift without
    /// the evaluator ever fetching a neighbouring frame.
    Exposure {
        curve: crate::graph::deflicker::ExposureCurve,
    },
}

/// Kind tags for [`analysis_key`]. One byte, distinct per [`AnalysisResult`]
/// variant, so two analyses of the same input never share a cache slot.
pub const KIND_HISTOGRAM: u8 = 1;
/// See [`KIND_HISTOGRAM`].
pub const KIND_LEVELS: u8 = 2;
/// See [`KIND_HISTOGRAM`].
pub const KIND_LOUDNESS: u8 = 3;
/// See [`KIND_HISTOGRAM`].
pub const KIND_EXPOSURE: u8 = 4;

/// Context for analysis (tick + optional canvas hint). Reserved for multi-frame
/// windowed analysis.
#[derive(Copy, Clone, Debug, Default)]
pub struct AnalysisCtx {
    pub at: photonic_core::timeline::Tick,
}

/// In-memory analysis cache keyed by content hash of (op identity + input hash).
#[derive(Default)]
pub struct AnalysisCache {
    map: HashMap<u128, AnalysisResult>,
}

impl AnalysisCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: ContentHash) -> Option<&AnalysisResult> {
        self.map.get(&key.0)
    }

    pub fn insert(&mut self, key: ContentHash, value: AnalysisResult) {
        self.map.insert(key.0, value);
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Build a cache key from an analysis kind tag and the input frame content hash.
pub fn analysis_key(kind_tag: u8, input_hash: ContentHash) -> ContentHash {
    ContentHash((kind_tag as u128) << 120 | (input_hash.0 & ((1u128 << 120) - 1)))
}

/// 256-bin Rec.709 luma histogram of `img` (every pixel, premultiplied → approx
/// via max-alpha guard). Pure function of the pixels.
pub fn analyze_histogram(img: &Image) -> AnalysisResult {
    let mut bins = [0u32; 256];
    let mut samples = 0u32;
    for px in &img.pixels {
        let a = px[3].max(1e-4);
        let r = (px[0] / a).clamp(0.0, 1.0);
        let g = (px[1] / a).clamp(0.0, 1.0);
        let b = (px[2] / a).clamp(0.0, 1.0);
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let bin = (y * 255.0).round().clamp(0.0, 255.0) as usize;
        bins[bin] += 1;
        samples += 1;
    }
    AnalysisResult::Histogram { bins, samples }
}

/// Per-channel mean and peak of premultiplied linear pixels.
pub fn analyze_levels(img: &Image) -> AnalysisResult {
    let mut sum = [0.0f64; 4];
    let mut peak = [0.0f32; 4];
    let n = img.pixels.len() as f64;
    for px in &img.pixels {
        for c in 0..4 {
            sum[c] += px[c] as f64;
            peak[c] = peak[c].max(px[c]);
        }
    }
    let samples = img.pixels.len() as u32;
    let mean = if n > 0.0 {
        [
            (sum[0] / n) as f32,
            (sum[1] / n) as f32,
            (sum[2] / n) as f32,
            (sum[3] / n) as f32,
        ]
    } else {
        [0.0; 4]
    };
    AnalysisResult::Levels {
        mean,
        peak,
        samples,
    }
}

// ── Audio analysis (31 §5 — the audio half of E-2) ───────────────────────────

/// Chunk size (in samples) for [`audio_content_hash`]'s little-endian staging
/// buffer — 4 KiB of stack, amortising the `Xxh3::update` call over 1024
/// samples without an allocation.
const AUDIO_HASH_CHUNK: usize = 1024;

/// Content hash of an interleaved PCM range — the audio equivalent of the
/// pixel-node hash a frame carries, so [`analysis_key`] has an honest input
/// identity for audio.
///
/// It hashes **every sample's bit pattern** plus the interleaving parameters
/// (`sample_rate`, `channels`) and the sample count, with xxh3-128. Two ranges
/// therefore key equal only when they are bit-identical audio interpreted the
/// same way — no duration/level/asset-id summary that would collide across
/// different audio, which is the failure this key exists to prevent. `-0.0`
/// and `0.0` (and each NaN payload) have distinct bit patterns and so hash
/// apart: conservative — a redundant re-measure, never a wrong reuse.
///
/// Deterministic across platforms: samples are staged little-endian, matching
/// the crate's other content hashes.
///
/// Cost is one linear pass at memcpy-ish speed, ~2 orders of magnitude cheaper
/// than the K-weighting + 4× oversampled measurement it guards.
pub fn audio_content_hash(pcm: &[f32], sample_rate: u32, channels: u16) -> ContentHash {
    use xxhash_rust::xxh3::Xxh3;
    let mut h = Xxh3::new();
    // Domain tag: keeps this hash space disjoint from the pixel-node one even
    // if a future key scheme drops the kind tag.
    h.update(b"photonic.analysis.audio.v1");
    h.update(&sample_rate.to_le_bytes());
    h.update(&channels.to_le_bytes());
    h.update(&(pcm.len() as u64).to_le_bytes());
    let mut buf = [0u8; AUDIO_HASH_CHUNK * 4];
    for chunk in pcm.chunks(AUDIO_HASH_CHUNK) {
        for (i, s) in chunk.iter().enumerate() {
            buf[i * 4..i * 4 + 4].copy_from_slice(&s.to_bits().to_le_bytes());
        }
        h.update(&buf[..chunk.len() * 4]);
    }
    ContentHash(h.digest128())
}

/// Cache key for a loudness measurement of `pcm` at `sample_rate`
/// ([`KIND_LOUDNESS`] over [`audio_content_hash`]).
pub fn loudness_key(pcm: &[f32], sample_rate: u32) -> ContentHash {
    analysis_key(
        KIND_LOUDNESS,
        audio_content_hash(pcm, sample_rate, CHANNELS as u16),
    )
}

/// EBU R128 / BS.1770-4 measurement of an interleaved-stereo range — the pure,
/// pull-based analysis behind [`AnalysisResult::Loudness`].
///
/// This is exactly [`integrated_lufs`] + [`true_peak_dbtp`] on the same buffer,
/// boxed into the typed result; it does not re-tune either. Whole-range
/// measurement makes it a **job, not a node** (32 §2's two-pass rule): callers
/// run it once off the realtime path and read the cached verdict thereafter.
pub fn analyze_loudness(pcm: &[f32], sample_rate: u32) -> AnalysisResult {
    AnalysisResult::Loudness {
        integrated_lufs: integrated_lufs(pcm, sample_rate),
        true_peak_dbtp: true_peak_dbtp(pcm, sample_rate),
        frames: (pcm.len() / CHANNELS) as u32,
        sample_rate,
    }
}

/// Cached loudness: returns the prior measurement when `key` hits, so a
/// re-render of unchanged audio never pays the measurement pass again
/// (31 §6.2 step 4). Mirrors [`histogram_cached`].
pub fn loudness_cached(
    cache: &mut AnalysisCache,
    key: ContentHash,
    pcm: &[f32],
    sample_rate: u32,
) -> AnalysisResult {
    if let Some(hit) = cache.get(key) {
        return hit.clone();
    }
    let result = analyze_loudness(pcm, sample_rate);
    cache.insert(key, result.clone());
    result
}

/// Measure an exposure curve over `frames`, in frame order — the deflicker
/// analysis pass (see [`crate::graph::deflicker`]).
///
/// Whole-range like [`analyze_loudness`], so callers run it once off the
/// realtime path. Order matters here in a way it does not for the other image
/// analyses: the result is a *sequence*, and the caller owns that ordering.
pub fn analyze_exposure<'a>(frames: impl IntoIterator<Item = &'a Image>) -> AnalysisResult {
    AnalysisResult::Exposure {
        curve: crate::graph::deflicker::ExposureCurve {
            samples: frames
                .into_iter()
                .map(crate::graph::deflicker::measure_frame)
                .collect(),
        },
    }
}

/// Cached exposure curve: returns the prior measurement when `key` hits, so
/// re-rendering an unchanged range never pays the measurement pass again.
/// Mirrors [`loudness_cached`].
pub fn exposure_cached<'a>(
    cache: &mut AnalysisCache,
    key: ContentHash,
    frames: impl IntoIterator<Item = &'a Image>,
) -> AnalysisResult {
    if let Some(hit) = cache.get(key) {
        return hit.clone();
    }
    let result = analyze_exposure(frames);
    cache.insert(key, result.clone());
    result
}

/// Cached histogram: returns prior result when `key` hits.
pub fn histogram_cached(
    cache: &mut AnalysisCache,
    key: ContentHash,
    img: &Image,
) -> AnalysisResult {
    if let Some(hit) = cache.get(key) {
        return hit.clone();
    }
    let result = analyze_histogram(img);
    cache.insert(key, result.clone());
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::ir::LinearColor;
    use crate::graph::ops::Image;

    #[test]
    fn histogram_of_uniform_gray_peaks_one_bin() {
        let img = Image::filled(
            4,
            4,
            LinearColor {
                r: 0.5,
                g: 0.5,
                b: 0.5,
                a: 1.0,
            },
        );
        let AnalysisResult::Histogram { bins, samples } = analyze_histogram(&img) else {
            panic!("expected histogram");
        };
        assert_eq!(samples, 16);
        let non_zero: Vec<_> = bins.iter().enumerate().filter(|(_, c)| **c > 0).collect();
        assert_eq!(non_zero.len(), 1);
        assert_eq!(*non_zero[0].1, 16);
    }

    #[test]
    fn cache_hits_on_same_key() {
        let mut cache = AnalysisCache::new();
        let img = Image::filled(
            2,
            2,
            LinearColor {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        );
        let key = analysis_key(KIND_HISTOGRAM, ContentHash(0xABC));
        let a = histogram_cached(&mut cache, key, &img);
        let b = histogram_cached(&mut cache, key, &img);
        assert_eq!(a, b);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn levels_mean_of_solid() {
        let img = Image::filled(
            2,
            2,
            LinearColor {
                r: 0.25,
                g: 0.5,
                b: 0.75,
                a: 1.0,
            },
        );
        let AnalysisResult::Levels {
            mean,
            peak,
            samples,
        } = analyze_levels(&img)
        else {
            panic!("expected levels");
        };
        assert_eq!(samples, 4);
        assert!((mean[0] - 0.25).abs() < 1e-5);
        assert!((mean[1] - 0.5).abs() < 1e-5);
        assert!((peak[2] - 0.75).abs() < 1e-5);
    }

    // ── Audio half (31 §5) ───────────────────────────────────────────────────

    /// Interleaved stereo sine, the same shape the loudness DSP tests use.
    fn tone(amp: f32, seconds: f64, fs: u32) -> Vec<f32> {
        let frames = (fs as f64 * seconds) as usize;
        let mut pcm = vec![0f32; frames * CHANNELS];
        for f in 0..frames {
            let s = (2.0 * std::f64::consts::PI * 997.0 * f as f64 / fs as f64).sin() as f32 * amp;
            for c in 0..CHANNELS {
                pcm[f * CHANNELS + c] = s;
            }
        }
        pcm
    }

    #[test]
    fn analyze_loudness_equals_the_direct_dsp_calls() {
        let fs = 48_000u32;
        let pcm = tone(0.1, 1.0, fs);
        let AnalysisResult::Loudness {
            integrated_lufs: lufs,
            true_peak_dbtp: peak,
            frames,
            sample_rate,
        } = analyze_loudness(&pcm, fs)
        else {
            panic!("expected loudness");
        };
        // Bit-exact, not approximate: routing through E-2 is re-plumbing.
        assert_eq!(lufs, integrated_lufs(&pcm, fs));
        assert_eq!(peak, true_peak_dbtp(&pcm, fs));
        assert_eq!(frames, fs);
        assert_eq!(sample_rate, fs);
    }

    #[test]
    fn audio_key_separates_different_audio() {
        let fs = 48_000u32;
        let a = tone(0.1, 0.5, fs);
        let mut b = a.clone();
        assert_eq!(loudness_key(&a, fs), loudness_key(&a, fs), "same buffer");
        // One sample perturbed by a single ULP must land on a different key.
        b[7] = f32::from_bits(b[7].to_bits() ^ 1);
        assert_ne!(loudness_key(&a, fs), loudness_key(&b, fs), "1-ULP change");
        // Same samples, different interpretation → different measurement.
        assert_ne!(loudness_key(&a, fs), loudness_key(&a, 44_100), "rate");
        assert_ne!(
            audio_content_hash(&a, fs, 2),
            audio_content_hash(&a, fs, 1),
            "channels"
        );
        // Length is hashed, so a prefix does not alias the whole.
        assert_ne!(
            loudness_key(&a, fs),
            loudness_key(&a[..a.len() - CHANNELS], fs),
            "truncated"
        );
    }

    #[test]
    fn audio_key_is_distinct_from_the_image_key_space() {
        let pcm = tone(0.1, 0.2, 48_000);
        let hash = audio_content_hash(&pcm, 48_000, CHANNELS as u16);
        assert_ne!(loudness_key(&pcm, 48_000), analysis_key(KIND_LEVELS, hash));
        assert_ne!(
            loudness_key(&pcm, 48_000),
            analysis_key(KIND_HISTOGRAM, hash)
        );
    }

    /// The cache-reuse proof: a poisoned entry can only be returned if the
    /// second call never ran the measurement pass.
    #[test]
    fn loudness_cache_hit_skips_the_measurement_pass() {
        let fs = 48_000u32;
        let pcm = tone(0.1, 1.0, fs);
        let key = loudness_key(&pcm, fs);
        let mut cache = AnalysisCache::new();

        let first = loudness_cached(&mut cache, key, &pcm, fs);
        assert_eq!(first, analyze_loudness(&pcm, fs));
        assert_eq!(cache.len(), 1);

        let poison = AnalysisResult::Loudness {
            integrated_lufs: -99.5,
            true_peak_dbtp: -42.0,
            frames: 1,
            sample_rate: fs,
        };
        cache.insert(key, poison.clone());
        assert_eq!(loudness_cached(&mut cache, key, &pcm, fs), poison);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn silence_measures_as_negative_infinity_and_is_cached_as_such() {
        let fs = 48_000u32;
        let pcm = vec![0f32; fs as usize * CHANNELS];
        let AnalysisResult::Loudness {
            integrated_lufs: lufs,
            ..
        } = analyze_loudness(&pcm, fs)
        else {
            panic!("expected loudness");
        };
        assert!(lufs.is_infinite() && lufs < 0.0, "lufs={lufs}");
        let mut cache = AnalysisCache::new();
        let key = loudness_key(&pcm, fs);
        assert_eq!(
            loudness_cached(&mut cache, key, &pcm, fs),
            loudness_cached(&mut cache, key, &pcm, fs)
        );
    }
}
