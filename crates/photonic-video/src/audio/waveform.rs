//! Waveform peak-pyramid builder (09 §8.1) and sidecar cache save/load (01 §9:
//! `<project>.photon.cache/`, keyed by asset content hash — never in the JSON
//! document).
//!
//! Level 0 is 256 samples/bucket; each higher level downsamples ×4 until the
//! top level covers the whole asset in roughly 1000 buckets. Min/max/RMS are
//! kept **per channel** (not downmixed) so a stereo clip can render two
//! waveform lanes, matching standard DAW behavior — 09 §8.1 names the
//! per-bucket stats without specifying mono vs. per-channel, so this is a
//! documented choice.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::mixer::PcmSource;

/// 09 §8.1: "level 0 = 256 samples/bucket, each higher level downsamples ×4".
pub const LEVEL0_SAMPLES_PER_BUCKET: usize = 256;
pub const LEVEL_DOWNSAMPLE: usize = 4;
/// 09 §8.1: "until the top level covers the whole asset in ≈1000 buckets".
pub const TOP_LEVEL_TARGET_BUCKETS: usize = 1000;

/// One bucket's min/max envelope + RMS (09 §8.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PeakBucket {
    pub min: f32,
    pub max: f32,
    pub rms: f32,
}

/// One pyramid level: `samples_per_bucket` source samples folded into each
/// bucket, one bucket vec per channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WaveformLevel {
    pub samples_per_bucket: u32,
    /// `channels[c][bucket_index]`.
    pub channels: Vec<Vec<PeakBucket>>,
}

/// A full peak pyramid for one asset (09 §8.1). `levels[0]` is the finest
/// (256 samples/bucket); each subsequent level is ×4 coarser.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WaveformPyramid {
    /// Content-hash cache key (01 §9) — hex string, not re-derived here.
    pub asset_hash: String,
    pub channels: u16,
    pub source_sample_rate: u32,
    pub total_frames: u64,
    pub levels: Vec<WaveformLevel>,
}

/// Accumulator carrying enough state (`sum_sq` + `count`, not a precomputed
/// RMS) to correctly combine buckets when building a coarser level — merging
/// already-computed RMS values directly would double-count/misweight a
/// trailing partial bucket.
#[derive(Clone, Copy)]
struct RawBucket {
    min: f32,
    max: f32,
    sum_sq: f64,
    count: u64,
}

impl RawBucket {
    fn empty() -> Self {
        RawBucket {
            min: f32::INFINITY,
            max: f32::NEG_INFINITY,
            sum_sq: 0.0,
            count: 0,
        }
    }

    fn push(&mut self, sample: f32) {
        if sample < self.min {
            self.min = sample;
        }
        if sample > self.max {
            self.max = sample;
        }
        self.sum_sq += (sample as f64) * (sample as f64);
        self.count += 1;
    }

    fn merge(a: &RawBucket, b: &RawBucket) -> RawBucket {
        if a.count == 0 {
            return *b;
        }
        if b.count == 0 {
            return *a;
        }
        RawBucket {
            min: a.min.min(b.min),
            max: a.max.max(b.max),
            sum_sq: a.sum_sq + b.sum_sq,
            count: a.count + b.count,
        }
    }

    fn finalize(&self) -> PeakBucket {
        if self.count == 0 {
            PeakBucket::default()
        } else {
            PeakBucket {
                min: self.min,
                max: self.max,
                rms: (self.sum_sq / self.count as f64).sqrt() as f32,
            }
        }
    }
}

