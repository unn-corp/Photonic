//! The export render loop shell (02-engine.md §7): "for frame f in work
//! range → compile graph at tick(f) → eval GPU → readback `Rgba16Float` →
//! convert to encoder pix_fmt → write to encoder sidecar stdin... cancellable
//! between frames... `ExportProgress { frame, total, fps, eta }` events."
//!
//! [`export_frames`] is deliberately **engine-independent**: it takes a
//! `frame_source` closure (`u64` tick index → [`Frame`]) rather than a
//! `VideoEngine`/graph handle, so it compiles and is fully testable (with
//! synthetic gradient/counter frames) before the P3 evaluator exists. The
//! evaluator builder wires a real closure in later — that integration point
//! is intentionally the only thing this module doesn't own.
//!
//! Per-frame retiming (05 §6.2: nearest-source-frame resampling when
//! `FrameRatePolicy::Explicit` differs from the sequence rate) is the
//! evaluator's responsibility, not this loop's — `export_frames` just asks
//! `frame_source` for `total_frames` consecutive *output* ticks (`0..total`)
//! and encodes exactly what comes back.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use photonic_core::timeline::FrameRate;
use photonic_render::color::Colorimetry;

use super::convert;
use super::encoder::{
    plane_kind_for, validate_encode_options, AudioStreamSpec, EncodeError, EncodeSpec,
    EncoderCapabilities, EncoderProcess, PlaneKind,
};
use super::presets::ExportPreset;
use crate::media::ffmpeg_locate::FfmpegTools;

/// One evaluated frame handed to the render loop: linear, premultiplied,
/// Rec.709 RGBA — the CPU-side `f32` shape of an `Rgba16Float` working-texture
/// readback (03 §4.4 rule 3 keeps CPU reference math in `f32`, not `f16`).
/// `rgba_premult.len()` must equal `width * height * 4`.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba_premult: Vec<f32>,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ExportProgress {
    pub frame: u64,
    pub total: u64,
    pub fps: f32,
    pub eta: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExportEvent {
    Progress(ExportProgress),
    Done,
    Cancelled,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("encode error: {0}")]
    Encode(#[from] EncodeError),
    #[error("frame {frame} is {got_w}x{got_h} but the export was resolved to {want_w}x{want_h}")]
    FrameSizeMismatch {
        frame: u64,
        got_w: u32,
        got_h: u32,
        want_w: u32,
        want_h: u32,
    },
    /// The abstract preset could not be resolved against the sequence (bad
    /// sequence/format, empty range, upscaling refusal, or an invalid preset).
    /// Raised by [`super::job::resolve_export_job`].
    #[error("{0}")]
    Resolve(String),
    /// A frame was not produced within the per-frame deadline (02 §7): the run
    /// is poisoned rather than substituting content.
    #[error("render timeout: {0}")]
    RenderTimeout(String),
    #[error("frame {frame} has {got} RGBA components; expected {expected}")]
    FrameBufferSize {
        frame: u64,
        got: usize,
        expected: usize,
    },
    #[error("export conversion/encoder worker panicked")]
    WorkerPanicked,
}

/// Everything resolved from an [`ExportPreset`]'s abstract `ResolutionSpec`/
/// `FrameRatePolicy` (05 §3.2/§3.3) down to concrete numbers, plus the
/// output-color target and destination path. Resolving these from a
/// `Sequence`/`SequenceFormat` (01 §4) is the caller's job — this module only
/// consumes the resolved result.
pub struct ResolvedExport {
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
    pub audio: Option<AudioStreamSpec>,
    pub out_path: PathBuf,
    /// Target matrix/range for the YUV conversion (§3.5 step 4) — independent
    /// of the *source's* probed colorimetry (03 §3.5: "re-matrices, does not
    /// just relabel").
    pub colorimetry: Colorimetry,
    /// K-F5: prefer hardware encoder (fail-closed if unavailable).
    pub prefer_hardware: bool,
    /// K-F4: optional ffmpeg `-preset` speed.
    pub encoder_speed: Option<String>,
    /// K-F5: free-form encoder args.
    pub raw_encoder_args: Vec<String>,
    /// K-F polish: burn sequence timecode into the picture via ffmpeg drawtext.
    pub burn_in_timecode: bool,
    /// Unsupported two-pass requests fail before probing or creating files.
    pub two_pass: bool,
}

