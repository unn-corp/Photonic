//! Minimal seek + sequential-fill driver for one decode source (02 §3).
//!
//! This is the P3 floor: [`DecodeSource::seek`] picks the keyframe at or before
//! the target, (re)starts the sidecar there, decode-forwards discarding until
//! `pts >= target`, and fills the ring; [`DecodeSource::pump`] keeps the ring
//! filled for sequential playback. Cut-ahead / cross-clip prefetch is a later
//! story (playback controller) — the seams (`SharedRing`, restart budget, the
//! reused sidecar) are left in place for it.
//!
//! Failure containment (02 §3): a sidecar crash / mid-frame EOF surfaces as
//! [`DecodeError`]; `seek` restarts the process up to [`MAX_RESTARTS`] with
//! backoff before giving up, and a wedged pipe can never block the caller
//! because every read has already happened on this (worker) thread with the
//! process killed-on-drop.

use std::path::PathBuf;
use std::process::ChildStdout;
use std::sync::Arc;
use std::time::Duration;

use photonic_core::timeline::{FrameRate, Tick};

use super::reader::{FrameReader, PtsModel};
use super::ring::SharedRing;
use super::sidecar::{DecodeCancellation, Sidecar, SidecarConfig};
use super::{DecodeError, DecodedFrame, PixFmt};
use crate::media::ffmpeg_locate::FfmpegTools;
use crate::media::keyframe_index::{KeyframeIndex, PtsIndex};

/// Max process restarts per seek before giving up (02 §3 "max 3").
pub const MAX_RESTARTS: u32 = 3;

/// How a source's presentation ticks are derived (chosen at import from the
/// probe's VFR flag — 02 §4).
#[derive(Clone)]
pub enum PtsKind {
    /// Constant-frame-rate: arithmetic derivation, no extra probe.
    Cfr(FrameRate),
    /// Variable-frame-rate: ground-truth table (pts-true path).
    Vfr(Arc<PtsIndex>),
}

/// Everything needed to open a decode source.
#[derive(Clone)]
pub struct SourceParams {
    pub input: PathBuf,
    pub width: u32,
    pub height: u32,
    pub pix_fmt: PixFmt,
    pub pts_kind: PtsKind,
    pub keyframes: KeyframeIndex,
}

/// A single active decode source: keyframe-seek + sequential fill into a ring.
pub struct DecodeSource {
    tools: FfmpegTools,
    params: SourceParams,
    ring: SharedRing,
    // The live process + its parsed pipe. Both `None` until the first seek; a
    // restart drops (kills) and re-creates them together.
    sidecar: Option<Sidecar>,
    reader: Option<FrameReader<ChildStdout>>,
    /// True while CUDA is still usable for this source. A failed CUDA decode
    /// permanently falls back to software for the lifetime of the source.
    cuda_enabled: bool,
    known_end_pts: Option<Tick>,
    process_last_pts: Option<Tick>,
    software_decoder: Option<&'static str>,
    cancellation: DecodeCancellation,
}

impl DecodeSource {
    pub fn new(tools: FfmpegTools, params: SourceParams, ring: SharedRing) -> Self {
        let cuda_enabled = super::sidecar::cuda_hwaccel_listed(&tools.ffmpeg);
        DecodeSource {
            tools,
            params,
            ring,
            sidecar: None,
            reader: None,
            cuda_enabled,
            known_end_pts: None,
            process_last_pts: None,
            software_decoder: None,
            cancellation: DecodeCancellation::default(),
        }
    }

    /// Preview VP9 alpha requires libvpx-vp9: FFmpeg's native VP9 decoder
    /// discards the alpha side channel. This opt-in keeps normal sources on
    /// their existing decoder selection and disables incompatible HW decode.
    pub(crate) fn with_software_decoder(mut self, decoder: &'static str) -> Self {
        self.software_decoder = Some(decoder);
        self.cuda_enabled = false;
        self
    }

    pub(crate) fn cancellation(&self) -> DecodeCancellation {
        self.cancellation.clone()
    }

    /// The ring this source fills (shared with the consumer).
    pub fn ring(&self) -> &SharedRing {
        &self.ring
    }

    /// The backing file this source decodes — original or proxy, per the
    /// proxy-selection made at construction (02 §6). Exposed for the
    /// proxy-selection integration test and diagnostics.
    pub fn input_path(&self) -> &std::path::Path {
        &self.params.input
    }

    /// Source frame ordinal of the keyframe at `kf_tick`, for the pts model.
    fn start_frame(&self, kf_tick: Tick) -> i64 {
        match &self.params.pts_kind {
            PtsKind::Cfr(rate) => cfr_keyframe_ordinal(*rate, kf_tick),
            PtsKind::Vfr(table) => table.ordinal_before(kf_tick),
        }
    }