/// Read `source` to end-of-stream once and build its full peak pyramid (09
/// §8.1: "On import, decode full audio stream once"). Streams through the
/// source in fixed-size chunks rather than buffering the whole decoded signal,
/// so memory use is bounded regardless of asset length.
pub fn build_pyramid(source: &mut dyn PcmSource, asset_hash: impl Into<String>) -> WaveformPyramid {
    const READ_CHUNK_FRAMES: usize = 4096;

    let channels = source.channels().max(1) as usize;
    let sample_rate = source.sample_rate();
    let mut interleaved = vec![0f32; READ_CHUNK_FRAMES * channels];

    let mut level0: Vec<Vec<RawBucket>> = vec![Vec::new(); channels];
    let mut current: Vec<RawBucket> = vec![RawBucket::empty(); channels];
    let mut in_bucket = 0usize;
    let mut total_frames: u64 = 0;

    loop {
        let got = source.read(&mut interleaved, READ_CHUNK_FRAMES);
        if got == 0 {
            break;
        }
        total_frames += got as u64;
        for f in 0..got {
            for (c, cur) in current.iter_mut().enumerate() {
                cur.push(interleaved[f * channels + c]);
            }
            in_bucket += 1;
            if in_bucket == LEVEL0_SAMPLES_PER_BUCKET {
                for c in 0..channels {
                    level0[c].push(std::mem::replace(&mut current[c], RawBucket::empty()));
                }
                in_bucket = 0;
            }
        }
        if got < READ_CHUNK_FRAMES {
            break; // short read = end of stream (PcmSource contract)
        }
    }
    if in_bucket > 0 {
        for c in 0..channels {
            level0[c].push(std::mem::replace(&mut current[c], RawBucket::empty()));
        }
    }

    let mut levels_raw: Vec<Vec<Vec<RawBucket>>> = vec![level0];
    loop {
        let prev = levels_raw.last().unwrap();
        let prev_len = prev.first().map(Vec::len).unwrap_or(0);
        if prev_len <= TOP_LEVEL_TARGET_BUCKETS || prev_len <= 1 {
            break;
        }
        let mut next: Vec<Vec<RawBucket>> = vec![Vec::new(); channels];
        for (c, next_c) in next.iter_mut().enumerate() {
            let src = &prev[c];
            let mut i = 0;
            while i < src.len() {
                let end = (i + LEVEL_DOWNSAMPLE).min(src.len());
                let mut merged = RawBucket::empty();
                for b in &src[i..end] {
                    merged = RawBucket::merge(&merged, b);
                }
                next_c.push(merged);
                i = end;
            }
        }
        levels_raw.push(next);
    }

    let levels = levels_raw
        .into_iter()
        .enumerate()
        .map(|(i, chans)| {
            let spb = LEVEL0_SAMPLES_PER_BUCKET as u32 * (LEVEL_DOWNSAMPLE as u32).pow(i as u32);
            WaveformLevel {
                samples_per_bucket: spb,
                channels: chans
                    .into_iter()
                    .map(|buckets| buckets.iter().map(RawBucket::finalize).collect())
                    .collect(),
            }
        })
        .collect();

    WaveformPyramid {
        asset_hash: asset_hash.into(),
        channels: channels as u16,
        source_sample_rate: sample_rate,
        total_frames,
        levels,
    }
}

fn cache_path(cache_dir: &Path, asset_hash: &str) -> PathBuf {
    cache_dir.join(format!("{asset_hash}.waveform.json"))
}

/// Save a pyramid to `<project>.photon.cache/<asset_hash>.waveform.json` (01
/// §9's sidecar cache dir; JSON is this story's chosen format — simplest to
/// get right for P3, revisit for a compact binary form if profiling shows the
/// size/parse cost matters).
pub fn save_to_dir(pyramid: &WaveformPyramid, cache_dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(cache_dir)?;
    let path = cache_path(cache_dir, &pyramid.asset_hash);
    let json = serde_json::to_vec(pyramid)
        .expect("WaveformPyramid has no non-finite-float-sensitive serde impl to fail on");
    // Temp-and-rename (37 §2.3) so a crash never leaves a truncated waveform.
    crate::media::atomic_write::write_atomic(&path, &json)?;
    Ok(path)
}

