//! FFmpeg **encode** sidecar (02-engine.md §7, 05-import-export.md §3.5/§3.7).
//!
//! ## Two piped inputs: video (stdin) + audio (second input)
//!
//! 02 §7 asks for rawvideo on stdin plus "audio mixed offline (09), piped as
//! f32le on a second input." A single process has exactly one stdin, so a
//! *second* live/pipe input cannot also be `pipe:0` — ffmpeg has no
//! multi-stream stdin demux.
//!
//! Platform strategy (24-preview-media-load / Windows export path):
//! - **Unix:** audio via a **named FIFO** (`mkfifo` / `libc::mkfifo`). A
//!   background thread opens and writes with nonblocking I/O, checking its
//!   stop flag while FFmpeg has not opened the reader or the pipe is full.
//!   The thread is joined before the FIFO is unlinked.
//! - **Windows (and any non-unix):** audio is written to a **temp f32le file**
//!   *before* ffmpeg is spawned, then passed as a second `-i` path. The temp
//!   file is deleted on `finish`/`cancel`/drop. Same ffmpeg arg shape; no
//!   concurrent open race.
//!
//! Audio is mixed offline in full before export starts (09's mixer). Writers
//! pack it in fixed-size chunks, avoiding a second whole-track byte buffer.
//!
//! ## Encoder selection (D-03, §3.4, §3.7)
//!
//! [`EncoderCapabilities::probe`] runs `ffmpeg -encoders` once and records
//! which named encoders exist, so codec choice adapts to whatever ffmpeg the
//! caller pointed at (§3.7's bring-your-own-ffmpeg escape hatch) rather than
//! assuming a fixed build:
//! - **H.264**: `libopenh264` when present (the LGPL build Photonic ships,
//!   D-03), else `libx264` (GPL) — the fallback is exercised **only** in
//!   local/CI test runs against the operator's system ffmpeg, never in the
//!   shipped binary, which always carries `libopenh264`. This workstation's
//!   ffmpeg has no `libopenh264` build, so the fallback path is exactly what
//!   every test here exercises; the `libopenh264` branch's CRF→bitrate
//!   translation ([`crf_to_kbps_heuristic`]) is a best-effort/unverified
//!   implementation flagged for a real check once a libopenh264 build is
//!   available (dev finding, reported alongside this module).
//! - **AV1**: `libsvtav1` when present (§3.5's pick — best unencumbered
//!   speed/quality), else `librav1e`. This workstation has both; the SVT-AV1
//!   path is exercised by tests, the rav1e fallback only by a unit test on
//!   the selection logic (not a real encode).
//! - **VP9 alpha**: `libvpx-vp9` rejects `yuva444p` ("not widely supported")
//!   at this build's default strictness — confirmed empirically — so alpha
//!   VP9 uses `yuva420p` (full-res alpha plane, 4:2:0 chroma) plus
//!   `-auto-alt-ref 0` (the standard recipe for VP9-alpha-in-WebM).
//! - **ProRes 4444**: `prores_ks -profile:v 4`. `prores_ks` only accepts
//!   10/12-bit pixel formats (`yuv444p10le`/`yuva444p10le` etc, no 8-bit);
//!   feeding it our 8-bit `yuva444p` rawvideo and *not* forcing an output
//!   `-pix_fmt` lets ffmpeg's implicit format-negotiation upconvert
//!   automatically (confirmed empirically — no explicit `-pix_fmt`/`-vf
//!   format=` needed on the output side).
//! - **Color tagging**: `-color_primaries/-color_trc/-colorspace bt709
//!   -color_range tv` alone only reliably sets *container*-level tags for
//!   some encoders (confirmed: `libx264` alone left `color_transfer`/
//!   `color_primaries` "unknown" via `ffprobe`); pairing it with
//!   `-vf setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv`
//!   makes the encoder itself write the tags into its bitstream (confirmed:
//!   all four fields then read back correctly). Applied to the YUV-family
//!   containers only (Mp4/Mov/WebM/Mkv) — GIF has no such metadata, and
//!   PNG/APNG's sRGB convention doesn't use it (§6.1's carve-out).

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use photonic_core::timeline::FrameRate;

use super::convert::EncodePlanes;
use super::presets::{
    AudioCodec, AudioEncodeSpec, Container, ExportPreset, QualityMode, VideoCodec,
};
use crate::media::ffmpeg_locate::FfmpegTools;

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("failed to spawn ffmpeg encoder: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("failed to probe ffmpeg encoder capabilities: {0}")]
    Probe(#[source] std::io::Error),
    #[error("io error: {0}")]
    Io(#[source] std::io::Error),
    #[error("invalid audio sidecar path (non-UTF8 / contains NUL)")]
    InvalidFifoPath,
    #[error("audio writer thread panicked")]
    AudioWriterPanicked,
    #[error("encoder stderr reader thread panicked")]
    StderrReaderPanicked,
    #[error("export cancelled")]
    Cancelled,
    #[error("two-pass encoding is not supported by the streaming export path; disable two_pass")]
    TwoPassUnsupported,
    #[error("encoder exited with status {status:?}; stderr tail:\n{stderr}")]
    EncoderExited { status: Option<i32>, stderr: String },
    /// K-F5 fail-closed: hardware preferred but no matching encoder was probed.
    #[error(
        "hardware encoder requested for {codec:?} but none was detected in this \
         ffmpeg build (probed: {probed}). Availability is never inferred (23 §10.3)."
    )]
    HardwareUnavailable { codec: VideoCodec, probed: String },
}

// ── Encoder capability probing (§3.4/§3.7) ───────────────────────────────────

/// Which named encoders/muxers this ffmpeg build has, from one
/// `ffmpeg -encoders` invocation.
#[derive(Clone, Debug)]
pub struct EncoderCapabilities {
    names: HashSet<String>,
}

impl EncoderCapabilities {
    pub fn probe(tools: &FfmpegTools) -> Result<Self, EncodeError> {
        let output = Command::new(&tools.ffmpeg)
            .args(["-hide_banner", "-encoders"])
            .output()
            .map_err(EncodeError::Probe)?;
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(Self::parse(&text))
    }

    /// Parse `ffmpeg -encoders` output. Each encoder line looks like
    /// `<flags> <name>  <long name>` (flags is a fixed-width 6-char field,
    /// `ffmpeg -encoders`'s documented format) — the name is the first
    /// whitespace-delimited token after the flags column.
    fn parse(text: &str) -> Self {
        let mut names = HashSet::new();
        for line in text.lines() {
            let trimmed = line.trim_start();
            // Encoder lines start with a flags column like "V....D" / "A....D";
            // header/blank lines don't. Skip anything that doesn't match.
            let mut parts = trimmed.splitn(3, char::is_whitespace);
            let flags = match parts.next() {
                Some(f) if f.len() == 6 && f.starts_with(['V', 'A', 'S']) => f,
                _ => continue,
            };
            let _ = flags;
            if let Some(name) = trimmed.split_whitespace().nth(1) {
                names.insert(name.to_string());
            }
        }
        EncoderCapabilities { names }
    }

