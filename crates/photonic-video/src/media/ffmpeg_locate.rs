//! Locate the `ffmpeg`/`ffprobe` binaries (02 §3).
//!
//! Resolution order for each tool: the `PHOTONIC_FFMPEG_DIR` environment
//! override first (an explicit install the operator points us at), then a
//! plain `PATH` lookup only when no override was supplied. The *same*
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
    #[error("${env} points to `{dir}`, which does not contain `{binary}`", env = FFMPEG_DIR_ENV)]
    InvalidOverride { dir: PathBuf, binary: String },
    #[error("could not find ffmpeg and ffprobe together on PATH; install both in one directory or set ${env}", env = FFMPEG_DIR_ENV)]
    PairNotFound,
}

/// The platform executable file name for `stem` (`stem.exe` on Windows).
fn exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

fn locate_from(
    explicit_dir: Option<&Path>,
    path: Option<&std::ffi::OsStr>,
) -> Result<FfmpegTools, LocateError> {
    let pair_in = |dir: &Path| {
        let ffmpeg = dir.join(exe_name("ffmpeg"));
        let ffprobe = dir.join(exe_name("ffprobe"));
        (ffmpeg.is_file() && ffprobe.is_file()).then_some(FfmpegTools { ffmpeg, ffprobe })
    };
    if let Some(dir) = explicit_dir {
        let ffmpeg = dir.join(exe_name("ffmpeg"));
        let missing = if ffmpeg.is_file() {
            "ffprobe"
        } else {
            "ffmpeg"
        };
        return pair_in(dir).ok_or_else(|| LocateError::InvalidOverride {
            dir: dir.to_path_buf(),
            binary: exe_name(missing),
        });
    }
    path.and_then(|path| std::env::split_paths(path).find_map(|dir| pair_in(&dir)))
        .ok_or(LocateError::PairNotFound)
}

/// Resolve both tools from the same explicit install or the operator's PATH.
/// Errors identify a missing executable without switching to another build.
pub fn locate() -> Result<FfmpegTools, LocateError> {
    let override_dir = std::env::var_os(FFMPEG_DIR_ENV);
    let path = std::env::var_os("PATH");
    locate_from(override_dir.as_deref().map(Path::new), path.as_deref())
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
    fn explicit_install_cannot_fall_back_to_path() {
        let root =
            std::env::temp_dir().join(format!("photonic-ffmpeg-locate-{}", uuid::Uuid::new_v4()));
        let explicit = root.join("explicit");
        let on_path = root.join("path");
        std::fs::create_dir_all(&explicit).unwrap();
        std::fs::create_dir_all(&on_path).unwrap();
        std::fs::write(explicit.join(exe_name("ffmpeg")), b"ffmpeg").unwrap();
        std::fs::write(on_path.join(exe_name("ffprobe")), b"ffprobe").unwrap();
        let path = std::env::join_paths([&on_path]).unwrap();
        assert!(matches!(
            locate_from(Some(&explicit), Some(&path)),
            Err(LocateError::InvalidOverride { binary, .. }) if binary == exe_name("ffprobe")
        ));
        std::fs::write(explicit.join(exe_name("ffprobe")), b"ffprobe").unwrap();
        let resolved = locate_from(Some(&explicit), Some(&path)).unwrap();
        assert_eq!(resolved.ffmpeg, explicit.join(exe_name("ffmpeg")));
        assert_eq!(resolved.ffprobe, explicit.join(exe_name("ffprobe")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn path_lookup_requires_a_matching_pair() {
        let root =
            std::env::temp_dir().join(format!("photonic-ffmpeg-pair-{}", uuid::Uuid::new_v4()));
        let a = root.join("a");
        let b = root.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join(exe_name("ffmpeg")), b"ffmpeg").unwrap();
        std::fs::write(b.join(exe_name("ffprobe")), b"ffprobe").unwrap();
        let path = std::env::join_paths([&a, &b]).unwrap();
        assert!(matches!(
            locate_from(None, Some(&path)),
            Err(LocateError::PairNotFound)
        ));
        std::fs::write(b.join(exe_name("ffmpeg")), b"ffmpeg").unwrap();
        let resolved = locate_from(None, Some(&path)).unwrap();
        assert_eq!(resolved.ffmpeg, b.join(exe_name("ffmpeg")));
        assert_eq!(resolved.ffprobe, b.join(exe_name("ffprobe")));
        std::fs::remove_dir_all(root).unwrap();
    }
}