impl ResolvedExport {
    /// Construct with default (software, no extras) job options.
    pub fn basic(
        width: u32,
        height: u32,
        frame_rate: FrameRate,
        audio: Option<AudioStreamSpec>,
        out_path: PathBuf,
        colorimetry: Colorimetry,
    ) -> Self {
        Self {
            width,
            height,
            frame_rate,
            audio,
            out_path,
            colorimetry,
            prefer_hardware: false,
            encoder_speed: None,
            raw_encoder_args: Vec::new(),
            burn_in_timecode: false,
            two_pass: false,
        }
    }
}

/// A delivery from a common rendered frame stream. Audio stays per-output so
/// distinct codecs and loudness targets do not accidentally share a mix.
pub struct ExportTarget<'a> {
    pub preset: &'a ExportPreset,
    pub resolved: &'a ResolvedExport,
    pub audio_samples: Option<Vec<f32>>,
}

/// Bound the number of live encoder children and their I/O workers per group.
pub const MAX_SHARED_OUTPUTS: usize = 4;

/// Run one export. Rendering and GPU readback stay on the caller's thread;
/// a bounded worker converts and writes the preceding frame concurrently.
/// No GPU-map/next-render overlap is claimed by this API.
#[allow(clippy::too_many_arguments)]
pub fn export_frames(
    tools: &FfmpegTools,
    preset: &ExportPreset,
    resolved: &ResolvedExport,
    total_frames: u64,
    frame_source: impl FnMut(u64) -> Frame,
    audio_samples: Option<Vec<f32>>,
    cancel: &AtomicBool,
    mut on_event: impl FnMut(ExportEvent),
) -> Result<(), ExportError> {
    export_frames_multi(
        tools,
        vec![ExportTarget {
            preset,
            resolved,
            audio_samples,
        }],
        total_frames,
        frame_source,
        cancel,
        |_, event| on_event(event),
    )
}

