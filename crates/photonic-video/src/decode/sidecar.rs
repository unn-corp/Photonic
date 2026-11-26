//! FFmpeg decode sidecar: one process, `rawvideo` on stdout (02 §3).
//!
//! We build the args ourselves (no `ffmpeg-sidecar` crate) so the exact seek /
//! pixel-format / verbosity flags are under our control. The process is killed
//! on drop so a dropped [`DecodeSource`](super::scheduler::DecodeSource) never
//! leaks an ffmpeg. stderr is drained on a worker thread (a full stderr pipe
//! would otherwise deadlock ffmpeg) and its tail is kept for error reporting.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use photonic_core::timeline::Tick;

use super::{DecodeError, PixFmt};
use crate::media::ffmpeg_locate::FfmpegTools;

/// What to decode and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidecarConfig {
    pub input: PathBuf,
    /// Input-level seek origin (a keyframe tick from the index). `-ss` before
    /// `-i` = fast keyframe-accurate seek; decode-forward discards to the target.
    pub seek: Tick,
    pub pix_fmt: PixFmt,
    pub frame_sync: FrameSync,
}

/// How FFmpeg maps decoded frames onto the raw output stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameSync {
    /// FFmpeg's default constant-rate output. Duplicates fill timestamp gaps,
    /// so output ordinal `i` stays at `start_frame + i` source frames for the
    /// arithmetic CFR timestamp model.
    ConstantRate,
    /// One output frame per decoded frame, so ordinal `i` matches the VFR
    /// timestamp table entry `start_frame + i`.
    Passthrough,
}

/// Lines of stderr kept for diagnostics (bounded ring).
const STDERR_TAIL: usize = 16;

/// A running ffmpeg decode process. Holds the child for kill-on-drop; stdout is
/// handed to the reader separately (so the reader can own it without a
/// self-referential borrow on the `Sidecar`).
pub struct Sidecar {
    child: Arc<Mutex<Child>>,
    stderr_tail: Arc<Mutex<Vec<String>>>,
}

impl Sidecar {
    /// Spawn ffmpeg and return the sidecar plus its stdout pipe.
    ///
    /// Args: `ffmpeg -hide_banner -nostdin -loglevel error -ss <seek> -i <file>
    /// -map 0:v:0 -an [<passthrough sync>] -f rawvideo -pix_fmt <fmt> pipe:1`. Input `-ss` seeks to the keyframe at
    /// or before `seek`; the caller decode-forwards to the exact target.
    pub fn spawn(
        tools: &FfmpegTools,
        cfg: &SidecarConfig,
        use_cuda: bool,
    ) -> Result<(Self, ChildStdout), DecodeError> {
        Self::spawn_with_decoder(tools, cfg, use_cuda, None)
    }