    fn pts_model(&self, start_frame: i64) -> PtsModel {
        match &self.params.pts_kind {
            PtsKind::Cfr(rate) => PtsModel::Cfr {
                rate: *rate,
                start_frame,
            },
            PtsKind::Vfr(table) => PtsModel::Table {
                start_frame,
                table: Arc::clone(table),
            },
        }
    }

    /// Start (or restart) the ffmpeg process seeked to `kf_tick` and rebuild the
    /// reader with a pts model whose origin is that keyframe.
    fn start_process(&mut self, kf_tick: Tick, use_cuda: bool) -> Result<(), DecodeError> {
        if self.cancellation.cancelled() {
            return Err(DecodeError::Cancelled);
        }
        // Drop the old process/reader first (kill-on-drop) before respawning.
        self.reader = None;
        self.sidecar = None;
        self.process_last_pts = None;

        let cfg = SidecarConfig {
            input: self.params.input.clone(),
            seek: kf_tick,
            pix_fmt: self.params.pix_fmt,
        };
        let (sidecar, stdout) =
            Sidecar::spawn_with_decoder(&self.tools, &cfg, use_cuda, self.software_decoder)?;
        if !self.cancellation.register(sidecar.child_handle()) {
            return Err(DecodeError::Cancelled);
        }
        let model = self.pts_model(self.start_frame(kf_tick));
        let reader = FrameReader::new(
            stdout,
            self.params.width,
            self.params.height,
            self.params.pix_fmt,
            model,
        );
        self.sidecar = Some(sidecar);
        self.reader = Some(reader);
        Ok(())
    }

    /// Playback requests the frame whose interval contains a possibly fractional
    /// source tick. Keep `seek`'s public at-or-after contract for other callers.
    pub fn seek_covering(&mut self, target: Tick) -> Result<Arc<DecodedFrame>, DecodeError> {
        if target < Tick::ZERO {
            return Err(DecodeError::EmptyDecode);
        }
        let boundary = match &self.params.pts_kind {
            PtsKind::Cfr(rate) => rate.snap(target),
            PtsKind::Vfr(table) => table
                .pts_at(table.ordinal_before(target))
                .ok_or(DecodeError::EmptyDecode)?,
        };
        self.seek(boundary)
    }

    /// Seek so the frame at or after `target` is decoded and ringed.
    ///
    /// Picks `keyframe_before(target)`, (re)starts the process there, discards
    /// frames with `pts < target`, then rings the first frame with
    /// `pts >= target` (and returns it). On a sidecar crash mid-seek, restarts
    /// up to [`MAX_RESTARTS`] with backoff.
    pub fn seek(&mut self, target: Tick) -> Result<Arc<DecodedFrame>, DecodeError> {
        let kf = self.params.keyframes.keyframe_before(target);
        self.ring.clear();

        let mut last_err = None;
        let mut modes = [false, false];
        let mode_count = if self.cuda_enabled { 2 } else { 1 };
        if self.cuda_enabled {
            modes[0] = true;
        }
        for mode in modes.into_iter().take(mode_count) {
            if self.cancellation.cancelled() {
                return Err(DecodeError::Cancelled);
            }
            match self.seek_mode(kf, target, mode) {
                Ok(frame) => {
                    self.ring.set_playhead(frame.pts);
                    return Ok(frame);
                }
                Err(DecodeError::EmptyDecode) => {
                    last_err = Some(DecodeError::EmptyDecode);
                }
                Err(e) => last_err = Some(e),
            }
            if mode {
                // `-hwaccel cuda` can be listed by ffmpeg while the runtime
                // driver/device is unavailable. Retry the exact seek in
                // software and keep that choice for later seeks.
                self.cuda_enabled = false;
            }
        }
        Err(DecodeError::RestartsExhausted {
            max: MAX_RESTARTS,
            last: last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown".into()),
        })
    }

    fn seek_mode(
        &mut self,
        kf: Tick,
        target: Tick,
        use_cuda: bool,
    ) -> Result<Arc<DecodedFrame>, DecodeError> {
        let mut last_err: Option<DecodeError> = None;
        for attempt in 0..=MAX_RESTARTS {
            if self.cancellation.cancelled() {
                return Err(DecodeError::Cancelled);
            }
            if attempt > 0 {
                // Exponential-ish backoff: 20ms, 40ms, 80ms.
                std::thread::sleep(Duration::from_millis(20u64 << (attempt - 1)));
            }
            match self.seek_attempt(kf, target, use_cuda) {
                Ok(frame) => return Ok(frame),
                Err(DecodeError::EmptyDecode) => return Err(DecodeError::EmptyDecode),
                Err(e) => last_err = Some(e),
            }
        }
        Err(DecodeError::RestartsExhausted {
            max: MAX_RESTARTS,
            last: last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown".into()),
        })
    }