/// Encode a common frame stream into multiple delivery formats. The targets
/// must agree on dimensions and cadence; the caller must also ensure the
/// stream represents the same sequence, framing, range, and quality choices.
/// [`super::job::run_export_jobs`] enforces those semantic constraints.
///
/// Only rendering/readback is shared. Conversion honors each target's transfer,
/// matrix, alpha and pixel layout, and each encoder receives its own audio.
/// One worker writes outputs in turn, so the slowest encoder backpressures the
/// shared producer. At most three input `Frame` values are live in this
/// pipeline (one producing, one queued, one consuming), independent of duration
/// and output count. Rendering/readback/resize and conversion scratch buffers
/// are additional to that queue bound.
/// Offline audio remains a complete PCM track per output; up to four such
/// tracks plus the encoder processes' own memory are outside that video bound.
/// Files publish individually after successful encoder completion.
pub fn export_frames_multi(
    tools: &FfmpegTools,
    targets: Vec<ExportTarget<'_>>,
    total_frames: u64,
    frame_source: impl FnMut(u64) -> Frame,
    cancel: &AtomicBool,
    mut on_event: impl FnMut(usize, ExportEvent),
) -> Result<(), ExportError> {
    if targets.len() > MAX_SHARED_OUTPUTS {
        return Err(ExportError::Resolve(format!(
            "shared export supports at most {MAX_SHARED_OUTPUTS} simultaneous outputs"
        )));
    }
    let Some(first) = targets.first() else {
        return Err(ExportError::Resolve(
            "export requires at least one output".into(),
        ));
    };
    let size = (first.resolved.width, first.resolved.height);
    let rate = first.resolved.frame_rate;
    if size.0 == 0 || size.1 == 0 || rate.num == 0 || rate.den == 0 {
        return Err(ExportError::Resolve(
            "export dimensions and frame rate must be positive".into(),
        ));
    }
    for (index, target) in targets.iter().enumerate() {
        validate_encode_options(target.resolved.two_pass)?;
        if (target.resolved.width, target.resolved.height) != size
            || target.resolved.frame_rate != rate
        {
            return Err(ExportError::Resolve(
                "shared exports must have identical dimensions and frame rate".into(),
            ));
        }
        if targets[..index]
            .iter()
            .any(|other| other.resolved.out_path == target.resolved.out_path)
        {
            return Err(ExportError::Resolve(
                "shared exports must use distinct output paths".into(),
            ));
        }
    }
    let output_count = targets.len();
    if cancel.load(Ordering::Relaxed) {
        for index in 0..output_count {
            on_event(index, ExportEvent::Cancelled);
        }
        return Ok(());
    }
    let caps = EncoderCapabilities::probe(tools)?;
    let mut encoders = Vec::with_capacity(output_count);
    for target in targets {
        let resolved = target.resolved;
        let spec = EncodeSpec {
            preset: target.preset,
            width: resolved.width,
            height: resolved.height,
            frame_rate: resolved.frame_rate,
            audio: resolved.audio,
            out_path: resolved.out_path.clone(),
            prefer_hardware: resolved.prefer_hardware,
            encoder_speed: resolved.encoder_speed.as_deref(),
            raw_encoder_args: &resolved.raw_encoder_args,
            burn_in_timecode: resolved.burn_in_timecode,
            two_pass: resolved.two_pass,
        };
        let process = EncoderProcess::spawn(tools, &caps, &spec, target.audio_samples)?;
        let conversion = FrameConversion {
            kind: plane_kind_for(
                target.preset.video.as_ref().map(|v| v.codec),
                target.preset.alpha,
            ),
            alpha: target.preset.alpha,
            colorimetry: resolved.colorimetry,
        };
        encoders.push((process, conversion));
    }
    let interrupts: Vec<_> = encoders
        .iter()
        .map(|(process, _)| process.interrupt_handle())
        .collect();
    let interrupt = || {
        for handle in &interrupts {
            handle.interrupt();
        }
    };
    let completed = drive_pipeline(
        total_frames,
        size,
        frame_source,
        cancel,
        interrupt,
        move |frames, progress| {
            for (index, frame) in frames {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(());
                }
                // Keep only one converted frame, reusing it for consecutive
                // targets with equal conversion semantics.
                let mut previous = None;
                let mut planes = None;
                for (process, conversion) in &mut encoders {
                    if previous != Some(*conversion) {
                        planes = Some(conversion.convert(&frame));
                        previous = Some(*conversion);
                    }
                    process.write_video_frame(planes.as_ref().expect("converted above"))?;
                }
                if progress.send(index + 1).is_err() {
                    return Ok(());
                }
            }
            if cancel.load(Ordering::Relaxed) {
                return Ok(());
            }
            for (process, _) in encoders {
                process.finish_with_cancel(cancel)?;
            }
            Ok(())
        },
        |progress| {
            for index in 0..output_count {
                on_event(index, ExportEvent::Progress(progress));
            }
        },
    )?;
    let event = if completed {
        ExportEvent::Done
    } else {
        ExportEvent::Cancelled
    };
    for index in 0..output_count {
        on_event(index, event.clone());
    }
    Ok(())
}

#[derive(Copy, Clone, PartialEq, Eq)]
struct FrameConversion {
    kind: PlaneKind,
    alpha: bool,
    colorimetry: Colorimetry,
}

impl FrameConversion {
    fn convert(self, frame: &Frame) -> convert::EncodePlanes {
        match self.kind {
            PlaneKind::Rgba8 => {
                convert::working_frame_to_rgba8(&frame.rgba_premult, frame.width, frame.height)
            }
            _ => convert::working_frame_to_yuv_planes(
                &frame.rgba_premult,
                frame.width,
                frame.height,
                self.colorimetry,
                self.alpha,
                self.kind == PlaneKind::Yuva444,
            ),
        }
    }
}

const PIPELINE_DEPTH: usize = 1;
const PIPELINE_POLL: Duration = Duration::from_millis(10);
type QueuedFrame = (u64, Frame);

/// The guard also interrupts on a caller/frame-source panic before the scoped
/// worker is joined; otherwise a full stdin pipe could deadlock unwinding.
struct InterruptOnDrop<F: Fn()>(F);
impl<F: Fn()> Drop for InterruptOnDrop<F> {
    fn drop(&mut self) {
        (self.0)();
    }
}

