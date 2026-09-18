//! Import-ladder L3 poster frames (24-preview-media-load §2).
//!
//! A **poster** is a single low-res still extracted at (or near) the first
//! keyframe, written to the sidecar cache as PNG. It exists so the media pool
//! and single monitor can paint *something* within ~1 s of import without
//! waiting on a full decode ring or keyframe index.
//!
//! Posters are never required for correctness — missing poster just falls back
//! to kind glyphs / last EngineFrame / placeholder.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::cache_dir_for_project;
use super::ffmpeg_locate::FfmpegTools;

#[derive(Debug, thiserror::Error)]
pub enum PosterError {
    #[error("failed to spawn ffmpeg: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("ffmpeg exited with {status}: {stderr}")]
    Exit { status: String, stderr: String },
    #[error("poster cache I/O error: {0}")]
    Io(#[source] std::io::Error),
}

/// Directory posters share with proxies / keyframe indices.
pub fn poster_cache_dir(project_path: Option<&Path>) -> PathBuf {
    match project_path {
        Some(p) => cache_dir_for_project(p),
        None => std::env::temp_dir().join("photonic-proxy-cache"),
    }
}

/// `<cache_dir>/<content_hash>.poster.png`
pub fn poster_cache_path(cache_dir: &Path, content_hash: &str) -> PathBuf {
    cache_dir.join(format!("{content_hash}.poster.png"))
}

/// Ensure a poster PNG exists for `input`. Reuses a warm cache file when present.
///
/// Extracts one frame near t=0 (input seek), scales so the long edge is ≤ 320 px,
/// writes PNG. Safe to call from a worker thread.
pub fn ensure_poster(
    tools: &FfmpegTools,
    input: &Path,
    output: &Path,
) -> Result<PathBuf, PosterError> {
    if output.is_file() {
        return Ok(output.to_path_buf());
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(PosterError::Io)?;
    }
    // Write to a temp sibling then rename so a crash mid-write never leaves a
    // truncated poster that subsequent calls would treat as warm.
    let tmp = output.with_extension("poster.tmp.png");
    let status = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", "0", "-i"])
        .arg(input)
        .args([
            "-frames:v",
            "1",
            "-vf",
            "scale='min(320,iw)':-2",
            "-f",
            "image2",
        ])
        .arg(&tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .map_err(PosterError::Spawn)?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(PosterError::Exit {
            status: status.to_string(),
            stderr: String::new(),
        });
    }
    std::fs::rename(&tmp, output).map_err(PosterError::Io)?;
    Ok(output.to_path_buf())
}

/// True when a poster file for this hash already exists (L3 ready).
pub fn poster_ready(cache_dir: &Path, content_hash: &str) -> bool {
    poster_cache_path(cache_dir, content_hash).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poster_cache_path_is_hash_keyed_png() {
        let p = poster_cache_path(Path::new("/cache"), "abc123");
        assert_eq!(p, PathBuf::from("/cache/abc123.poster.png"));
    }

    #[test]
    fn poster_ready_false_for_missing() {
        assert!(!poster_ready(Path::new("/no/such/cache"), "missing-hash"));
    }
}