    pub fn has(&self, encoder_name: &str) -> bool {
        self.names.contains(encoder_name)
    }

    /// H.264: `libopenh264` (shipped LGPL build, D-03) else `libx264` (dev/CI
    /// fallback only — see module docs).
    pub fn h264_encoder(&self) -> &'static str {
        if self.has("libopenh264") {
            "libopenh264"
        } else {
            "libx264"
        }
    }

    /// AV1: `libsvtav1` (§3.5's pick) else `librav1e`.
    pub fn av1_encoder(&self) -> &'static str {
        if self.has("libsvtav1") {
            "libsvtav1"
        } else {
            "librav1e"
        }
    }

    /// Hardware encoder names Photonic recognises (K-F5 / 26 §14).
    pub const HW_ENCODER_CANDIDATES: &'static [&'static str] = &[
        "h264_nvenc",
        "hevc_nvenc",
        "av1_nvenc",
        "h264_vaapi",
        "hevc_vaapi",
        "av1_vaapi",
        "h264_videotoolbox",
        "hevc_videotoolbox",
        "h264_qsv",
        "hevc_qsv",
        "av1_qsv",
    ];

    /// Probed hardware encoders present in this ffmpeg build (K-F5).
    /// Availability is never inferred — only names that appear in
    /// `ffmpeg -encoders` are returned (23 §10.3 fail-closed).
    pub fn hardware_encoders(&self) -> Vec<&'static str> {
        Self::HW_ENCODER_CANDIDATES
            .iter()
            .copied()
            .filter(|n| self.has(n))
            .collect()
    }

    /// Resolve a hardware encoder for `codec` when `prefer_hardware` is set.
    /// Returns `None` when no HW twin is probed — callers must fail closed
    /// rather than silently falling back to software (K-F5).
    pub fn hardware_for(&self, codec: VideoCodec) -> Option<&'static str> {
        let candidates: &[&str] = match codec {
            VideoCodec::H264 => &["h264_nvenc", "h264_vaapi", "h264_videotoolbox", "h264_qsv"],
            VideoCodec::Av1 => &["av1_nvenc", "av1_vaapi", "av1_qsv"],
            // VP9/ProRes/GIF/PNG have no HW profiles we advertise.
            _ => &[],
        };
        candidates.iter().copied().find(|n| self.has(n))
    }

    /// Human-readable detection report for the export dialog (K-F5 honesty).
    pub fn detection_report(&self) -> String {
        let hw = self.hardware_encoders();
        let soft = [
            ("H.264", self.h264_encoder()),
            ("AV1", self.av1_encoder()),
            (
                "VP9",
                if self.has("libvpx-vp9") {
                    "libvpx-vp9"
                } else {
                    "(missing)"
                },
            ),
        ];
        let mut lines = Vec::new();
        lines.push("Software:".to_string());
        for (label, name) in soft {
            lines.push(format!("  {label}: {name}"));
        }
        if hw.is_empty() {
            lines.push("Hardware: (none detected)".into());
        } else {
            lines.push(format!("Hardware: {}", hw.join(", ")));
        }
        lines.join("\n")
    }
}

// ── Plane-shape selection (which convert.rs function to feed) ───────────────

/// Which raw pixel layout a codec expects, driving both the ffmpeg input
/// `-pix_fmt` declaration and which `convert::working_frame_to_*` function
/// `render_loop` must call for a given frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PlaneKind {
    Yuv420,
    Yuva420,
    Yuva444,
    Rgba8,
}

impl PlaneKind {
    pub fn ffmpeg_pix_fmt(self) -> &'static str {
        match self {
            PlaneKind::Yuv420 => "yuv420p",
            PlaneKind::Yuva420 => "yuva420p",
            PlaneKind::Yuva444 => "yuva444p",
            PlaneKind::Rgba8 => "rgba",
        }
    }
}

/// §3.4's allow-list, restated as a plane-shape choice: PNG/APNG are RGB, VP9
/// alpha is 4:2:0 (broad real-world decoder compatibility — `yuva444p` is
/// rejected by `libvpx-vp9` at default strictness), ProRes 4444 is 4:4:4.
pub fn plane_kind_for(codec: Option<VideoCodec>, alpha: bool) -> PlaneKind {
    match codec {
        Some(VideoCodec::Png) | Some(VideoCodec::Apng) => PlaneKind::Rgba8,
        Some(VideoCodec::ProResLikeMezzanine) if alpha => PlaneKind::Yuva444,
        Some(VideoCodec::Vp9) if alpha => PlaneKind::Yuva420,
        _ if alpha => PlaneKind::Yuva444, // not reachable via `validate`'s allow-list; safe default
        _ => PlaneKind::Yuv420,
    }
}

// ── ffmpeg arg building (pure — testable without spawning ffmpeg) ───────────

/// Resolved (post-`ResolutionSpec`/`FrameRatePolicy`) target audio format.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AudioStreamSpec {
    pub sample_rate: u32,
    pub channels: u16,
}

/// Everything [`build_ffmpeg_args`]/[`EncoderProcess::spawn`] need, already
/// resolved from an [`ExportPreset`]'s abstract `ResolutionSpec`/
/// `FrameRatePolicy` down to concrete numbers — that resolution is the
/// caller's (render_loop/the evaluator's) job, not this module's.
pub struct EncodeSpec<'a> {
    pub preset: &'a ExportPreset,
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
    pub audio: Option<AudioStreamSpec>,
    pub out_path: PathBuf,
    /// K-F5: prefer a hardware encoder; fail closed if none is probed.
    pub prefer_hardware: bool,
    /// K-F4: optional ffmpeg `-preset` speed string.
    pub encoder_speed: Option<&'a str>,
    /// K-F5: free-form extra args appended after codec selection.
    pub raw_encoder_args: &'a [String],
    /// K-F polish: burn-in drawtext filter.
    pub burn_in_timecode: bool,
    /// Two-pass requests are rejected before staging inputs or spawning FFmpeg.
    pub two_pass: bool,
}

/// Validate options that cannot be fulfilled by the streaming encoder. Call
/// before capability probing, audio mixing, rendering, or file creation.
pub fn validate_encode_options(two_pass: bool) -> Result<(), EncodeError> {
    if two_pass {
        return Err(EncodeError::TwoPassUnsupported);
    }
    Ok(())
}

/// Best-effort CRF→bitrate translation for encoders without true CRF-mode
/// rate control (currently only the `libopenh264` branch — **unverified**,
/// see module docs: no `libopenh264` build is available to test against
/// here). Follows the common x264 rule of thumb that perceptual bitrate
/// roughly halves every +6 CRF, anchored at 1080p/CRF23≈4000kbps and scaled
/// by pixel count.
fn crf_to_kbps_heuristic(crf: f32, width: u32, height: u32) -> u32 {
    const BASE_KBPS_1080P_CRF23: f32 = 4000.0;
    let scale = (width as f32 * height as f32) / (1920.0 * 1080.0);
    let factor = 2f32.powf((23.0 - crf) / 6.0);
    (BASE_KBPS_1080P_CRF23 * scale * factor).clamp(200.0, 50_000.0) as u32
}

