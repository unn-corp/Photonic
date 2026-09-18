//! Keyframe (GOP) index and per-frame PTS index (02 §3, 01 §9).
//!
//! [`KeyframeIndex`] is built once at import via `ffprobe -skip_frame nokey
//! -show_frames`, listing the presentation tick of every keyframe. Seeking then
//! costs one GOP decode: [`KeyframeIndex::keyframe_before`] binary-searches for
//! the keyframe at or before a target tick, which decode passes to `ffmpeg -ss`.
//!
//! [`PtsIndex`] lists *every* frame's presentation tick. It is only built for
//! variable-frame-rate sources, where decode cannot derive PTS arithmetically
//! (02 §4 "pts-true"): the rawvideo pipe carries no timestamps, so a VFR source
//! needs the ground-truth table. CFR sources skip it entirely.
//!
//! Both persist as JSON in the sidecar cache dir `<project>.photon.cache/`
//! (01 §9) keyed by the asset's content hash, so they survive project moves and
//! are rebuildable at any time.

use std::path::{Path, PathBuf};
use std::process::Command;

use photonic_core::timeline::{Tick, TICKS_PER_SECOND};
use serde::{Deserialize, Serialize};

use super::ffmpeg_locate::FfmpegTools;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("failed to spawn ffprobe: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("ffprobe exited with {status}: {stderr}")]
    Exit { status: String, stderr: String },
    #[error("ffprobe output was not valid JSON: {0}")]
    Json(#[source] serde_json::Error),
    #[error("cache I/O error: {0}")]
    Io(#[source] std::io::Error),
}

// ── Sidecar cache dir resolution (01 §9) ────────────────────────────────────

/// Suffix appended to the project file path to form its sidecar cache dir.
/// The project file already ends in `.photon`, so a `.cache` suffix yields the
/// spec's `<project>.photon.cache/` form (01 §9).
pub const CACHE_DIR_SUFFIX: &str = ".cache";

/// The sidecar cache dir for `project_path`: append `.cache` to the project
/// file path (01 §9). For `/proj/movie.photon` this is
/// `/proj/movie.photon.cache/`. The directory is not created here — callers
/// create on first write.
pub fn cache_dir_for_project(project_path: &Path) -> PathBuf {
    let mut s = project_path.as_os_str().to_os_string();
    s.push(CACHE_DIR_SUFFIX);
    PathBuf::from(s)
}

/// Path of the keyframe-index cache file for `content_hash` inside `cache_dir`.
pub fn keyframe_cache_path(cache_dir: &Path, content_hash: &str) -> PathBuf {
    cache_dir.join(format!("{content_hash}.keyframes.json"))
}

/// True when a keyframe index for this hash already exists (24 L4 ready).
pub fn keyframe_index_ready(cache_dir: &Path, content_hash: &str) -> bool {
    keyframe_cache_path(cache_dir, content_hash).is_file()
}

/// Path of the pts-index cache file for `content_hash` inside `cache_dir`.
fn pts_cache_path(cache_dir: &Path, content_hash: &str) -> PathBuf {
    cache_dir.join(format!("{content_hash}.pts.json"))
}

// ── Keyframe index ──────────────────────────────────────────────────────────

/// Sorted presentation ticks of every keyframe (GOP boundary) in a source.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct KeyframeIndex {
    /// Ascending keyframe presentation ticks; `keyframes[0]` is normally 0.
    pub keyframes: Vec<Tick>,
}

impl KeyframeIndex {
    /// Build from a media file via `ffprobe -skip_frame nokey` (keyframes only).
    pub fn build(tools: &FfmpegTools, path: &Path) -> Result<Self, IndexError> {
        let ticks = probe_frame_ticks(tools, path, /* keyframes_only = */ true)?;
        Ok(KeyframeIndex { keyframes: ticks })
    }

    /// The keyframe at or before `t`. Falls back to the first keyframe (or tick
    /// 0 for an empty index) so decode always has a valid `-ss` origin.
    pub fn keyframe_before(&self, t: Tick) -> Tick {
        if self.keyframes.is_empty() {
            return Tick::ZERO;
        }
        match self.keyframes.binary_search(&t) {
            Ok(i) => self.keyframes[i],
            // `Err(i)` = insertion point; the keyframe before `t` is `i - 1`.
            Err(0) => self.keyframes[0],
            Err(i) => self.keyframes[i - 1],
        }
    }

    /// Load a cached index for `content_hash`, or `None` if absent/corrupt.
    pub fn load(cache_dir: &Path, content_hash: &str) -> Option<Self> {
        let bytes = std::fs::read(keyframe_cache_path(cache_dir, content_hash)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Persist this index for `content_hash`, creating `cache_dir` if needed.
    pub fn save(&self, cache_dir: &Path, content_hash: &str) -> Result<(), IndexError> {
        std::fs::create_dir_all(cache_dir).map_err(IndexError::Io)?;
        let bytes = serde_json::to_vec(self).map_err(IndexError::Json)?;
        // Temp-and-rename (37 §2.3) so a crash never leaves a truncated index.
        super::atomic_write::write_atomic(&keyframe_cache_path(cache_dir, content_hash), &bytes)
            .map_err(IndexError::Io)
    }

    /// Load from cache, else build + persist (the import-time path).
    pub fn load_or_build(
        tools: &FfmpegTools,
        path: &Path,
        cache_dir: &Path,
        content_hash: &str,
    ) -> Result<Self, IndexError> {
        if let Some(idx) = Self::load(cache_dir, content_hash) {
            return Ok(idx);
        }
        let idx = Self::build(tools, path)?;
        idx.save(cache_dir, content_hash)?;
        Ok(idx)
    }
}

// ── Per-frame PTS index (VFR only) ──────────────────────────────────────────

/// Sorted presentation ticks of *every* frame — decode's pts-true source of
/// truth for VFR playback (02 §4).
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PtsIndex {
    /// Ascending per-frame presentation ticks; `pts[i]` = source frame `i`.
    pub pts: Vec<Tick>,
}

impl PtsIndex {
    /// Build from a media file (all frames).
    pub fn build(tools: &FfmpegTools, path: &Path) -> Result<Self, IndexError> {
        let ticks = probe_frame_ticks(tools, path, /* keyframes_only = */ false)?;
        Ok(PtsIndex { pts: ticks })
    }

    /// Presentation tick of source frame `ordinal`, if in range.
    pub fn pts_at(&self, ordinal: i64) -> Option<Tick> {
        usize::try_from(ordinal)
            .ok()
            .and_then(|i| self.pts.get(i).copied())
    }

    /// Source frame ordinal whose pts equals (or is the last one before) `t`.
    pub fn ordinal_before(&self, t: Tick) -> i64 {
        match self.pts.binary_search(&t) {
            Ok(i) => i as i64,
            Err(0) => 0,
            Err(i) => (i - 1) as i64,
        }
    }

    pub fn load(cache_dir: &Path, content_hash: &str) -> Option<Self> {
        let bytes = std::fs::read(pts_cache_path(cache_dir, content_hash)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn save(&self, cache_dir: &Path, content_hash: &str) -> Result<(), IndexError> {
        std::fs::create_dir_all(cache_dir).map_err(IndexError::Io)?;
        let bytes = serde_json::to_vec(self).map_err(IndexError::Json)?;
        // Temp-and-rename (37 §2.3) so a crash never leaves a truncated index.
        super::atomic_write::write_atomic(&pts_cache_path(cache_dir, content_hash), &bytes)
            .map_err(IndexError::Io)
    }

    pub fn load_or_build(
        tools: &FfmpegTools,
        path: &Path,
        cache_dir: &Path,
        content_hash: &str,
    ) -> Result<Self, IndexError> {
        if let Some(idx) = Self::load(cache_dir, content_hash) {
            return Ok(idx);
        }
        let idx = Self::build(tools, path)?;
        idx.save(cache_dir, content_hash)?;
        Ok(idx)
    }
}

// ── Shared ffprobe frame-timestamp reader ───────────────────────────────────

#[derive(Deserialize)]
struct FramesDoc {
    #[serde(default)]
    frames: Vec<FrameEntry>,
}

#[derive(Deserialize)]
struct FrameEntry {
    // Prefer best_effort (handles containers where pts is unset), fall back to
    // pts_time. Both are seconds as a decimal string.
    best_effort_timestamp_time: Option<String>,
    pts_time: Option<String>,
}

/// Run `ffprobe -show_frames` over the first video stream and return each
/// frame's presentation tick, ascending. With `keyframes_only`, `-skip_frame
/// nokey` makes the decoder emit keyframes only (02 §3).
fn probe_frame_ticks(
    tools: &FfmpegTools,
    path: &Path,
    keyframes_only: bool,
) -> Result<Vec<Tick>, IndexError> {
    let mut cmd = Command::new(&tools.ffprobe);
    cmd.arg("-v").arg("error");
    if keyframes_only {
        cmd.arg("-skip_frame").arg("nokey");
    }
    cmd.args([
        "-select_streams",
        "v:0",
        "-show_frames",
        "-show_entries",
        "frame=best_effort_timestamp_time,pts_time",
        "-print_format",
        "json",
    ])
    .arg(path);

    let output = cmd.output().map_err(IndexError::Spawn)?;
    if !output.status.success() {
        return Err(IndexError::Exit {
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    let doc: FramesDoc = serde_json::from_slice(&output.stdout).map_err(IndexError::Json)?;
    let mut ticks: Vec<Tick> = doc
        .frames
        .iter()
        .filter_map(|f| {
            f.best_effort_timestamp_time
                .as_deref()
                .or(f.pts_time.as_deref())
                .and_then(parse_secs)
        })
        .map(secs_to_tick)
        .collect();
    // ffprobe emits frames in decode order; keyframe/pts indices are keyed by
    // presentation time, so sort ascending.
    ticks.sort_unstable();
    Ok(ticks)
}

fn parse_secs(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() || s == "N/A" {
        return None;
    }
    s.parse::<f64>().ok()
}

fn secs_to_tick(secs: f64) -> Tick {
    Tick((secs * TICKS_PER_SECOND as f64).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_dir_appends_suffix() {
        let d = cache_dir_for_project(Path::new("/proj/movie.photon"));
        assert_eq!(d, PathBuf::from("/proj/movie.photon.cache"));
    }

    #[test]
    fn keyframe_before_binary_search() {
        // counter.mp4's documented keyframes: 0, 2s, 4s, 6s, 8s.
        let s = TICKS_PER_SECOND;
        let idx = KeyframeIndex {
            keyframes: vec![Tick(0), Tick(2 * s), Tick(4 * s), Tick(6 * s), Tick(8 * s)],
        };
        // Exactly on a keyframe.
        assert_eq!(idx.keyframe_before(Tick(4 * s)), Tick(4 * s));
        // Mid-GOP: 2.5s → keyframe at 2s.
        assert_eq!(idx.keyframe_before(Tick(2 * s + s / 2)), Tick(2 * s));
        // Before the first keyframe: clamp to 0.
        assert_eq!(idx.keyframe_before(Tick(-5)), Tick(0));
        // Past the last keyframe: the last one.
        assert_eq!(idx.keyframe_before(Tick(100 * s)), Tick(8 * s));
    }

    #[test]
    fn pts_index_ordinal_lookup() {
        let idx = PtsIndex {
            pts: vec![Tick(0), Tick(100), Tick(200), Tick(300)],
        };
        assert_eq!(idx.pts_at(2), Some(Tick(200)));
        assert_eq!(idx.pts_at(9), None);
        assert_eq!(idx.ordinal_before(Tick(250)), 2);
        assert_eq!(idx.ordinal_before(Tick(200)), 2);
        assert_eq!(idx.ordinal_before(Tick(0)), 0);
    }

    #[test]
    fn keyframe_index_roundtrips_through_cache() {
        let dir = std::env::temp_dir().join("photonic_kf_cache_test");
        let _ = std::fs::remove_dir_all(&dir);
        let idx = KeyframeIndex {
            keyframes: vec![Tick(0), Tick(1000), Tick(2000)],
        };
        idx.save(&dir, "deadbeef").unwrap();
        let back = KeyframeIndex::load(&dir, "deadbeef").unwrap();
        assert_eq!(idx, back);
        assert!(KeyframeIndex::load(&dir, "missing").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