    pub(crate) fn spawn_with_decoder(
        tools: &FfmpegTools,
        cfg: &SidecarConfig,
        use_cuda: bool,
        decoder: Option<&str>,
    ) -> Result<(Self, ChildStdout), DecodeError> {
        let seek_secs = format!("{:.6}", cfg.seek.as_seconds_f64());
        let mut command = Command::new(&tools.ffmpeg);
        command.args(["-hide_banner", "-nostdin", "-loglevel", "error"]);
        // Prefer NVDEC only after the caller has opted into the capability.
        // Capability probing is not enough to prove that a driver/device can
        // initialize; the scheduler owns the software fallback when a decode
        // attempt fails.
        if use_cuda {
            command.args(["-hwaccel", "cuda"]);
        }
        if let Some(decoder) = decoder {
            command.args(["-c:v", decoder]);
        }
        command
            .arg("-ss")
            .arg(&seek_secs)
            .arg("-i")
            .arg(&cfg.input)
            // Probe dimensions and frame indexes refer to this stream.
            .args(["-map", "0:v:0", "-an"]);
        if cfg.frame_sync == FrameSync::Passthrough {
            // FrameReader assigns one table timestamp to each raw frame.
            let sync_flag = if fps_mode_supported(&tools.ffmpeg) {
                "-fps_mode"
            } else {
                "-vsync"
            };
            command.args([sync_flag, "passthrough", "-enc_time_base", "1:1000000"]);
        }
        command
            .args(["-f", "rawvideo", "-pix_fmt", cfg.pix_fmt.ffmpeg_name()])
            .arg("pipe:1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // 37 §2.2: on Linux ask the kernel to SIGKILL this ffmpeg child if the
        // editor process dies, so a hard-killed parent leaves no orphan decoder.
        crate::media::child_registry::arm_parent_death_signal(&mut command);
        let mut child = command.spawn().map_err(DecodeError::Spawn)?;

        // We requested `Stdio::piped()`, so stdout should be present — but if
        // ffmpeg was killed / exited between spawn and here, `take` yields None.
        // Return a typed error instead of panicking, and don't leak the child
        // (std's `Child` drop does not kill the process).
        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(DecodeError::Spawn(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "ffmpeg stdout pipe unavailable (process exited during spawn)",
                )));
            }
        };

        // Drain stderr on a worker thread; a full pipe would wedge ffmpeg.
        let stderr_tail = Arc::new(Mutex::new(Vec::<String>::new()));
        if let Some(stderr) = child.stderr.take() {
            let tail = Arc::clone(&stderr_tail);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stderr);
                let mut line = Vec::with_capacity(4096);
                loop {
                    let available = match reader.fill_buf() {
                        Ok(bytes) if !bytes.is_empty() => bytes,
                        _ => break,
                    };
                    let newline = available.iter().position(|byte| *byte == b'\n');
                    let consumed = newline.map_or(available.len(), |index| index + 1);
                    let keep = consumed.min(4096_usize.saturating_sub(line.len()));
                    line.extend_from_slice(&available[..keep]);
                    reader.consume(consumed);
                    if newline.is_some() {
                        let text = String::from_utf8_lossy(&line).trim_end().to_owned();
                        line.clear();
                        let mut t = tail.lock().unwrap_or_else(PoisonError::into_inner);
                        if t.len() == STDERR_TAIL {
                            t.remove(0);
                        }
                        t.push(text);
                    }
                }
            });
        }

        Ok((
            Sidecar {
                child: Arc::new(Mutex::new(child)),
                stderr_tail,
            },
            stdout,
        ))
    }

    pub(crate) fn child_handle(&self) -> Arc<Mutex<Child>> {
        Arc::clone(&self.child)
    }

    /// The last lines ffmpeg wrote to stderr (for a `DecodeError` message).
    pub fn stderr_tail(&self) -> String {
        // Poison-tolerant: recover the tail even if the drain thread panicked.
        self.stderr_tail
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .join("\n")
    }

    /// Whether the process has exited (non-blocking check).
    pub fn has_exited(&mut self) -> bool {
        matches!(
            self.child
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .try_wait(),
            Ok(Some(_))
        )
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        // Kill-on-drop (02 §3). Ignore errors — the process may already be gone.
        let mut child = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Cancellation handle independent of the decoder mutex. Killing the active
/// child interrupts a blocked raw-frame read before its worker is joined.
#[derive(Clone, Default)]
pub(crate) struct DecodeCancellation(Arc<CancellationState>);
#[derive(Default)]
struct CancellationState {
    cancelled: AtomicBool,
    child: Mutex<Option<Arc<Mutex<Child>>>>,
}
impl DecodeCancellation {
    pub(crate) fn cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }
    pub(crate) fn register(&self, child: Arc<Mutex<Child>>) -> bool {
        let mut active = self.0.child.lock().unwrap_or_else(PoisonError::into_inner);
        if self.cancelled() {
            let _ = child.lock().unwrap_or_else(PoisonError::into_inner).kill();
            return false;
        }
        *active = Some(child);
        true
    }
    pub(crate) fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        if let Some(child) = self
            .0
            .child
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            let _ = child.lock().unwrap_or_else(PoisonError::into_inner).kill();
        }
    }
}

/// Whether `ffmpeg -hwaccels` lists `cuda` (NVDEC), cached per executable.
pub(crate) fn cuda_hwaccel_listed(ffmpeg: &std::path::Path) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static CACHE: OnceLock<Mutex<HashMap<std::path::PathBuf, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = ffmpeg.to_path_buf();
    if let Some(value) = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
        .copied()
    {
        return value;
    }
    let value = Command::new(ffmpeg)
        .args(["-hide_banner", "-hwaccels"])
        .output()
        .map(|out| {
            out.status.success()
                && String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .any(|line| line.trim() == "cuda")
        })
        .unwrap_or(false);
    cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key, value);
    value
}

/// Whether this FFmpeg accepts `-fps_mode` (FFmpeg 5.1+), cached per
/// executable. Older releases only understand `-vsync`, which FFmpeg 9 removed.
/// An unreadable help listing assumes a current FFmpeg.
fn fps_mode_supported(ffmpeg: &std::path::Path) -> bool {
    use std::collections::HashMap;
    use std::sync::OnceLock;

    static CACHE: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = ffmpeg.to_path_buf();
    if let Some(value) = cache
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
        .copied()
    {
        return value;
    }
    let value = Command::new(ffmpeg)
        .args(["-hide_banner", "-h", "long"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map_or(true, |out| help_lists_fps_mode(&out.stdout));
    cache
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key, value);
    value
}

fn help_lists_fps_mode(help: &[u8]) -> bool {
    let help = String::from_utf8_lossy(help);
    let mut lines = help.lines().map(str::trim_start);
    if lines.clone().any(|line| line.starts_with("-fps_mode")) {
        return true;
    }
    // A listing that documents `-vsync` but not `-fps_mode` predates 5.1.
    !lines.any(|line| line.starts_with("-vsync"))
}

#[cfg(test)]
mod tests {
    use super::help_lists_fps_mode;

    #[test]
    fn fps_mode_detection_distinguishes_ffmpeg_generations() {
        let modern = b"Advanced Video options:\n-fps_mode[:<stream_spec>]  set framerate mode\n";
        let legacy = b"Advanced Video options:\n-vsync              video sync method\n";
        assert!(help_lists_fps_mode(modern));
        assert!(!help_lists_fps_mode(legacy));
        assert!(help_lists_fps_mode(b"unrecognized listing"));
    }
}