fn push_video_codec_args(
    args: &mut Vec<String>,
    caps: &EncoderCapabilities,
    codec: VideoCodec,
    quality: QualityMode,
    alpha: bool,
    width: u32,
    height: u32,
    prefer_hardware: bool,
    encoder_speed: Option<&str>,
) -> Result<(), EncodeError> {
    // K-F5: when hardware is preferred, resolve a probed HW encoder or fail
    // closed — never silently fall back to software.
    if prefer_hardware {
        let Some(hw) = caps.hardware_for(codec) else {
            let probed = caps.hardware_encoders().join(", ");
            return Err(EncodeError::HardwareUnavailable {
                codec,
                probed: if probed.is_empty() {
                    "(none)".into()
                } else {
                    probed
                },
            });
        };
        args.extend(["-c:v".into(), hw.into()]);
        // Hardware encoders take bitrate-style RC more reliably than CRF.
        match quality {
            QualityMode::Crf(crf) => {
                let kbps = crf_to_kbps_heuristic(crf, width, height);
                args.extend(["-b:v".into(), format!("{kbps}k")]);
            }
            QualityMode::Bitrate {
                target_kbps,
                max_kbps,
            } => {
                args.extend([
                    "-b:v".into(),
                    format!("{target_kbps}k"),
                    "-maxrate".into(),
                    format!("{max_kbps}k"),
                ]);
            }
            QualityMode::Lossless => {
                args.extend(["-b:v".into(), "0".into()]);
            }
        }
        if let Some(speed) = encoder_speed {
            args.extend(["-preset".into(), speed.into()]);
        }
        return Ok(());
    }

    match codec {
        VideoCodec::H264 => {
            let enc = caps.h264_encoder();
            args.extend(["-c:v".into(), enc.into()]);
            let is_libx264 = enc == "libx264";
            if let Some(speed) = encoder_speed {
                if is_libx264 {
                    args.extend(["-preset".into(), speed.into()]);
                }
            }
            match quality {
                QualityMode::Crf(crf) if is_libx264 => {
                    args.extend(["-crf".into(), crf.to_string()]);
                }
                QualityMode::Crf(crf) => {
                    let kbps = crf_to_kbps_heuristic(crf, width, height);
                    args.extend(["-b:v".into(), format!("{kbps}k")]);
                }
                QualityMode::Bitrate {
                    target_kbps,
                    max_kbps,
                } => {
                    args.extend([
                        "-b:v".into(),
                        format!("{target_kbps}k"),
                        "-maxrate".into(),
                        format!("{max_kbps}k"),
                        "-bufsize".into(),
                        format!("{}k", max_kbps.saturating_mul(2)),
                    ]);
                }
                QualityMode::Lossless if is_libx264 => {
                    args.extend(["-crf".into(), "0".into()]);
                }
                QualityMode::Lossless => {
                    args.extend(["-b:v".into(), "0".into()]);
                }
            }
        }
        VideoCodec::Av1 => {
            let enc = caps.av1_encoder();
            args.extend(["-c:v".into(), enc.into()]);
            if enc == "libsvtav1" {
                let speed = encoder_speed.unwrap_or("4");
                args.extend(["-preset".into(), speed.into()]); // §3.5: "preset speed 4"
                let crf = match quality {
                    QualityMode::Crf(v) => v,
                    QualityMode::Bitrate { .. } => {
                        // libsvtav1 also accepts -b:v directly; CRF path is what
                        // the catalog uses, Bitrate mode just passes through.
                        if let QualityMode::Bitrate {
                            target_kbps,
                            max_kbps,
                        } = quality
                        {
                            args.extend([
                                "-b:v".into(),
                                format!("{target_kbps}k"),
                                "-maxrate".into(),
                                format!("{max_kbps}k"),
                            ]);
                        }
                        return Ok(());
                    }
                    QualityMode::Lossless => 0.0,
                };
                args.extend(["-crf".into(), (crf.round() as i32).clamp(0, 63).to_string()]);
            } else {
                // librav1e fallback: `-qp` (0..255, lower = better) is not the
                // same scale as SVT-AV1's CRF (0..63) — documented approximate
                // mapping (x4), not a claimed perceptual equivalence.
                args.extend(["-speed".into(), "6".into()]);
                let qp = match quality {
                    QualityMode::Crf(v) => (v * 4.0).round() as i32,
                    QualityMode::Bitrate { target_kbps, .. } => {
                        args.extend(["-b:v".into(), format!("{target_kbps}k")]);
                        return Ok(());
                    }
                    QualityMode::Lossless => 0,
                };
                args.extend(["-qp".into(), qp.clamp(0, 255).to_string()]);
            }
        }
        VideoCodec::Vp9 => {
            args.extend(["-c:v".into(), "libvpx-vp9".into()]);
            match quality {
                QualityMode::Crf(v) => {
                    args.extend([
                        "-crf".into(),
                        (v.round() as i32).clamp(0, 63).to_string(),
                        // libvpx-vp9 CRF mode requires -b:v 0, else it runs
                        // constrained-quality instead of true constant-quality.
                        "-b:v".into(),
                        "0".into(),
                    ]);
                }
                QualityMode::Bitrate {
                    target_kbps,
                    max_kbps,
                } => {
                    args.extend([
                        "-b:v".into(),
                        format!("{target_kbps}k"),
                        "-maxrate".into(),
                        format!("{max_kbps}k"),
                    ]);
                }
                QualityMode::Lossless => {
                    args.extend(["-lossless".into(), "1".into()]);
                }
            }
            if alpha {
                // Standard VP9-alpha-in-WebM recipe: alt-ref frames don't
                // interact well with the alpha side-channel.
                args.extend(["-auto-alt-ref".into(), "0".into()]);
            }
        }
        VideoCodec::ProResLikeMezzanine => {
            // profile 4 == "4444" (ffmpeg's -profile enum for prores_ks).
            args.extend([
                "-c:v".into(),
                "prores_ks".into(),
                "-profile:v".into(),
                "4".into(),
            ]);
        }
        VideoCodec::Gif => {
            // High-quality paletted GIF: generate a palette from the stream,
            // then dither against it (standard ffmpeg recipe). This replaces
            // the color-tagging `-vf` entirely for GIF outputs (mutually
            // exclusive with `setparams` on the same output stream) — the
            // caller skips the color-tag block for `VideoCodec::Gif`.
            //
            // The graph's first pad is labeled `[0:v]` (not left unlabeled): an
            // unlabeled `filter_complex` input pad is auto-fed from an *unused*
            // input stream, which the caller's explicit `-map 0:v` would have
            // already consumed ("Cannot find an unused video input stream to
            // feed the unlabeled input pad split"). The caller therefore also
            // omits `-map 0:v` for GIF and lets `paletteuse`'s unlabeled output
            // auto-map to the output file.
            args.extend([
                "-filter_complex".into(),
                "[0:v]split[s0][s1];[s0]palettegen[p];[s1][p]paletteuse=dither=bayer".into(),
            ]);
        }
        VideoCodec::Png => {
            args.extend(["-c:v".into(), "png".into()]);
        }
        VideoCodec::Apng => {
            // `-plays 0` = loop forever (APNG's standard "loop" convention).
            args.extend(["-c:v".into(), "apng".into(), "-plays".into(), "0".into()]);
        }
    }
    Ok(())
}