    /// One seek attempt: spawn at `kf`, decode-forward discarding `pts < target`,
    /// ring the first `pts >= target` frame and return it.
    fn seek_attempt(
        &mut self,
        kf: Tick,
        target: Tick,
        use_cuda: bool,
    ) -> Result<Arc<DecodedFrame>, DecodeError> {
        self.start_process(kf, use_cuda)?;
        let reader = self.reader.as_mut().expect("reader set by start_process");

        loop {
            if self.cancellation.cancelled() {
                return Err(DecodeError::Cancelled);
            }
            // The reader's PTS model can answer this before consuming the raw
            // bytes. Pre-target GOP frames are never retained, so read them
            // through its reusable discard buffer rather than allocating one
            // decoded payload per frame on a long-GOP seek.
            match reader.next_pts() {
                Some(pts) if pts < target => match reader.discard_next_frame()? {
                    Some(pts) => self.process_last_pts = Some(pts),
                    None => {
                        if !use_cuda {
                            self.known_end_pts = self.process_last_pts;
                        }
                        return Err(DecodeError::EmptyDecode);
                    }
                },
                Some(_) => match reader.next_frame()? {
                    Some(frame) => {
                        let pts = frame.pts;
                        self.process_last_pts = Some(pts);
                        self.ring.push(frame);
                        return self.ring.get(pts).ok_or(DecodeError::EmptyDecode);
                    }
                    None => {
                        if !use_cuda {
                            self.known_end_pts = self.process_last_pts;
                        }
                        return Err(DecodeError::EmptyDecode);
                    }
                },
                None => {
                    if !use_cuda {
                        self.known_end_pts = self.process_last_pts;
                    }
                    return Err(DecodeError::EmptyDecode);
                }
            }
        }
    }

    /// Scrub preview: ensure the keyframe covering `target` is resident,
    /// decoding **only that keyframe** (no forward-decode to `target`, no pump).
    /// Returns `true` if it had to (re)seek, `false` if the keyframe was already
    /// resident (a prior scrub landed in the same GOP). Cheap enough to run per
    /// drag frame — the discard loop [`seek`](Self::seek) runs is what makes a
    /// normal seek expensive on long GOPs.
    pub fn scrub_to(&mut self, target: Tick) -> Result<bool, DecodeError> {
        let kf = self.params.keyframes.keyframe_before(target);
        if self.ring.get(kf).is_some() {
            return Ok(false);
        }
        self.seek_keyframe(kf)?;
        Ok(true)
    }

    /// Seek to keyframe tick `kf` and ring the first decoded frame (the
    /// keyframe itself), skipping the `pts < target` discard loop.
    fn seek_keyframe(&mut self, kf: Tick) -> Result<Arc<DecodedFrame>, DecodeError> {
        self.ring.clear();
        let mut last_err = None;
        let mut modes = [false, false];
        let mode_count = if self.cuda_enabled { 2 } else { 1 };
        if self.cuda_enabled {
            modes[0] = true;
        }
        for mode in modes.into_iter().take(mode_count) {
            if self.cancellation.cancelled() {
                return Err(DecodeError::Cancelled);
            }
            match self.seek_keyframe_mode(kf, mode) {
                Ok(frame) => {
                    self.ring.set_playhead(frame.pts);
                    return Ok(frame);
                }
                Err(DecodeError::EmptyDecode) => last_err = Some(DecodeError::EmptyDecode),
                Err(e) => last_err = Some(e),
            }
            if mode {
                self.cuda_enabled = false;
            }
        }
        Err(DecodeError::RestartsExhausted {
            max: MAX_RESTARTS,
            last: last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown".into()),
        })
    }