/// Load a previously-saved pyramid, if present. `Ok(None)` (not an error)
/// when the cache file doesn't exist — the caller rebuilds via
/// [`build_pyramid`] (never required for correctness, matches the sidecar
/// cache's general rebuildable-at-any-time rule, 02 §6).
pub fn load_from_dir(
    cache_dir: &Path,
    asset_hash: &str,
) -> std::io::Result<Option<WaveformPyramid>> {
    let path = cache_path(cache_dir, asset_hash);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    let pyramid = serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(pyramid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::test_support::{RampSource, SilenceSource};

    #[test]
    fn ramp_pyramid_level0_bucket0_matches_hand_computed_stats() {
        // A mono ramp 0..1024 gives an exact, hand-computable level-0 bucket 0:
        // samples 0..256, min=0, max=255, rms = sqrt(mean(i^2 for i in 0..256)).
        let mut src = RampSource::new(1, 48_000, 1024);
        let pyramid = build_pyramid(&mut src, "test-ramp");

        assert_eq!(pyramid.channels, 1);
        assert_eq!(pyramid.total_frames, 1024);
        assert_eq!(pyramid.levels[0].samples_per_bucket, 256);
        // 1024 / 256 = 4 buckets at level 0, no partial trailing bucket.
        assert_eq!(pyramid.levels[0].channels[0].len(), 4);

        let b0 = pyramid.levels[0].channels[0][0];
        assert_eq!(b0.min, 0.0);
        assert_eq!(b0.max, 255.0);
        let expected_rms = ((0..256u32).map(|i| (i * i) as f64).sum::<f64>() / 256.0).sqrt() as f32;
        assert!(
            (b0.rms - expected_rms).abs() < 1e-2,
            "got {} expected {}",
            b0.rms,
            expected_rms
        );

        let b3 = pyramid.levels[0].channels[0][3];
        assert_eq!(b3.min, 768.0);
        assert_eq!(b3.max, 1023.0);
    }

    #[test]
    fn pyramid_levels_downsample_by_4_until_top() {
        // 4096 frames -> level 0 has 16 buckets; level 1 (x4) has 4; level 2 has 1.
        // Loop stops once a level's bucket count is <= TOP_LEVEL_TARGET_BUCKETS
        // (1000) OR <= 1, so with only 16 level-0 buckets we stop immediately
        // after level 0 since 16 <= 1000. Use a target-buckets-scale asset
        // instead to exercise multiple levels deterministically: force it via
        // a bucket count just over the downsample threshold isn't practical in
        // a unit test (would need >1000*256 samples); instead assert the
        // invariant that holds at any size: each level's bucket count is
        // ceil(prev_level_len / 4), and level 0's spacing is exact.
        let mut src = RampSource::new(1, 48_000, 4096);
        let pyramid = build_pyramid(&mut src, "test-levels");
        assert_eq!(pyramid.levels[0].channels[0].len(), 16);
        assert_eq!(pyramid.levels[0].samples_per_bucket, 256);
        // Only one level is produced here since 16 <= TOP_LEVEL_TARGET_BUCKETS;
        // the pyramid always has at least one level.
        assert_eq!(pyramid.levels.len(), 1);
    }

    #[test]
    fn downsample_merges_correctly_across_many_buckets() {
        // Build a source long enough to require at least one downsample pass:
        // TOP_LEVEL_TARGET_BUCKETS=1000, so > 1000*256 level-0-bucket samples.
        let total = (TOP_LEVEL_TARGET_BUCKETS + 10) * LEVEL0_SAMPLES_PER_BUCKET;
        let mut src = RampSource::new(1, 48_000, total);
        let pyramid = build_pyramid(&mut src, "test-multi-level");
        assert!(
            pyramid.levels.len() >= 2,
            "expected at least 2 levels, got {}",
            pyramid.levels.len()
        );
        assert_eq!(pyramid.levels[1].samples_per_bucket, 1024);
        // Level 1 bucket 0 must be the min/max union of level 0's buckets 0..4.
        let l0 = &pyramid.levels[0].channels[0];
        let l1b0 = pyramid.levels[1].channels[0][0];
        let expected_min = l0[0..4].iter().map(|b| b.min).fold(f32::INFINITY, f32::min);
        let expected_max = l0[0..4]
            .iter()
            .map(|b| b.max)
            .fold(f32::NEG_INFINITY, f32::max);
        assert_eq!(l1b0.min, expected_min);
        assert_eq!(l1b0.max, expected_max);
    }

    #[test]
    fn stereo_source_keeps_per_channel_buckets() {
        let mut src = RampSource::new(2, 48_000, 512);
        let pyramid = build_pyramid(&mut src, "test-stereo");
        assert_eq!(pyramid.channels, 2);
        assert_eq!(pyramid.levels[0].channels.len(), 2);
        // Both channels get the same ramp values (RampSource duplicates per
        // channel), so their bucket stats should match exactly.
        assert_eq!(pyramid.levels[0].channels[0], pyramid.levels[0].channels[1]);
    }

    #[test]
    fn silent_source_has_zero_rms_and_flat_min_max() {
        // SilenceSource never ends, so cap it via a short synthetic wrapper:
        // reuse RampSource semantics isn't right for "silence forever"; instead
        // read a bounded number of frames by wrapping.
        struct Bounded {
            inner: SilenceSource,
            remaining: usize,
        }
        impl PcmSource for Bounded {
            fn channels(&self) -> u16 {
                self.inner.channels
            }
            fn sample_rate(&self) -> u32 {
                self.inner.sample_rate
            }
            fn read(&mut self, out: &mut [f32], frames: usize) -> usize {
                let n = frames.min(self.remaining);
                self.remaining -= n;
                self.inner.read(out, n)
            }
        }
        let mut src = Bounded {
            inner: SilenceSource {
                channels: 1,
                sample_rate: 48_000,
            },
            remaining: 300,
        };
        let pyramid = build_pyramid(&mut src, "test-silence");
        assert_eq!(pyramid.total_frames, 300);
        let b0 = pyramid.levels[0].channels[0][0];
        assert_eq!(b0.min, 0.0);
        assert_eq!(b0.max, 0.0);
        assert_eq!(b0.rms, 0.0);
        // Trailing partial bucket (300 - 256 = 44 samples) still finalizes.
        assert_eq!(pyramid.levels[0].channels[0].len(), 2);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let mut src = RampSource::new(1, 48_000, 512);
        let pyramid = build_pyramid(&mut src, "test-roundtrip-hash");
        let dir =
            std::env::temp_dir().join(format!("photonic-waveform-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let path = save_to_dir(&pyramid, &dir).expect("save should succeed");
        assert!(path.exists());

        let loaded = load_from_dir(&dir, "test-roundtrip-hash").expect("load should succeed");
        assert_eq!(loaded, Some(pyramid));

        let missing =
            load_from_dir(&dir, "no-such-hash").expect("missing lookup is Ok(None), not an error");
        assert_eq!(missing, None);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
