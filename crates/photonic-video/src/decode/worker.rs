//! Background decode worker (02 §3, decode-worker-pool story).
//!
//! Moves per-source ffmpeg decode off the engine thread: each active decode
//! source gets one worker thread that keeps its [`SharedRing`](super::SharedRing)
//! pumped ahead of the playhead while the engine thread composites the current
//! frame. Decode (CPU / ffmpeg) and compositing (GPU) then overlap instead of
//! serializing on the engine thread — which is what lets preview playback
//! approach the source frame rate instead of stalling one frame at a time.
//!
//! The worker shares the [`DecodeSource`] with the engine via `Arc<Mutex<..>>`
//! and only ever [`try_lock`](std::sync::Mutex::try_lock)s it, so it never
//! blocks the engine's cold-miss inline seek: it just yields that iteration and
//! resumes pumping once the engine releases the lock. Dropping the
//! [`DecodeWorker`] stops and joins the thread; the sidecar process is killed
//! when the shared [`DecodeSource`] finally drops.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use photonic_core::timeline::{FrameRate, Tick};

use crate::decode::scheduler::DecodeSource;
use crate::playback::prefetch::pump_ahead;

// Diagnostic counters (temporary): how the worker spent its decode budget.
pub static WORKER_SEEKS: AtomicU64 = AtomicU64::new(0);
pub static WORKER_PUMPED: AtomicU64 = AtomicU64::new(0);

/// (seeks, frames_pumped) since process start.
pub fn worker_stats() -> (u64, u64) {
    (
        WORKER_SEEKS.load(Ordering::Relaxed),
        WORKER_PUMPED.load(Ordering::Relaxed),
    )
}

/// How long the worker parks when it is caught up (ring already pumped ahead)
/// or has no steer target yet. A steer or stop wakes it immediately.
const IDLE_PARK: Duration = Duration::from_millis(4);

/// If the steer target is more than this many frames ahead of the decode
/// frontier, re-seek instead of decoding forward to reach it (a genuine jump,
/// not sequential drift). A target behind the oldest resident frame always
/// re-seeks. Sequential playback keeps the target inside `[oldest, frontier]`,
/// so it never triggers.
const SEEK_AHEAD_FRAMES: i64 = 32;

struct Steer {
    /// Latest source-time playhead the engine asked us to pump ahead of.
    target: Option<Tick>,
    /// Scrub mode: decode only the keyframe covering `target` (cheap preview,
    /// no forward-decode-to-target discard, no pump-ahead) so dragging the
    /// playhead stays responsive. Cleared when the engine steers normally.
    scrub: bool,
    stop: bool,
}

struct Ctrl {
    steer: Mutex<Steer>,
    wake: Condvar,
}

/// Handle to one source's background decode thread.
pub struct DecodeWorker {
    ctrl: Arc<Ctrl>,
    join: Option<JoinHandle<()>>,
    cancellation: super::sidecar::DecodeCancellation,
}

impl DecodeWorker {
    /// Spawn a worker that keeps `decode`'s ring pumped ahead of the steered
    /// playhead. `rate` is the source frame rate (for the look-ahead target).
    pub fn spawn(decode: Arc<Mutex<DecodeSource>>, rate: FrameRate) -> Self {
        let cancellation = decode
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cancellation();
        let ctrl = Arc::new(Ctrl {
            steer: Mutex::new(Steer {
                target: None,
                scrub: false,
                stop: false,
            }),
            wake: Condvar::new(),
        });
        let worker_ctrl = Arc::clone(&ctrl);
        let join = std::thread::Builder::new()
            .name("photonic-decode".into())
            .spawn(move || run(worker_ctrl, decode, rate))
            .expect("spawn decode worker thread");
        DecodeWorker {
            ctrl,
            cancellation,
            join: Some(join),
        }
    }

    /// Point the worker at the current playhead (source-time ticks) for normal
    /// forward-decode playback. Latest-wins and idempotent: an unchanged target
    /// in the same mode does not wake the thread. Clears scrub mode.
    pub fn steer(&self, target: Tick) {
        let mut g = self.ctrl.steer.lock().unwrap_or_else(|e| e.into_inner());
        if g.target != Some(target) || g.scrub {
            g.target = Some(target);
            g.scrub = false;
            self.ctrl.wake.notify_one();
        }
    }

    /// Point the worker at a scrub target: decode just the keyframe covering it
    /// (fast preview while the playhead is being dragged). Latest-wins.
    pub fn steer_scrub(&self, target: Tick) {
        let mut g = self.ctrl.steer.lock().unwrap_or_else(|e| e.into_inner());
        if g.target != Some(target) || !g.scrub {
            g.target = Some(target);
            g.scrub = true;
            self.ctrl.wake.notify_one();
        }
    }
}