fn push_audio_codec_args(args: &mut Vec<String>, audio: &AudioEncodeSpec) {
    match audio.codec {
        AudioCodec::Aac => {
            args.extend(["-c:a".into(), "aac".into()]);
            if let Some(kbps) = audio.bitrate_kbps {
                args.extend(["-b:a".into(), format!("{kbps}k")]);
            }
        }
        AudioCodec::Opus => {
            args.extend(["-c:a".into(), "libopus".into()]);
            if let Some(kbps) = audio.bitrate_kbps {
                args.extend(["-b:a".into(), format!("{kbps}k")]);
            }
        }
        AudioCodec::Pcm => {
            args.extend(["-c:a".into(), "pcm_s16le".into()]);
        }
    }
}

/// Whether `container` carries the kind of stream-level color metadata the
/// `setparams`/`-color_*` tagging block applies to (§6.1).
fn container_supports_color_tags(container: Container) -> bool {
    matches!(
        container,
        Container::Mp4 | Container::Mov | Container::WebM | Container::Mkv
    )
}

/// Build the full ffmpeg argument list for one export encode. Pure/testable
/// without spawning a process — [`EncoderProcess::spawn`] is the only caller
/// that actually runs it. Returns [`EncodeError::HardwareUnavailable`] when
/// hardware is preferred but not probed (K-F5 fail-closed).
pub fn build_ffmpeg_args(
    caps: &EncoderCapabilities,
    spec: &EncodeSpec,
    video_pix_fmt: &str,
    audio_fifo: Option<&Path>,
) -> Result<Vec<String>, EncodeError> {
    validate_encode_options(spec.two_pass)?;
    let mut args = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-y".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
    ];

    let fr = format!("{}/{}", spec.frame_rate.num, spec.frame_rate.den);
    args.extend([
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        video_pix_fmt.into(),
        "-s".into(),
        format!("{}x{}", spec.width, spec.height),
        "-r".into(),
        fr,
        "-i".into(),
        "pipe:0".into(),
    ]);

    let has_audio = audio_fifo.is_some() && spec.audio.is_some() && spec.preset.audio.is_some();
    if let (Some(fifo), Some(a)) = (audio_fifo, spec.audio.as_ref()) {
        if spec.preset.audio.is_some() {
            args.extend([
                "-f".into(),
                "f32le".into(),
                "-ar".into(),
                a.sample_rate.to_string(),
                "-ac".into(),
                a.channels.to_string(),
                "-i".into(),
                fifo.to_string_lossy().into_owned(),
            ]);
        }
    }

    // GIF drives its video through a `filter_complex` whose `paletteuse`
    // output auto-maps; an explicit `-map 0:v` there would both double-map and
    // starve the filtergraph's `[0:v]` input pad (see the `VideoCodec::Gif`
    // arm). Every other codec maps the raw video stream directly.
    let is_gif = matches!(
        spec.preset.video.as_ref().map(|v| v.codec),
        Some(VideoCodec::Gif)
    );
    if !is_gif {
        args.extend(["-map".into(), "0:v".into()]);
    }
    if has_audio {
        args.extend(["-map".into(), "1:a".into()]);
    }

    match &spec.preset.video {
        Some(v) => push_video_codec_args(
            &mut args,
            caps,
            v.codec,
            v.quality,
            spec.preset.alpha,
            spec.width,
            spec.height,
            spec.prefer_hardware,
            spec.encoder_speed,
        )?,
        None => args.push("-vn".into()),
    }

    // K-F5 free-form escape hatch — append after codec selection.
    for raw in spec.raw_encoder_args {
        if !raw.is_empty() {
            args.push(raw.clone());
        }
    }

    if has_audio {
        if let Some(a) = &spec.preset.audio {
            push_audio_codec_args(&mut args, a);
        }
    } else {
        args.push("-an".into());
    }

    // Colour tags + optional burn-in (K-F polish) share one `-vf` chain.
    if !is_gif {
        let mut vf = String::new();
        if container_supports_color_tags(spec.preset.container) {
            vf.push_str(
                "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv",
            );
        }
        if spec.burn_in_timecode {
            if !vf.is_empty() {
                vf.push(',');
            }
            // drawtext: frame number + timecode-style counter; font fallback
            // is ffmpeg's default. Missing drawtext is a soft fail at runtime.
            vf.push_str(
                "drawtext=text='%{n}  %{pts\\:hms}':x=24:y=h-th-24:fontsize=28:\
                 fontcolor=white:box=1:boxcolor=black@0.5:boxborderw=6",
            );
        }
        if !vf.is_empty() {
            args.extend(["-vf".into(), vf]);
        }
        if container_supports_color_tags(spec.preset.container) {
            args.extend([
                "-color_primaries".into(),
                "bt709".into(),
                "-color_trc".into(),
                "bt709".into(),
                "-colorspace".into(),
                "bt709".into(),
                "-color_range".into(),
                "tv".into(),
            ]);
        }
    }

    if spec.preset.faststart {
        args.extend(["-movflags".into(), "+faststart".into()]);
    }

    args.push(spec.out_path.to_string_lossy().into_owned());
    Ok(args)
}

// ── Process management ───────────────────────────────────────────────────────

const STDERR_TAIL: usize = 32;

fn unique_audio_sidecar_path(ext: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "photonic-export-audio-{}-{n}-{nanos}.{ext}",
        std::process::id()
    ))
}

