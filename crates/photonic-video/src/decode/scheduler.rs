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
            PtsKind::Cfr(rate) => rate.frame_at(kf_tick),
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
                Some(pts) if pts < target => {
                    if reader.discard_next_frame()?.is_none() {
                        return Err(DecodeError::EmptyDecode);
                    }
                }
                Some(_) => match reader.next_frame()? {
                    Some(frame) => {
                        let pts = frame.pts;
                        self.ring.push(frame);
                        return self.ring.get(pts).ok_or(DecodeError::EmptyDecode);
                    }
                    None => return Err(DecodeError::EmptyDecode),
                },
                None => return Err(DecodeError::EmptyDecode),
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
                    self.ring.push(frame);
                    count += 1;
                }
                None => break, // clean EOF
            }
        }
        Ok(count)
    }

    /// Whether a decode process is currently live.
    pub fn is_active(&self) -> bool {
        self.sidecar.is_some()
    }
}