#[allow(clippy::too_many_arguments)]
fn drive_pipeline(
    total_frames: u64,
    size: (u32, u32),
    mut frame_source: impl FnMut(u64) -> Frame,
    cancel: &AtomicBool,
    interrupt: impl Fn(),
    worker: impl FnOnce(
            crossbeam_channel::Receiver<QueuedFrame>,
            crossbeam_channel::Sender<u64>,
        ) -> Result<(), ExportError>
        + Send,
    mut on_progress: impl FnMut(ExportProgress),
) -> Result<bool, ExportError> {
    let start = Instant::now();
    let mut report = |done| {
        let fps = done as f32 / start.elapsed().as_secs_f32().max(1e-6);
        on_progress(ExportProgress {
            frame: done,
            total: total_frames,
            fps,
            eta: Duration::from_secs_f32(total_frames.saturating_sub(done) as f32 / fps.max(1e-6)),
        });
    };
    std::thread::scope(|scope| {
        let (frame_tx, frame_rx) = crossbeam_channel::bounded(PIPELINE_DEPTH);
        // At most queued + consuming + producing frames can complete since
        // the last drain; acknowledgements are bounded without blocking writes.
        let (progress_tx, progress_rx) = crossbeam_channel::bounded(PIPELINE_DEPTH + 2);
        let join = scope.spawn(move || worker(frame_rx, progress_tx));
        let guard = InterruptOnDrop(interrupt);
        let mut source_error = None;
        'frames: for index in 0..total_frames {
            for done in progress_rx.try_iter() {
                report(done);
            }
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let frame = frame_source(index);
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if (frame.width, frame.height) != size {
                source_error = Some(ExportError::FrameSizeMismatch {
                    frame: index,
                    got_w: frame.width,
                    got_h: frame.height,
                    want_w: size.0,
                    want_h: size.1,
                });
                break;
            }
            let expected = size.0 as usize * size.1 as usize * 4;
            if frame.rgba_premult.len() != expected {
                source_error = Some(ExportError::FrameBufferSize {
                    frame: index,
                    got: frame.rgba_premult.len(),
                    expected,
                });
                break;
            }
            let mut pending = (index, frame);
            loop {
                match frame_tx.send_timeout(pending, PIPELINE_POLL) {
                    Ok(()) => break,
                    Err(crossbeam_channel::SendTimeoutError::Timeout(frame)) => pending = frame,
                    Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => break 'frames,
                }
                for done in progress_rx.try_iter() {
                    report(done);
                }
                if cancel.load(Ordering::Relaxed) {
                    break 'frames;
                }
            }
        }
        if source_error.is_some() || cancel.load(Ordering::Relaxed) {
            (guard.0)();
        }
        drop(frame_tx);
        loop {
            if cancel.load(Ordering::Relaxed) {
                (guard.0)();
            }
            match progress_rx.recv_timeout(PIPELINE_POLL) {
                Ok(done) => report(done),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        }
        let result = join.join().map_err(|_| ExportError::WorkerPanicked)?;
        if let Some(error) = source_error {
            return Err(error);
        }
        if cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        result?;
        Ok(true)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::presets::{
        AudioCodec, AudioEncodeSpec, Container, FrameRatePolicy, QualityMode, ResolutionSpec,
        VideoCodec, VideoEncodeSpec,
    };
    use crate::media::ffmpeg_locate::locate_for_test;

    macro_rules! tools_or_skip {
        () => {
            match locate_for_test() {
                Some(t) => t,
                None => {
                    eprintln!(
                        "ffmpeg/ffprobe not found — skipping export render_loop test at {}:{}",
                        file!(),
                        line!()
                    );
                    return;
                }
            }
        };
    }

    /// A moving horizontal gradient, premultiplied straight (alpha 1.0) —
    /// deterministic per-frame content so distinct frames are distinguishable.
    fn synthetic_gradient_frame(i: u64, w: u32, h: u32) -> Frame {
        let mut rgba = vec![0.0f32; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let idx = ((y * w + x) * 4) as usize;
                let t = ((x as u64 + i * 3) % w as u64) as f32 / w.max(1) as f32;
                rgba[idx] = t; // R ramps + shifts per frame
                rgba[idx + 1] = 1.0 - t;
                rgba[idx + 2] = 0.5;
                rgba[idx + 3] = 1.0;
            }
        }
        Frame {
            width: w,
            height: h,
            rgba_premult: rgba,
        }
    }

    fn tiny_preset() -> ExportPreset {
        ExportPreset {
            name: "render-loop-test".into(),
            container: Container::Mp4,
            video: Some(VideoEncodeSpec {
                codec: VideoCodec::H264,
                quality: QualityMode::Crf(28.0),
            }),
            audio: Some(AudioEncodeSpec {
                codec: AudioCodec::Aac,
                bitrate_kbps: Some(96),
            }),
            resolution: ResolutionSpec::SourceFormat,
            frame_rate: FrameRatePolicy::MatchSequence,
            alpha: false,
            faststart: false,
            loudness_target: None,
            stems: false,
        }
    }

    fn tmp_out(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "photonic-render-loop-test-{}-{name}",
            std::process::id()
        ))
    }

    #[test]
    fn export_frames_emits_monotonic_progress_then_done_and_writes_output() {
        let tools = tools_or_skip!();
        let preset = tiny_preset();
        let out_path = tmp_out("progress.mp4");
        let _ = std::fs::remove_file(&out_path);
        let resolved = ResolvedExport {
            width: 16,
            height: 16,
            frame_rate: FrameRate::new(10, 1),
            audio: Some(AudioStreamSpec {
                sample_rate: 48_000,
                channels: 2,
            }),
            out_path: out_path.clone(),
            colorimetry: Colorimetry::BT709_LIMITED,
            prefer_hardware: false,
            encoder_speed: None,
            raw_encoder_args: vec![],
            burn_in_timecode: false,
            two_pass: false,
        };
        let audio = vec![0.0f32; 48_000 / 10 * 2 * 5]; // 5 frames' worth of silence
        let cancel = AtomicBool::new(false);
        let mut events = Vec::new();

        export_frames(
            &tools,
            &preset,
            &resolved,
            5,
            |i| synthetic_gradient_frame(i, 16, 16),
            Some(audio),
            &cancel,
            |e| events.push(e),
        )
        .expect("export succeeds");

        let progress: Vec<u64> = events
            .iter()
            .filter_map(|e| match e {
                ExportEvent::Progress(p) => Some(p.frame),
                _ => None,
            })
            .collect();
        assert_eq!(
            progress,
            vec![1, 2, 3, 4, 5],
            "progress frames are monotonic 1..=total"
        );
        assert_eq!(events.last(), Some(&ExportEvent::Done));
        assert!(out_path.exists(), "output file was written");
        assert!(std::fs::metadata(&out_path).unwrap().len() > 0);

        let _ = std::fs::remove_file(&out_path);
    }

    #[test]
    fn export_frames_stops_and_reports_cancelled_when_flag_is_set() {
        let tools = tools_or_skip!();
        let preset = tiny_preset();
        let out_path = tmp_out("cancel.mp4");
        let _ = std::fs::remove_file(&out_path);
        let resolved = ResolvedExport {
            width: 16,
            height: 16,
            frame_rate: FrameRate::new(10, 1),
            audio: None,
            out_path: out_path.clone(),
            colorimetry: Colorimetry::BT709_LIMITED,
            prefer_hardware: false,
            encoder_speed: None,
            raw_encoder_args: vec![],
            burn_in_timecode: false,
            two_pass: false,
        };
        let mut preset = preset;
        preset.audio = None;
        let cancel = AtomicBool::new(true); // cancel before the first frame
        let mut events = Vec::new();

        export_frames(
            &tools,
            &preset,
            &resolved,
            100,
            |i| synthetic_gradient_frame(i, 16, 16),
            None,
            &cancel,
            |e| events.push(e),
        )
        .expect("cancel is not an error");

        assert_eq!(events, vec![ExportEvent::Cancelled]);
        let _ = std::fs::remove_file(&out_path);
    }

    #[test]
    fn export_frames_rejects_a_frame_size_mismatch() {
        let tools = tools_or_skip!();
        let mut preset = tiny_preset();
        preset.audio = None;
        let out_path = tmp_out("mismatch.mp4");
        let _ = std::fs::remove_file(&out_path);
        let resolved = ResolvedExport {
            width: 16,
            height: 16,
            frame_rate: FrameRate::new(10, 1),
            audio: None,
            out_path: out_path.clone(),
            colorimetry: Colorimetry::BT709_LIMITED,
            prefer_hardware: false,
            encoder_speed: None,
            raw_encoder_args: vec![],
            burn_in_timecode: false,
            two_pass: false,
        };
        let cancel = AtomicBool::new(false);

        let result = export_frames(
            &tools,
            &preset,
            &resolved,
            3,
            |i| synthetic_gradient_frame(i, 8, 8), // wrong size on purpose
            None,
            &cancel,
            |_| {},
        );

        assert!(matches!(
            result,
            Err(ExportError::FrameSizeMismatch { frame: 0, .. })
        ));
        let _ = std::fs::remove_file(&out_path);
    }

    #[test]
    fn pipeline_overlaps_source_and_consumer_with_three_frame_bound() {
        use std::sync::atomic::AtomicU64;
        let produced = AtomicU64::new(0);
        let consumed = AtomicU64::new(0);
        let cancel = AtomicBool::new(false);
        let (started_tx, started_rx) = crossbeam_channel::bounded(1);
        let (release_tx, release_rx) = crossbeam_channel::bounded(1);
        let mut progress = Vec::new();
        let completed = drive_pipeline(
            12,
            (1, 1),
            |index| {
                let outstanding =
                    produced.fetch_add(1, Ordering::SeqCst) + 1 - consumed.load(Ordering::SeqCst);
                assert!(
                    outstanding <= 3,
                    "unbounded render lookahead: {outstanding}"
                );
                if index == 1 {
                    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                }
                if index == 2 {
                    release_tx.send(()).unwrap();
                }
                synthetic_gradient_frame(index, 1, 1)
            },
            &cancel,
            || {},
            |frames, progress| {
                for (expected, (index, _)) in frames.into_iter().enumerate() {
                    assert_eq!(index, expected as u64);
                    if index == 0 {
                        started_tx.send(()).unwrap();
                        // The producer must create frames 1 and 2 while this
                        // consumer is working on frame 0.
                        release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                    }
                    consumed.fetch_add(1, Ordering::SeqCst);
                    progress.send(index + 1).unwrap();
                }
                Ok(())
            },
            |event| progress.push(event.frame),
        )
        .unwrap();
        assert!(completed);
        assert_eq!(progress, (1..=12).collect::<Vec<_>>());
    }

    #[test]
    fn pipeline_cancellation_interrupts_backpressure_and_joins_worker() {
        use std::sync::atomic::AtomicUsize;
        let produced = AtomicUsize::new(0);
        let cancel = AtomicBool::new(false);
        let interrupted = AtomicBool::new(false);
        let joined = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let deadline = Instant::now() + Duration::from_secs(2);
                while produced.load(Ordering::Relaxed) < 3 && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(1));
                }
                cancel.store(true, Ordering::Relaxed);
            });
            let completed = drive_pipeline(
                100,
                (1, 1),
                |index| {
                    produced.fetch_add(1, Ordering::Relaxed);
                    synthetic_gradient_frame(index, 1, 1)
                },
                &cancel,
                || interrupted.store(true, Ordering::Relaxed),
                |frames, _| {
                    let _first = frames.recv().unwrap();
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while !interrupted.load(Ordering::Relaxed) && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    assert!(interrupted.load(Ordering::Relaxed));
                    joined.store(true, Ordering::Relaxed);
                    Ok(())
                },
                |_| {},
            )
            .unwrap();
            assert!(!completed);
        });
        assert!(joined.load(Ordering::Relaxed));
        assert!(produced.load(Ordering::Relaxed) <= 3);
    }

    #[test]
    fn two_pass_rejected_before_probing_or_rendering() {
        let missing = FfmpegTools {
            ffmpeg: PathBuf::from("/missing/ffmpeg"),
            ffprobe: PathBuf::from("/missing/ffprobe"),
        };
        let preset = tiny_preset();
        let mut resolved = ResolvedExport::basic(
            16,
            16,
            FrameRate::FPS_30,
            None,
            tmp_out("two-pass.mp4"),
            Colorimetry::BT709_LIMITED,
        );
        resolved.two_pass = true;
        let result = export_frames(
            &missing,
            &preset,
            &resolved,
            1,
            |_| panic!("must not render"),
            None,
            &AtomicBool::new(false),
            |_| {},
        );
        assert!(matches!(
            result,
            Err(ExportError::Encode(EncodeError::TwoPassUnsupported))
        ));
    }

    #[test]
    fn shared_export_renders_once_and_preserves_distinct_alpha_and_codec_outputs() {
        let tools = tools_or_skip!();
        let directory =
            std::env::temp_dir().join(format!("photonic-shared-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let mut mp4 = tiny_preset();
        mp4.audio = None;
        let mut png = mp4.clone();
        png.container = Container::ImageSequence;
        png.video = Some(VideoEncodeSpec {
            codec: VideoCodec::Png,
            quality: QualityMode::Lossless,
        });
        png.alpha = true;
        let mp4_out = ResolvedExport::basic(
            16,
            16,
            FrameRate::new(10, 1),
            None,
            directory.join("movie.mp4"),
            Colorimetry::BT709_LIMITED,
        );
        let png_out = ResolvedExport::basic(
            16,
            16,
            FrameRate::new(10, 1),
            None,
            directory.join("image-%03d.png"),
            Colorimetry::BT709_LIMITED,
        );
        let mut calls = 0;
        let mut events = Vec::new();
        let frame = Frame {
            width: 16,
            height: 16,
            rgba_premult: [0.125, 0.25, 0.375, 0.5].repeat(256),
        };
        export_frames_multi(
            &tools,
            vec![
                ExportTarget {
                    preset: &mp4,
                    resolved: &mp4_out,
                    audio_samples: None,
                },
                ExportTarget {
                    preset: &png,
                    resolved: &png_out,
                    audio_samples: None,
                },
            ],
            5,
            |_| {
                calls += 1;
                frame.clone()
            },
            &AtomicBool::new(false),
            |index, event| events.push((index, event)),
        )
        .unwrap();
        assert_eq!(
            calls, 5,
            "one source render per output tick, independent of output count"
        );
        for index in 0..2 {
            assert_eq!(
                events
                    .iter()
                    .filter(|(i, event)| *i == index && matches!(event, ExportEvent::Progress(_)))
                    .count(),
                5
            );
            assert!(events.contains(&(index, ExportEvent::Done)));
        }
        let decoded = image::open(directory.join("image-001.png"))
            .unwrap()
            .to_rgba8();
        assert_eq!(
            decoded.into_raw(),
            convert::working_frame_to_rgba8(&frame.rgba_premult, 16, 16).to_bytes()
        );
        let probe = std::process::Command::new(&tools.ffprobe)
            .args(["-v", "error", "-show_streams", "-of", "json"])
            .arg(&mp4_out.out_path)
            .output()
            .unwrap();
        assert!(probe.status.success());
        let probe: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
        let video = &probe["streams"][0];
        assert_eq!(video["codec_name"], "h264");
        assert_eq!(video["width"], 16);
        assert_eq!(video["height"], 16);
        assert_eq!(video["nb_frames"], "5");
        assert_eq!(video["color_space"], "bt709");
        assert!(!std::fs::read_dir(&directory).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".photonic-export")));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_frame_keeps_previous_destination_and_removes_staged_output() {
        let tools = tools_or_skip!();
        let directory =
            std::env::temp_dir().join(format!("photonic-failed-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let out = directory.join("previous.mp4");
        std::fs::write(&out, b"previous complete output").unwrap();
        let mut preset = tiny_preset();
        preset.audio = None;
        let resolved = ResolvedExport::basic(
            16,
            16,
            FrameRate::FPS_30,
            None,
            out.clone(),
            Colorimetry::BT709_LIMITED,
        );
        let result = export_frames(
            &tools,
            &preset,
            &resolved,
            3,
            |i| {
                if i == 0 {
                    synthetic_gradient_frame(i, 16, 16)
                } else {
                    Frame {
                        width: 16,
                        height: 16,
                        rgba_premult: Vec::new(),
                    }
                }
            },
            None,
            &AtomicBool::new(false),
            |_| {},
        );
        assert!(matches!(
            result,
            Err(ExportError::FrameBufferSize { frame: 1, .. })
        ));
        assert_eq!(std::fs::read(&out).unwrap(), b"previous complete output");
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