#[cfg(unix)]
fn create_fifo(path: &Path) -> Result<(), EncodeError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c_path =
        CString::new(path.as_os_str().as_bytes()).map_err(|_| EncodeError::InvalidFifoPath)?;
    // 0o600: owner read/write only — the FIFO carries transient local PCM
    // data for the lifetime of one export job.
    let ret = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    if ret != 0 {
        return Err(EncodeError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Write interleaved f32le PCM to `path` (Windows / non-unix second input).
#[cfg_attr(unix, allow(dead_code))]
fn write_pcm_file(path: &Path, samples: &[f32]) -> Result<(), EncodeError> {
    let mut file = std::fs::File::create(path).map_err(EncodeError::Io)?;
    let mut bytes = Vec::with_capacity(16_384);
    for chunk in samples.chunks(4096) {
        bytes.clear();
        for sample in chunk {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        file.write_all(&bytes).map_err(EncodeError::Io)?;
    }
    Ok(())
}

/// Stage PCM for a second ffmpeg `-i` without requiring a platform FIFO.
/// Public for headless tests of the Windows/non-unix path on any OS.
pub fn stage_audio_tempfile(samples: &[f32]) -> Result<PathBuf, EncodeError> {
    let p = unique_audio_sidecar_path("f32le");
    if let Err(error) = write_pcm_file(&p, samples) {
        let _ = std::fs::remove_file(&p);
        return Err(error);
    }
    Ok(p)
}

/// Second (audio) input path staged for this encode job (FIFO or temp file).
#[derive(Debug)]
struct AudioSidecar {
    path: PathBuf,
}

impl AudioSidecar {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for AudioSidecar {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Stage beside the destination so successful files publish by rename on the
/// same filesystem. An image sequence publishes each completed image atomically;
/// its existing filename-pattern API cannot promise an atomic directory swap.
struct StagedOutput {
    directory: PathBuf,
    path: PathBuf,
    destination: PathBuf,
    image_sequence: bool,
}

impl StagedOutput {
    fn new(destination: &Path, container: Container) -> Result<Self, EncodeError> {
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let name = destination.file_name().ok_or_else(|| {
            EncodeError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "output must name a file",
            ))
        })?;
        std::fs::create_dir_all(parent).map_err(EncodeError::Io)?;
        let directory = parent.join(format!(".photonic-export-{}.tmp", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).map_err(EncodeError::Io)?;
        Ok(Self {
            path: directory.join(name),
            directory,
            destination: destination.to_path_buf(),
            image_sequence: container == Container::ImageSequence,
        })
    }

    fn publish(&self, cancel: &AtomicBool, interrupted: &AtomicBool) -> Result<(), EncodeError> {
        let check_cancel = || {
            if cancel.load(Ordering::Relaxed) || interrupted.load(Ordering::Relaxed) {
                Err(EncodeError::Cancelled)
            } else {
                Ok(())
            }
        };
        if self.image_sequence {
            let parent = self.destination.parent().unwrap_or(Path::new("."));
            let mut files: Vec<_> = std::fs::read_dir(&self.directory)
                .map_err(EncodeError::Io)?
                .collect::<Result<_, _>>()
                .map_err(EncodeError::Io)?;
            files.sort_by_key(|entry| entry.file_name());
            for entry in files {
                check_cancel()?;
                std::fs::rename(entry.path(), parent.join(entry.file_name()))
                    .map_err(EncodeError::Io)?;
            }
        } else {
            check_cancel()?;
            std::fs::rename(&self.path, &self.destination).map_err(EncodeError::Io)?;
        }
        Ok(())
    }
}

impl Drop for StagedOutput {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

const PROCESS_POLL: Duration = Duration::from_millis(10);

/// A separate owner can interrupt a blocked stdin write or encoder flush.
/// The process owner remains responsible for waiting and joining its workers.
#[derive(Clone)]
pub(super) struct EncoderInterrupt {
    child: Arc<Mutex<Child>>,
    stop: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}

impl EncoderInterrupt {
    pub(super) fn interrupt(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.stop.store(true, Ordering::Relaxed);
        let _ = self
            .child
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .kill();
    }
}

/// Nonblocking FIFO open/write is essential: FFmpeg may exit before opening
/// input 1, or stop reading it while a video write is pending. Both operations
/// must observe cancellation so every audio writer can be joined.
#[cfg(unix)]
fn write_audio_fifo(path: &Path, samples: &[f32], stop: &AtomicBool) -> Result<(), EncodeError> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => break file,
            Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                std::thread::sleep(PROCESS_POLL)
            }
            Err(error) => return Err(EncodeError::Io(error)),
        }
    };
    let mut bytes = Vec::with_capacity(16_384);
    for chunk in samples.chunks(4096) {
        bytes.clear();
        for sample in chunk {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        let mut remaining = bytes.as_slice();
        while !remaining.is_empty() {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            match file.write(remaining) {
                Ok(0) => return Err(EncodeError::Io(std::io::ErrorKind::WriteZero.into())),
                Ok(n) => remaining = &remaining[n..],
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(PROCESS_POLL)
                }
                Err(error) => return Err(EncodeError::Io(error)),
            }
        }
    }
    Ok(())
}

/// A running ffmpeg encode process: video frames go to stdin; if the preset
/// has an audio track, a second input carries pre-mixed f32le PCM (FIFO on
/// unix, temp file elsewhere — see module docs).
pub struct EncoderProcess {
    control: EncoderInterrupt,
    video_stdin: Option<ChildStdin>,
    audio_writer: Option<JoinHandle<Result<(), EncodeError>>>,
    audio_sidecar: Option<AudioSidecar>,
    stderr_tail: Arc<Mutex<Vec<String>>>,
    stderr_reader: Option<JoinHandle<()>>,
    output: StagedOutput,
}

impl EncoderProcess {
    /// Spawn the encoder. `audio_samples`, when present, is the **whole**
    /// pre-mixed interleaved `f32` PCM track (09's offline mix) at
    /// `spec.audio`'s sample rate/channel count.
    pub fn spawn(
        tools: &FfmpegTools,
        caps: &EncoderCapabilities,
        spec: &EncodeSpec,
        audio_samples: Option<Vec<f32>>,
    ) -> Result<Self, EncodeError> {
        validate_encode_options(spec.two_pass)?;
        let plane_kind = plane_kind_for(
            spec.preset.video.as_ref().map(|v| v.codec),
            spec.preset.alpha,
        );
        let wants_audio =
            spec.preset.audio.is_some() && spec.audio.is_some() && audio_samples.is_some();
        let audio_path = wants_audio
            .then(|| unique_audio_sidecar_path(if cfg!(unix) { "fifo" } else { "f32le" }));
        // Resolve fallible options before creating a sidecar or worker.
        let mut args = build_ffmpeg_args(
            caps,
            spec,
            plane_kind.ffmpeg_pix_fmt(),
            audio_path.as_deref(),
        )?;
        let output = StagedOutput::new(&spec.out_path, spec.preset.container)?;
        if let Some(destination) = args.last_mut() {
            *destination = output.path.to_string_lossy().into_owned();
        }
        let audio_sidecar = audio_path.map(|path| AudioSidecar { path });
        if let Some(sidecar) = &audio_sidecar {
            #[cfg(unix)]
            create_fifo(sidecar.path())?;
            #[cfg(not(unix))]
            write_pcm_file(sidecar.path(), audio_samples.as_deref().unwrap_or_default())?;
        }

        let mut command = Command::new(&tools.ffmpeg);
        command
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        crate::media::child_registry::arm_parent_death_signal(&mut command);
        let mut child = command.spawn().map_err(EncodeError::Spawn)?;
        let video_stdin = child.stdin.take();
        let stderr = child.stderr.take();
        let mut process = Self {
            control: EncoderInterrupt {
                child: Arc::new(Mutex::new(child)),
                stop: Arc::new(AtomicBool::new(false)),
                cancelled: Arc::new(AtomicBool::new(false)),
            },
            video_stdin,
            audio_writer: None,
            audio_sidecar,
            stderr_tail: Arc::new(Mutex::new(Vec::new())),
            stderr_reader: None,
            output,
        };
        if process.video_stdin.is_none() {
            return Err(EncodeError::Io(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "ffmpeg stdin pipe unavailable",
            )));
        }
        if let Some(stderr) = stderr {
            use std::io::{BufRead, BufReader};
            let tail = Arc::clone(&process.stderr_tail);
            process.stderr_reader = Some(
                std::thread::Builder::new()
                    .name("photonic-encode-stderr".into())
                    .spawn(move || {
                        let reader = BufReader::new(stderr);
                        for line in reader.lines().map_while(Result::ok) {
                            let mut tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
                            if tail.len() == STDERR_TAIL {
                                tail.remove(0);
                            }
                            tail.push(line);
                        }
                    })
                    .map_err(EncodeError::Spawn)?,
            );
        }
        #[cfg(unix)]
        if let Some(sidecar) = &process.audio_sidecar {
            let path = sidecar.path.clone();
            let samples = audio_samples.unwrap_or_default();
            let stop = Arc::clone(&process.control.stop);
            process.audio_writer = Some(
                std::thread::Builder::new()
                    .name("photonic-encode-audio".into())
                    .spawn(move || write_audio_fifo(&path, &samples, &stop))
                    .map_err(EncodeError::Spawn)?,
            );
        }
        Ok(process)
    }

    pub(super) fn interrupt_handle(&self) -> EncoderInterrupt {
        self.control.clone()
    }

    /// Write planes directly in rawvideo wire order. No concatenation buffer
    /// is allocated for each frame; cancellation interrupts a blocked pipe by
    /// killing the child through the independent control handle.
    pub fn write_video_frame(&mut self, planes: &EncodePlanes) -> Result<(), EncodeError> {
        if self.control.stop.load(Ordering::Relaxed) {
            return Err(EncodeError::Cancelled);
        }
        let stdin = self.video_stdin.as_mut().ok_or_else(|| {
            EncodeError::Io(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "video stdin already closed",
            ))
        })?;
        planes.write_to(stdin).map_err(EncodeError::Io)
    }

    /// Close inputs, wait for FFmpeg, join both I/O workers, then publish the
    /// completed output. The child mutex is never held across the wait: another
    /// thread can still interrupt a stalled codec flush.
    pub fn finish(self) -> Result<(), EncodeError> {
        self.finish_with_cancel(&AtomicBool::new(false))
    }

    pub(super) fn finish_with_cancel(mut self, cancel: &AtomicBool) -> Result<(), EncodeError> {
        drop(self.video_stdin.take());
        let status = loop {
            if cancel.load(Ordering::Relaxed) {
                self.control.interrupt();
            }
            let status = self
                .control
                .child
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .try_wait()
                .map_err(EncodeError::Io)?;
            if let Some(status) = status {
                break status;
            }
            std::thread::sleep(PROCESS_POLL);
        };
        self.control.stop.store(true, Ordering::Relaxed);
        let workers = self.join_workers();
        if self.control.cancelled.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
            return Err(EncodeError::Cancelled);
        }
        if !status.success() {
            return Err(EncodeError::EncoderExited {
                status: status.code(),
                stderr: self
                    .stderr_tail
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .join("\n"),
            });
        }
        workers?;
        if self.control.cancelled.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
            return Err(EncodeError::Cancelled);
        }
        self.output.publish(cancel, &self.control.cancelled)
    }

    fn join_workers(&mut self) -> Result<(), EncodeError> {
        let audio = self
            .audio_writer
            .take()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| EncodeError::AudioWriterPanicked)
                    .and_then(|result| result)
            })
            .unwrap_or(Ok(()));
        let stderr = self
            .stderr_reader
            .take()
            .map(|handle| handle.join().map_err(|_| EncodeError::StderrReaderPanicked))
            .unwrap_or(Ok(()));
        // Join both before propagating either error.
        audio.and(stderr)
    }

    /// Kill and reap the child, join I/O workers, and remove staged files.
    pub fn cancel(self) {
        drop(self);
    }
}