    fn seek_keyframe_mode(
        &mut self,
        kf: Tick,
        use_cuda: bool,
    ) -> Result<Arc<DecodedFrame>, DecodeError> {
        let mut last_err: Option<DecodeError> = None;
        for attempt in 0..=MAX_RESTARTS {
            if self.cancellation.cancelled() {
                return Err(DecodeError::Cancelled);
            }
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(20u64 << (attempt - 1)));
            }
            match self.seek_keyframe_attempt(kf, use_cuda) {
                Ok(frame) => return Ok(frame),
                Err(DecodeError::EmptyDecode) => return Err(DecodeError::EmptyDecode),
                Err(e) => last_err = Some(e),
            }
        }
        Err(DecodeError::RestartsExhausted {
            max: MAX_RESTARTS,
            last: last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown".into()),
        })
    }

    fn seek_keyframe_attempt(
        &mut self,
        kf: Tick,
        use_cuda: bool,
    ) -> Result<Arc<DecodedFrame>, DecodeError> {
        self.start_process(kf, use_cuda)?;
        let reader = self.reader.as_mut().expect("reader set by start_process");
        match reader.next_frame()? {
            Some(frame) => {
                let pts = frame.pts;
                self.process_last_pts = Some(pts);
                self.ring.push(frame);
                self.ring.get(pts).ok_or(DecodeError::EmptyDecode)
            }
            None => Err(DecodeError::EmptyDecode),
        }
    }

    /// Decode up to `n` more frames from the current process into the ring
    /// (sequential playback fill). Returns how many were decoded; fewer than
    /// `n` (possibly 0) at end-of-stream or with no active process.
    pub fn pump(&mut self, n: usize) -> Result<usize, DecodeError> {
        if self.reader.is_none() {
            return Ok(0);
        }
        let result = self.pump_mode(n);
        if result.is_err() && self.cuda_enabled {
            // A CUDA process can initialize successfully and still fail once
            // the first frame is read (for example when the driver disappears
            // or the decoder cannot allocate a surface). Drop the dead process
            // so the worker's next iteration seeks again in software mode.
            self.cuda_enabled = false;
            self.reader = None;
            self.sidecar = None;
        }
        result
    }

    fn pump_mode(&mut self, n: usize) -> Result<usize, DecodeError> {
        let reader = self.reader.as_mut().expect("reader checked by pump");
        let mut count = 0;
        for _ in 0..n {
            if self.cancellation.cancelled() {
                return Err(DecodeError::Cancelled);
            }
            if !self.ring.can_prefetch() {
                break;
            }
            match reader.next_frame()? {
                Some(frame) => {
                    self.process_last_pts = Some(frame.pts);
                    self.ring.push(frame);
                    count += 1;
                }
                None => {
                    self.known_end_pts = self.process_last_pts;
                    break;
                } // clean EOF
            }
        }
        Ok(count)
    }

    /// A confirmed stream end, not merely a full ring or a stalled read.
    pub fn ended_before(&self, target: Tick, rate: FrameRate) -> bool {
        self.known_end_pts.is_some_and(|last| {
            // The last decoded picture gets its nominal interval, matching the
            // presenter's last-frame policy. Container audio duration is irrelevant.
            target.0 as i128 - last.0 as i128 >= rate.ticks_per_frame().0.max(1) as i128
        })
    }

    /// Whether a decode process is currently live.
    pub fn is_active(&self) -> bool {
        self.sidecar.is_some()
    }
}

/// A keyframe timestamp identifies a frame boundary, not an arbitrary playhead.
/// Containers commonly quantize CFR timestamps (Matroska often to milliseconds).
/// Flooring 33 ms at 30 fps labels decoded frame 1 as frame 0 and a cold seek
/// returns the following picture. Recover the nearest CFR boundary instead.
/// VFR continues using its actual per-frame PTS table.
fn cfr_keyframe_ordinal(rate: FrameRate, tick: Tick) -> i64 {
    let duration = rate.ticks_per_frame().0.max(1) as i128;
    ((tick.0 as i128 + duration / 2).div_euclid(duration)).clamp(i64::MIN as i128, i64::MAX as i128)
        as i64
}

#[cfg(test)]
mod cfr_seek_tests {
    use super::*;
    #[test]
    fn native_time_offset_cfr_keyframe_ordinals_survive_container_quantization() {
        use photonic_core::timeline::TICKS_PER_SECOND;
        let millisecond = TICKS_PER_SECOND / 1000;
        for rate in [
            FrameRate::FPS_24,
            FrameRate::FPS_25,
            FrameRate::FPS_30,
            FrameRate::new(50, 1),
            FrameRate::FPS_60,
            FrameRate::FPS_23_976,
            FrameRate::FPS_29_97,
            FrameRate::FPS_59_94,
            FrameRate::new(120, 1),
        ] {
            for ordinal in [0, 1, 2, 3, 10, 29, 30, 59, 60, 1001, 10000] {
                let tick = rate.frame_start(ordinal);
                for quantized in [
                    tick,
                    Tick(tick.0 / millisecond * millisecond),
                    Tick((tick.0 + millisecond / 2) / millisecond * millisecond),
                ] {
                    assert_eq!(
                        cfr_keyframe_ordinal(rate, quantized),
                        ordinal,
                        "{rate:?} {quantized:?}"
                    );
                }
            }
        }
        assert_eq!(
            cfr_keyframe_ordinal(FrameRate::FPS_30, Tick(millisecond * 33)),
            1
        );
        assert_eq!(
            FrameRate::FPS_30.frame_at(Tick(millisecond * 33)),
            0,
            "ordinary playhead covering-frame semantics must stay unchanged"
        );
        assert!(cfr_keyframe_ordinal(FrameRate::FPS_30, Tick(i64::MAX)) > 0);
        assert!(cfr_keyframe_ordinal(FrameRate::FPS_30, Tick(i64::MIN)) < 0);
    }
}