impl Drop for DecodeWorker {
    fn drop(&mut self) {
        {
            let mut g = self.ctrl.steer.lock().unwrap_or_else(|e| e.into_inner());
            g.stop = true;
        }
        self.cancellation.cancel();
        self.ctrl.wake.notify_all();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn run(ctrl: Arc<Ctrl>, decode: Arc<Mutex<DecodeSource>>, rate: FrameRate) {
    let tpf = rate.ticks_per_frame().0;
    loop {
        let (target, scrub) = {
            let g = ctrl.steer.lock().unwrap_or_else(|e| e.into_inner());
            if g.stop {
                return;
            }
            (g.target, g.scrub)
        };
        let Some(t) = target else {
            // No playhead yet — park until the engine steers us.
            let g = ctrl.steer.lock().unwrap_or_else(|e| e.into_inner());
            if g.stop {
                return;
            }
            drop(wait_timeout_tolerant(&ctrl.wake, g, IDLE_PARK));
            continue;
        };

        // The worker owns the decoder. In scrub mode it decodes only the
        // keyframe covering `t` (cheap preview, no discard-to-target, no pump).
        // In normal mode it seeks once to bootstrap (or on a real discontinuity)
        // then decodes *forward* to keep the ring pumped ahead. Forward decode is
        // cheap (one frame); a seek respawns ffmpeg + decodes a GOP, so we do it
        // as rarely as possible. The engine thread never seeks.
        let progressed = {
            let mut src = decode.lock().unwrap_or_else(|e| e.into_inner());
            if scrub {
                WORKER_SEEKS.fetch_add(1, Ordering::Relaxed);
                src.scrub_to(t).unwrap_or(false)
            } else {
                let need_seek = match (src.ring().oldest(), src.ring().newest()) {
                    // Sequential playback stays inside the resident window; a jump
                    // before the oldest frame or far past the frontier re-seeks.
                    (Some(lo), Some(hi)) => t.0 < lo.0 || t.0 > hi.0 + SEEK_AHEAD_FRAMES * tpf,
                    // Empty ring (cold start / just after a clear) ⇒ bootstrap seek.
                    _ => true,
                };
                if !src.is_active() || need_seek {
                    WORKER_SEEKS.fetch_add(1, Ordering::Relaxed);
                    src.seek(t).is_ok()
                } else {
                    let n = pump_ahead(&mut src, t, rate);
                    WORKER_PUMPED.fetch_add(n as u64, Ordering::Relaxed);
                    n > 0
                }
            }
        };
        if progressed {
            // Decoded something — loop straight back to keep filling the ring
            // ahead of the playhead without parking.
            continue;
        }

        // Caught up (ring already pumped ahead) or end-of-stream: park until the
        // playhead moves (a new steer) or we're told to stop.
        let g = ctrl.steer.lock().unwrap_or_else(|e| e.into_inner());
        if g.stop {
            return;
        }
        drop(wait_timeout_tolerant(&ctrl.wake, g, IDLE_PARK));
    }
}

/// `Condvar::wait_timeout` that recovers the guard out of a `PoisonError`
/// instead of panicking, so one panicking decode worker cannot cascade a poison
/// panic into another thread parked on the same condvar (P0-2). The returned
/// `WaitTimeoutResult` is dropped by callers, so only the guard is threaded back.
fn wait_timeout_tolerant<'a, T>(
    cvar: &Condvar,
    guard: std::sync::MutexGuard<'a, T>,
    dur: Duration,
) -> std::sync::MutexGuard<'a, T> {
    match cvar.wait_timeout(guard, dur) {
        Ok((g, _)) => g,
        Err(poisoned) => poisoned.into_inner().0,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::decode::scheduler::{PtsKind, SourceParams};
    use crate::decode::{PixFmt, SharedRing};
    use crate::media::ffmpeg_locate::FfmpegTools;
    use crate::media::keyframe_index::KeyframeIndex;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    #[test]
    fn t005_worker_drop_interrupts_a_blocked_decode_pipe() {
        let path =
            std::env::temp_dir().join(format!("photonic-stalled-decoder-{}", uuid::Uuid::new_v4()));
        let marker = std::path::PathBuf::from(format!("{}.started", path.display()));
        std::fs::write(&path,b"#!/bin/sh\ncase \"$*\" in *-hwaccels*) exit 0;; esac\n: > \"$0.started\"\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let tools = FfmpegTools {
            ffmpeg: path.clone(),
            ffprobe: path.clone(),
        };
        let params = SourceParams {
            input: path.clone(),
            width: 2,
            height: 2,
            pix_fmt: PixFmt::Yuv420p,
            pts_kind: PtsKind::Cfr(FrameRate::FPS_30),
            keyframes: KeyframeIndex {
                keyframes: vec![Tick::ZERO],
            },
        };
        let decode = Arc::new(Mutex::new(DecodeSource::new(
            tools,
            params,
            SharedRing::preview(),
        )));
        let worker = DecodeWorker::spawn(decode, FrameRate::FPS_30);
        worker.steer(Tick::ZERO);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !marker.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            marker.exists(),
            "mock process did not reach its blocking read"
        );
        let start = Instant::now();
        drop(worker);
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "shutdown waited for the blocked pipe: {:?}",
            start.elapsed()
        );
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(marker);
    }
}