impl Drop for EncoderProcess {
    fn drop(&mut self) {
        self.control.interrupt();
        drop(self.video_stdin.take());
        let _ = self
            .control
            .child
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .wait();
        let _ = self.join_workers();
        // Sidecar/output guards clean up after every worker has stopped.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::presets::{
        AudioEncodeSpec, Container, ExportPreset, FrameRatePolicy, LoudnessTarget, QualityMode,
        ResolutionSpec, VideoEncodeSpec,
    };

    fn caps_with(names: &[&str]) -> EncoderCapabilities {
        EncoderCapabilities {
            names: names.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn base_preset() -> ExportPreset {
        ExportPreset {
            name: "test".into(),
            container: Container::Mp4,
            video: Some(VideoEncodeSpec {
                codec: VideoCodec::H264,
                quality: QualityMode::Crf(20.0),
            }),
            audio: Some(AudioEncodeSpec {
                codec: AudioCodec::Aac,
                bitrate_kbps: Some(128),
            }),
            resolution: ResolutionSpec::SourceFormat,
            frame_rate: FrameRatePolicy::MatchSequence,
            alpha: false,
            faststart: true,
            loudness_target: None::<LoudnessTarget>,
            stems: false,
        }
    }

    fn spec(preset: &ExportPreset) -> EncodeSpec<'_> {
        EncodeSpec {
            preset,
            width: 32,
            height: 32,
            frame_rate: FrameRate::new(10, 1),
            audio: Some(AudioStreamSpec {
                sample_rate: 48_000,
                channels: 2,
            }),
            out_path: PathBuf::from("/tmp/out.mp4"),
            prefer_hardware: false,
            encoder_speed: None,
            raw_encoder_args: &[],
            burn_in_timecode: false,
            two_pass: false,
        }
    }

    // ── capability parsing ───────────────────────────────────────────────────

    #[test]
    fn parse_encoders_output_extracts_names() {
        let sample = " V....D libx264              libx264 H.264 / AVC\n\
                        A....D aac                   AAC (Advanced Audio Coding)\n\
                        \n\
                        Encoders:\n";
        let caps = EncoderCapabilities::parse(sample);
        assert!(caps.has("libx264"));
        assert!(caps.has("aac"));
        assert!(!caps.has("libopenh264"));
    }

    #[test]
    fn hardware_encoders_only_lists_probed_names() {
        let caps = caps_with(&["libx264", "h264_nvenc", "hevc_vaapi", "not_a_real_hw"]);
        let hw = caps.hardware_encoders();
        assert_eq!(hw, vec!["h264_nvenc", "hevc_vaapi"]);
        assert_eq!(caps.hardware_for(VideoCodec::H264), Some("h264_nvenc"));
        assert_eq!(caps.hardware_for(VideoCodec::Av1), None);
        assert_eq!(caps.hardware_for(VideoCodec::Vp9), None);
    }

    #[test]
    fn prefer_hardware_fail_closed_when_missing() {
        let preset = base_preset();
        let mut s = spec(&preset);
        s.prefer_hardware = true;
        let caps = caps_with(&["libx264", "aac"]); // no HW
        let err = build_ffmpeg_args(&caps, &s, "yuv420p", None).unwrap_err();
        assert!(
            matches!(err, EncodeError::HardwareUnavailable { .. }),
            "expected HardwareUnavailable, got {err:?}"
        );
    }

    #[test]
    fn prefer_hardware_selects_nvenc_when_probed() {
        let preset = base_preset();
        let mut s = spec(&preset);
        s.prefer_hardware = true;
        let caps = caps_with(&["libx264", "h264_nvenc", "aac"]);
        let args = build_ffmpeg_args(&caps, &s, "yuv420p", None).unwrap();
        assert!(args.windows(2).any(|w| w == ["-c:v", "h264_nvenc"]));
        // Fail-closed path never falls back to software.
        assert!(!args.windows(2).any(|w| w == ["-c:v", "libx264"]));
    }

    #[test]
    fn detection_report_mentions_software_and_hardware() {
        let caps = caps_with(&["libx264", "libsvtav1", "h264_qsv"]);
        let report = caps.detection_report();
        assert!(report.contains("Software:"));
        assert!(report.contains("h264_qsv"));
    }

    #[test]
    fn h264_encoder_prefers_openh264_falls_back_to_libx264() {
        assert_eq!(
            caps_with(&["libopenh264", "libx264"]).h264_encoder(),
            "libopenh264"
        );
        assert_eq!(caps_with(&["libx264"]).h264_encoder(), "libx264");
    }

    #[test]
    fn av1_encoder_prefers_svtav1_falls_back_to_rav1e() {
        assert_eq!(
            caps_with(&["libsvtav1", "librav1e"]).av1_encoder(),
            "libsvtav1"
        );
        assert_eq!(caps_with(&["librav1e"]).av1_encoder(), "librav1e");
    }

    // ── plane-shape selection ────────────────────────────────────────────────

    #[test]
    fn plane_kind_routes_png_apng_to_rgba8() {
        assert_eq!(
            plane_kind_for(Some(VideoCodec::Png), true),
            PlaneKind::Rgba8
        );
        assert_eq!(
            plane_kind_for(Some(VideoCodec::Apng), true),
            PlaneKind::Rgba8
        );
    }

    #[test]
    fn plane_kind_routes_vp9_alpha_to_yuva420_prores_alpha_to_yuva444() {
        assert_eq!(
            plane_kind_for(Some(VideoCodec::Vp9), true),
            PlaneKind::Yuva420
        );
        assert_eq!(
            plane_kind_for(Some(VideoCodec::ProResLikeMezzanine), true),
            PlaneKind::Yuva444
        );
    }

    #[test]
    fn plane_kind_no_alpha_is_yuv420_for_any_yuv_codec() {
        assert_eq!(
            plane_kind_for(Some(VideoCodec::H264), false),
            PlaneKind::Yuv420
        );
        assert_eq!(
            plane_kind_for(Some(VideoCodec::Av1), false),
            PlaneKind::Yuv420
        );
    }

    // ── crf heuristic ────────────────────────────────────────────────────────

    #[test]
    fn crf_to_kbps_heuristic_is_monotonically_decreasing_in_crf() {
        let lo = crf_to_kbps_heuristic(15.0, 1920, 1080);
        let mid = crf_to_kbps_heuristic(23.0, 1920, 1080);
        let hi = crf_to_kbps_heuristic(35.0, 1920, 1080);
        assert!(lo > mid && mid > hi, "lo={lo} mid={mid} hi={hi}");
    }

    #[test]
    fn crf_to_kbps_heuristic_scales_with_pixel_count() {
        let hd = crf_to_kbps_heuristic(23.0, 1920, 1080);
        let sd = crf_to_kbps_heuristic(23.0, 960, 540);
        assert!(hd > sd);
    }

    // ── arg building (pure, no ffmpeg spawn) ─────────────────────────────────

    #[test]
    fn build_args_h264_uses_libx264_crf_when_no_openh264() {
        let preset = base_preset();
        let caps = caps_with(&["libx264", "aac"]);
        let s = spec(&preset);
        let args = build_ffmpeg_args(&caps, &s, "yuv420p", Some(Path::new("/tmp/a.fifo"))).unwrap();
        assert!(args.windows(2).any(|w| w == ["-c:v", "libx264"]));
        assert!(args.windows(2).any(|w| w == ["-crf", "20"]));
        assert!(args.iter().any(|a| a == "pipe:0"));
        assert!(args.windows(2).any(|w| w == ["-map", "0:v"]));
        assert!(args.windows(2).any(|w| w == ["-map", "1:a"]));
        assert!(args.windows(2).any(|w| w == ["-c:a", "aac"]));
        assert!(args.iter().any(|a| a.contains("setparams")));
        assert!(args.windows(2).any(|w| w == ["-movflags", "+faststart"]));
    }

    #[test]
    fn build_args_no_audio_when_preset_has_none() {
        let mut preset = base_preset();
        preset.audio = None;
        let s = spec(&preset);
        let caps = caps_with(&["libx264"]);
        let args = build_ffmpeg_args(&caps, &s, "yuv420p", None).unwrap();
        assert!(args.iter().any(|a| a == "-an"));
        assert!(!args.iter().any(|a| a == "-c:a"));
        assert!(!args.windows(2).any(|w| w == ["-map", "1:a"]));
    }

    #[test]
    fn build_args_no_video_sets_vn() {
        let mut preset = base_preset();
        preset.video = None;
        let s = spec(&preset);
        let caps = caps_with(&["aac"]);
        let args = build_ffmpeg_args(&caps, &s, "yuv420p", Some(Path::new("/tmp/a.fifo"))).unwrap();
        assert!(args.iter().any(|a| a == "-vn"));
    }

    #[test]
    fn build_args_vp9_alpha_uses_auto_alt_ref_0_and_b_v_0() {
        let mut preset = base_preset();
        preset.container = Container::WebM;
        preset.alpha = true;
        preset.video = Some(VideoEncodeSpec {
            codec: VideoCodec::Vp9,
            quality: QualityMode::Crf(24.0),
        });
        let s = spec(&preset);
        let caps = caps_with(&["libvpx-vp9", "libopus"]);
        let args =
            build_ffmpeg_args(&caps, &s, "yuva420p", Some(Path::new("/tmp/a.fifo"))).unwrap();
        assert!(args.windows(2).any(|w| w == ["-c:v", "libvpx-vp9"]));
        assert!(args.windows(2).any(|w| w == ["-auto-alt-ref", "0"]));
        assert!(args.windows(2).any(|w| w == ["-b:v", "0"]));
    }

    #[test]
    fn build_args_gif_uses_palette_filter_not_setparams() {
        let mut preset = base_preset();
        preset.container = Container::Gif;
        preset.audio = None;
        preset.video = Some(VideoEncodeSpec {
            codec: VideoCodec::Gif,
            quality: QualityMode::Lossless,
        });
        let s = spec(&preset);
        let caps = caps_with(&[]);
        let args = build_ffmpeg_args(&caps, &s, "yuv420p", None).unwrap();
        assert!(args.iter().any(|a| a.contains("palettegen")));
        assert!(
            !args.iter().any(|a| a.contains("setparams")),
            "gif skips color tagging"
        );
    }

    #[test]
    fn build_args_prores_sets_profile_4_no_forced_output_pix_fmt() {
        let mut preset = base_preset();
        preset.container = Container::Mov;
        preset.alpha = true;
        preset.video = Some(VideoEncodeSpec {
            codec: VideoCodec::ProResLikeMezzanine,
            quality: QualityMode::Lossless,
        });
        preset.audio = Some(AudioEncodeSpec {
            codec: AudioCodec::Pcm,
            bitrate_kbps: None,
        });
        let s = spec(&preset);
        let caps = caps_with(&[]);
        let args =
            build_ffmpeg_args(&caps, &s, "yuva444p", Some(Path::new("/tmp/a.fifo"))).unwrap();
        assert!(args.windows(2).any(|w| w == ["-c:v", "prores_ks"]));
        assert!(args.windows(2).any(|w| w == ["-profile:v", "4"]));
        assert!(args.windows(2).any(|w| w == ["-c:a", "pcm_s16le"]));
        // No output -pix_fmt override after the input declaration — only one
        // "-pix_fmt" occurrence total (the rawvideo input side).
        assert_eq!(args.iter().filter(|a| a.as_str() == "-pix_fmt").count(), 1);
    }

    #[test]
    fn build_args_faststart_only_added_when_preset_requests_it() {
        let mut preset = base_preset();
        preset.faststart = false;
        let s = spec(&preset);
        let caps = caps_with(&["libx264"]);
        let args = build_ffmpeg_args(&caps, &s, "yuv420p", None).unwrap();
        assert!(!args.iter().any(|a| a == "+faststart"));
    }

    #[test]
    fn build_args_color_tags_skipped_for_gif_and_image_sequence() {
        let mut preset = base_preset();
        preset.container = Container::ImageSequence;
        preset.audio = None;
        preset.video = Some(VideoEncodeSpec {
            codec: VideoCodec::Png,
            quality: QualityMode::Lossless,
        });
        let s = spec(&preset);
        let caps = caps_with(&["png"]);
        let args = build_ffmpeg_args(&caps, &s, "rgba", None).unwrap();
        assert!(!args.iter().any(|a| a.contains("bt709")));
    }

    #[test]
    fn two_pass_is_rejected_by_argument_builder() {
        let preset = base_preset();
        let mut spec = spec(&preset);
        spec.two_pass = true;
        assert!(matches!(
            build_ffmpeg_args(&caps_with(&["libx264"]), &spec, "yuv420p", None),
            Err(EncodeError::TwoPassUnsupported)
        ));
    }

    #[cfg(unix)]
    fn stalled_encoder(directory: &Path) -> FfmpegTools {
        use std::os::unix::fs::PermissionsExt;
        let executable = directory.join("stalled-encoder.sh");
        // exec keeps the sleeping process under the exact Child PID. It opens
        // neither video nor audio, exercising both blocked input paths.
        std::fs::write(&executable, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        FfmpegTools {
            ffmpeg: executable,
            ffprobe: PathBuf::from("ffprobe"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn interrupt_unblocks_video_write_and_reaps_child_and_audio_writer() {
        let directory =
            std::env::temp_dir().join(format!("photonic-encoder-cancel-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let tools = stalled_encoder(&directory);
        let preset = base_preset();
        let mut spec = spec(&preset);
        spec.out_path = directory.join("old.mp4");
        std::fs::write(&spec.out_path, b"previous output").unwrap();
        let mut process = EncoderProcess::spawn(
            &tools,
            &caps_with(&["libx264", "aac"]),
            &spec,
            Some(vec![0.0; 48_000]),
        )
        .unwrap();
        let control = process.interrupt_handle();
        let pid = control.child.lock().unwrap().id();
        let sidecar = process.audio_sidecar.as_ref().unwrap().path.clone();
        let staged = process.output.directory.clone();
        let planes = EncodePlanes::Rgba8 {
            width: 1024,
            height: 1024,
            rgba: vec![0; 1024 * 1024 * 4],
        };
        let start = std::time::Instant::now();
        std::thread::scope(|scope| {
            let join = scope.spawn(move || {
                let result = process.write_video_frame(&planes);
                drop(process);
                result
            });
            std::thread::sleep(Duration::from_millis(30));
            control.interrupt();
            assert!(join.join().unwrap().is_err());
        });
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(!sidecar.exists());
        assert!(!staged.exists());
        assert_eq!(std::fs::read(&spec.out_path).unwrap(), b"previous output");
        // waitpid must report ECHILD: Drop has already waited for this child.
        let mut status = 0;
        let waited = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
        assert_eq!(waited, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn encoder_exit_before_opening_audio_joins_fifo_writer() {
        use std::os::unix::fs::PermissionsExt;
        let directory =
            std::env::temp_dir().join(format!("photonic-encoder-exit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let executable = directory.join("encoder.sh");
        std::fs::write(&executable, "#!/bin/sh\necho rejected >&2\nexit 9\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let tools = FfmpegTools {
            ffmpeg: executable,
            ffprobe: PathBuf::from("ffprobe"),
        };
        let preset = base_preset();
        let mut spec = spec(&preset);
        spec.out_path = directory.join("out.mp4");
        let process = EncoderProcess::spawn(
            &tools,
            &caps_with(&["libx264"]),
            &spec,
            Some(vec![0.0; 48_000]),
        )
        .unwrap();
        let sidecar = process.audio_sidecar.as_ref().unwrap().path.clone();
        let staged = process.output.directory.clone();
        let start = std::time::Instant::now();
        assert!(
            matches!(process.finish(), Err(EncodeError::EncoderExited { status: Some(9), stderr }) if stderr.contains("rejected"))
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(!sidecar.exists());
        assert!(!staged.exists());
        assert!(!spec.out_path.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_before_publication_keeps_existing_output() {
        use std::os::unix::fs::PermissionsExt;
        let directory =
            std::env::temp_dir().join(format!("photonic-encoder-publish-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let executable = directory.join("encoder.sh");
        std::fs::write(
            &executable,
            "#!/bin/sh\nfor out; do :; done\nprintf complete > \"$out\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let tools = FfmpegTools {
            ffmpeg: executable,
            ffprobe: PathBuf::from("ffprobe"),
        };
        let mut preset = base_preset();
        preset.audio = None;
        let mut spec = spec(&preset);
        spec.out_path = directory.join("output.mp4");
        std::fs::write(&spec.out_path, b"previous").unwrap();
        let process = EncoderProcess::spawn(&tools, &caps_with(&["libx264"]), &spec, None).unwrap();
        let staged = process.output.directory.clone();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while process
            .control
            .child
            .lock()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none()
        {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(PROCESS_POLL);
        }
        assert!(matches!(
            process.finish_with_cancel(&AtomicBool::new(true)),
            Err(EncodeError::Cancelled)
        ));
        assert_eq!(std::fs::read(&spec.out_path).unwrap(), b"previous");
        assert!(!staged.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
