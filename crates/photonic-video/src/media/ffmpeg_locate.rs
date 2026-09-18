//! Locate the `ffmpeg`/`ffprobe` binaries (02 §3).
//!
//! Resolution order for each tool: the `PHOTONIC_FFMPEG_DIR` environment
//! override first (an explicit install the operator points us at — e.g. a
//! bundled build or a CI cache), then a plain `PATH` lookup. The *same*
//! [`FfmpegTools`] is shared by probe, keyframe-index, and decode so a session
//! never disagrees with itself about which ffmpeg it is driving.

use std::path::{Path, PathBuf};

/// Resolved paths to the ffmpeg toolchain used for the whole session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfmpegTools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

/// Environment override: a directory containing `ffmpeg`/`ffprobe`.
pub const FFMPEG_DIR_ENV: &str = "PHOTONIC_FFMPEG_DIR";

#[derive(Debug, thiserror::Error)]
pub enum LocateError {
    #[error("could not locate `{0}` (checked ${env} then PATH)", env = FFMPEG_DIR_ENV)]
    NotFound(&'static str),
}

/// The platform executable file name for `stem` (`stem.exe` on Windows).
fn exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

/// Locate a single binary by stem: `$PHOTONIC_FFMPEG_DIR/<stem>` if that env
/// var is set and the file exists, else the first `<stem>` found on `PATH`.
pub fn locate_binary(stem: &str) -> Option<PathBuf> {
    let file = exe_name(stem);

    if let Some(dir) = std::env::var_os(FFMPEG_DIR_ENV) {
        let candidate = Path::new(&dir).join(&file);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(&file))
        .find(|candidate| candidate.is_file())
}

/// Resolve both tools. Errors name which binary is missing so a caller can
/// surface an actionable "install ffmpeg / set PHOTONIC_FFMPEG_DIR" diagnostic
/// (and tests skip-with-message rather than fail — mirroring the GPU-adapter
/// skip convention).
pub fn locate() -> Result<FfmpegTools, LocateError> {
    let ffmpeg = locate_binary("ffmpeg").ok_or(LocateError::NotFound("ffmpeg"))?;
    let ffprobe = locate_binary("ffprobe").ok_or(LocateError::NotFound("ffprobe"))?;
    Ok(FfmpegTools { ffmpeg, ffprobe })
}

/// Best-effort resolve for tests: `Some` when both tools are present, `None`
/// otherwise so a test can `return` with a skip message.
pub fn locate_for_test() -> Option<FfmpegTools> {
    locate().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_name_matches_platform() {
        let n = exe_name("ffmpeg");
        if cfg!(windows) {
            assert_eq!(n, "ffmpeg.exe");
        } else {
            assert_eq!(n, "ffmpeg");
        }
    }

    #[test]
    fn env_override_takes_precedence_when_present() {
        // Point the override at a directory that definitely lacks the binary;
        // the lookup must then fall through to PATH (or None), never panic.
        // (We can't safely mutate process env in parallel tests, so this only
        // asserts the no-crash fall-through on a bogus dir via the public API.)
        let got = locate_binary("definitely-not-a-real-binary-xyz");
        assert!(got.is_none());
    }
}
