//! Audio extraction: source media → 48 kHz mono WAV for a transcription
//! request (06 §3.2).
//!
//! Rendering the *selected* audio (mixdown vs. per-clip, §3.1) is the
//! engine's offline-mix/export-audio path (02 §7, 09) — out of this story's
//! scope. This module owns the one ffmpeg invocation that takes a resolved
//! input (a single media file, or an already-mixed-down intermediate the
//! caller hands it) down to the exact WAV shape [`TranscriptionRequest`]
//! requires, at the sidecar cache path 06 §3.2 specifies.

use std::path::{Path, PathBuf};
use std::process::Command;

use photonic_core::timeline::Tick;

use crate::media::FfmpegTools;

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("failed to create cache directory: {0}")]
    CreateDir(#[source] std::io::Error),
    #[error("failed to spawn ffmpeg: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("ffmpeg exited with {status}: {stderr}")]
    Exit { status: String, stderr: String },
}

/// `<project>.photon.cache/ai/extract/<job_id>.wav` (06 §3.2). `job_id` is an
/// opaque caller-chosen key (this story does not own the engine's `JobId`
/// type, 02 §1) — a job UUID string or a content hash both work.
pub fn extract_cache_path(project_path: &Path, job_id: &str) -> PathBuf {
    crate::media::cache_dir_for_project(project_path)
        .join("ai")
        .join("extract")
        .join(format!("{job_id}.wav"))
}

/// Render `input` (optionally trimmed to `range`, in the input's own local
/// ticks) to 48 kHz mono WAV at `output`, creating parent directories as
/// needed. Blocks; run on a worker thread (06 §3.2 mirrors 02 §3's probing
/// convention: nothing in this module spawns threads itself).
pub fn extract_audio_48k_mono(
    tools: &FfmpegTools,
    input: &Path,
    output: &Path,
    range: Option<(Tick, Tick)>,
) -> Result<(), ExtractError> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(ExtractError::CreateDir)?;
    }

    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"]);
    // `-ss` before `-i` for fast input-side seek (matches decode/sidecar.rs's
    // convention); `-t` (duration) after `-i` trims the end.
    if let Some((start, end)) = range {
        cmd.arg("-ss").arg(format!("{:.6}", start.as_seconds_f64()));
        cmd.arg("-i").arg(input);
        let duration_secs =
            (end.0 - start.0).max(0) as f64 / photonic_core::timeline::TICKS_PER_SECOND as f64;
        cmd.arg("-t").arg(format!("{:.6}", duration_secs));
    } else {
        cmd.arg("-i").arg(input);
    }
    cmd.args(["-vn", "-ac", "1", "-ar", "48000", "-f", "wav"])
        .arg(output);

    let result = cmd.output().map_err(ExtractError::Spawn)?;
    if !result.status.success() {
        return Err(ExtractError::Exit {
            status: result.status.to_string(),
            stderr: String::from_utf8_lossy(&result.stderr).trim().to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captions::wav::read_wav_info;
    use crate::media::ffmpeg_locate::locate_for_test;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    #[test]
    fn cache_path_matches_the_sidecar_convention() {
        let project = Path::new("/projects/movie.photon");
        let path = extract_cache_path(project, "job-123");
        assert_eq!(
            path,
            PathBuf::from("/projects/movie.photon.cache/ai/extract/job-123.wav")
        );
    }

    #[test]
    fn extracts_48k_mono_wav_from_a_fixture() {
        let Some(tools) = locate_for_test() else {
            eprintln!("skip: ffmpeg/ffprobe not found on PATH");
            return;
        };
        let input = fixture("beep_flash.wav");
        if !input.exists() {
            eprintln!("skip: fixture {input:?} not present");
            return;
        }
        let out_dir = std::env::temp_dir().join("photonic_captions_extract_test");
        let output = out_dir.join("extracted.wav");
        let _ = std::fs::remove_file(&output);

        extract_audio_48k_mono(&tools, &input, &output, None).expect("extraction should succeed");

        let bytes = std::fs::read(&output).expect("output wav should exist");
        let info = read_wav_info(&bytes).expect("valid wav header");
        assert_eq!(info.sample_rate, 48_000);
        assert_eq!(info.channels, 1);
        assert!(info.duration_secs() > 0.0);
    }

    #[test]
    fn extracting_a_missing_input_fails_cleanly() {
        let Some(tools) = locate_for_test() else {
            eprintln!("skip: ffmpeg/ffprobe not found on PATH");
            return;
        };
        let out_dir = std::env::temp_dir().join("photonic_captions_extract_test");
        let output = out_dir.join("should_not_exist.wav");
        let result = extract_audio_48k_mono(
            &tools,
            Path::new("/definitely/not/a/real/file.mov"),
            &output,
            None,
        );
        assert!(result.is_err());
    }
}
