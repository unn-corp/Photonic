//! Integration tests for the P4 export engine (`photonic_video::export`),
//! run against **synthetic** frames only (05-import-export.md §3,
//! 02-engine.md §7) — the P3 evaluator is a concurrent, separate build, so
//! these tests never touch the frame-graph/decode path.
//!
//! Every test that shells out to ffmpeg **skips with a message** when the
//! toolchain is absent (mirrors `tests/decode_media.rs`'s convention), so the
//! suite is green on a machine without ffmpeg and exercises real encode where
//! ffmpeg is present (locally + CI).
//!
//! Coverage (05 §8 / this story's DoD):
//! - Preset serde round-trip + catalog assertions: covered by
//!   `export::presets::tests` (unit tests, `src/export/presets.rs`).
//! - Conversion math vs `photonic_render::color`'s CPU reference: covered by
//!   `export::convert::tests` (unit tests, `src/export/convert.rs`).
//! - E2E synthetic export per feasible built-in preset: this file —
//!   `ffprobe`-verified container/codec/dims/duration (CAP-013), plus a
//!   PSNR check between the exact pre-encode raw bytes `convert.rs` produced
//!   and the real encoder output decoded back (>35dB threshold).
//! - Alpha round-trip (VP9/WebM and PNG sequence): this file — decode +
//!   sample known transparent/opaque regions.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

use photonic_core::timeline::{FrameRate, Tick};
use photonic_render::{
    color::{Colorimetry, Range},
    video::{NativeChromaLocation, NativeYuvInput, YuvConverter},
};
use photonic_video::decode::scheduler::{PtsKind, SourceParams};
use photonic_video::decode::{DecodeSource, PixFmt, SharedRing};
use photonic_video::export::convert::{working_frame_to_rgba8, working_frame_to_yuv_planes};
use photonic_video::export::encoder::{
    plane_kind_for, AudioStreamSpec, EncoderCapabilities, PlaneKind,
};
use photonic_video::export::presets::built_in_presets;
use photonic_video::export::render_loop::{export_frames, ExportEvent, Frame, ResolvedExport};
use photonic_video::media::ffmpeg_locate::{locate_for_test, FfmpegTools};
use photonic_video::media::{keyframe_index::KeyframeIndex, probe::probe_details};
use photonic_video::{
    color::native::{decode_video_sample_to_ap1, NativeVideoInput},
    graph::eval::{read_texture_rgba16f, GpuContext},
};

macro_rules! tools_or_skip {
    () => {
        match locate_for_test() {
            Some(t) => t,
            None => {
                eprintln!(
                    "ffmpeg/ffprobe not found — skipping export test at {}:{} \
                     (set PHOTONIC_FFMPEG_DIR or install ffmpeg)",
                    file!(),
                    line!()
                );
                return;
            }
        }
    };
}

const W: u32 = 32;
const H: u32 = 32;
const N: u64 = 30;
const FPS: FrameRate = FrameRate { num: 10, den: 1 };

fn preset_by_name(name: &str) -> photonic_video::export::presets::ExportPreset {
    built_in_presets()
        .into_iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("no built-in preset named {name:?}"))
}

fn tmp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("photonic-export-e2e-{}-{name}", std::process::id()))
}

/// Deterministic, distinguishable-per-frame synthetic content: a diagonal
/// gradient plus a moving vertical bar (a cheap "counter" — position encodes
/// the frame index), fully opaque.
fn synthetic_frame(i: u64, w: u32, h: u32) -> Frame {
    let mut rgba = vec![0.0f32; (w * h * 4) as usize];
    let bar_x = (i % w as u64) as u32;
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            let r = x as f32 / (w - 1).max(1) as f32;
            let g = y as f32 / (h - 1).max(1) as f32;
            let b = if x == bar_x { 1.0 } else { 0.2 };
            rgba[idx] = r;
            rgba[idx + 1] = g;
            rgba[idx + 2] = b;
            rgba[idx + 3] = 1.0;
        }
    }
    Frame {
        width: w,
        height: h,
        rgba_premult: rgba,
        encoding: photonic_video::graph::ir::FrameColorEncoding::LegacyLinearRec709,
    }
}

/// Alpha ramps left (transparent) → right (opaque), matching the existing
/// `alpha_gradient.mov` fixture's convention elsewhere in this crate
/// (`tests/decode_media.rs`) — solid mid-grey color, premultiplied by alpha.
fn synthetic_alpha_frame(i: u64, w: u32, h: u32) -> Frame {
    let mut rgba = vec![0.0f32; (w * h * 4) as usize];
    let shift = (i % w as u64) as u32;
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            let xs = (x + shift) % w;
            let a = xs as f32 / (w - 1).max(1) as f32;
            let color = 0.6f32;
            rgba[idx] = color * a;
            rgba[idx + 1] = color * a;
            rgba[idx + 2] = color * a;
            rgba[idx + 3] = a;
        }
    }
    Frame {
        width: w,
        height: h,
        rgba_premult: rgba,
        encoding: photonic_video::graph::ir::FrameColorEncoding::LegacyLinearRec709,
    }
}

fn synthetic_prores_ramp_frame(_i: u64, w: u32, h: u32) -> Frame {
    let mut rgba = vec![0.0f32; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let pixel = (y * w + x) as usize;
            let value = pixel as f32 / (w * h - 1) as f32;
            let alpha = x as f32 / (w - 1) as f32;
            rgba[pixel * 4..pixel * 4 + 4].copy_from_slice(&[
                value * alpha,
                value * alpha,
                value * alpha,
                alpha,
            ]);
        }
    }
    Frame {
        width: w,
        height: h,
        rgba_premult: rgba,
        encoding: photonic_video::graph::ir::FrameColorEncoding::LegacyLinearRec709,
    }
}

fn ffprobe_json(tools: &FfmpegTools, path: &Path) -> serde_json::Value {
    let out = Command::new(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .expect("ffprobe spawns");
    assert!(
        out.status.success(),
        "ffprobe failed on {path:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("ffprobe JSON parses")
}

fn video_stream(json: &serde_json::Value) -> &serde_json::Value {
    json["streams"]
        .as_array()
        .expect("streams array")
        .iter()
        .find(|s| s["codec_type"] == "video")
        .expect("a video stream exists")
}

/// Build the raw reference bytes exactly as `render_loop` would feed them to
/// the encoder (calling the same `convert.rs` functions the production path
/// uses), for `psnr_against_reference`'s comparison. Returns
/// `(raw_bytes, ffmpeg_pix_fmt)`.
fn build_reference_raw(
    preset: &photonic_video::export::presets::ExportPreset,
    frames: u64,
    w: u32,
    h: u32,
    frame_fn: impl Fn(u64, u32, u32) -> Frame,
) -> (Vec<u8>, &'static str) {
    let plane_kind = plane_kind_for(preset.video.as_ref().map(|v| v.codec), preset.alpha);
    let alpha_444 = plane_kind == PlaneKind::Yuva444;
    let mut raw = Vec::new();
    for i in 0..frames {
        let f = frame_fn(i, w, h);
        let planes = match plane_kind {
            PlaneKind::Rgba8 => working_frame_to_rgba8(&f.rgba_premult, w, h),
            PlaneKind::Yuv422P10 => photonic_video::export::convert::working_frame_to_yuv422p10(
                &f.rgba_premult,
                w,
                h,
                Colorimetry::BT709_LIMITED,
            ),
            PlaneKind::Yuva444P12 => photonic_video::export::convert::working_frame_to_yuva444p12(
                &f.rgba_premult,
                w,
                h,
                Colorimetry::BT709_LIMITED,
            ),
            _ => working_frame_to_yuv_planes(
                &f.rgba_premult,
                w,
                h,
                Colorimetry::BT709_LIMITED,
                preset.alpha,
                alpha_444,
            ),
        };
        raw.extend(planes.to_bytes());
    }
    (raw, plane_kind.ffmpeg_pix_fmt())
}

/// PSNR between the pre-encode raw reference and the real encoder output,
/// via ffmpeg's own `psnr` filter (decodes `encoded_path` internally).
fn psnr_against_reference(
    tools: &FfmpegTools,
    raw_ref: &[u8],
    ref_pix_fmt: &str,
    w: u32,
    h: u32,
    fps: FrameRate,
    encoded_path: &Path,
) -> f64 {
    let ref_path = tmp_path(&format!(
        "ref-{}.raw",
        encoded_path.file_stem().unwrap().to_string_lossy()
    ));
    std::fs::write(&ref_path, raw_ref).expect("write raw reference");

    // The raw reference carries the *same* signal the encoder was fed:
    // BT.709-matrixed, limited-range Y'CbCr (convert.rs range-compresses to
    // `Range::Limited` for every built-in preset, and the encoder tags its
    // output `bt709`/`color_range tv`). A rawvideo input has no color metadata
    // of its own, so ffmpeg would assume a *different* default and auto-insert
    // a range/matrix conversion inside the `psnr` graph — a systematic
    // limited↔full luma shift (~10 code levels, ~28 dB Y) that swamps the real
    // codec loss and has nothing to do with the encode. Tagging the reference
    // to match the encoded stream makes `psnr` compare like-for-like signal, so
    // the number reflects only encoder quality. (convert.rs's own math is
    // verified against `photonic_render::color` by its `working_pixel_round_
    // trips_through_decodes_yuv_to_working` unit test, <1e-3 linear-light — the
    // >35 dB floor stays put; this fixes the measurement, not a conversion bug.)
    let out = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-nostdin", "-loglevel", "info"])
        .args(["-f", "rawvideo", "-pix_fmt", ref_pix_fmt])
        .args([
            "-colorspace",
            "bt709",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-color_range",
            "tv",
        ])
        .args([
            "-s",
            &format!("{w}x{h}"),
            "-r",
            &format!("{}/{}", fps.num, fps.den),
        ])
        .arg("-i")
        .arg(&ref_path)
        .arg("-i")
        .arg(encoded_path)
        .args(["-lavfi", "[0:v][1:v]psnr", "-f", "null", "-"])
        .output()
        .expect("ffmpeg psnr spawns");
    let _ = std::fs::remove_file(&ref_path);

    let stderr = String::from_utf8_lossy(&out.stderr);
    // ffmpeg's psnr filter prints a line like:
    // "[Parsed_psnr_0 ... ] PSNR y:XX.xx u:XX.xx v:XX.xx average:XX.xx min:.. max:.."
    let line = stderr
        .lines()
        .find(|l| l.contains("average:"))
        .unwrap_or_else(|| panic!("no PSNR line in ffmpeg output:\n{stderr}"));
    let avg = line
        .split("average:")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or_else(|| panic!("could not parse PSNR average from: {line}"));
    avg
}

fn try_export(
    tools: &FfmpegTools,
    preset: &photonic_video::export::presets::ExportPreset,
    out_path: &Path,
    frames: u64,
    frame_fn: impl Fn(u64, u32, u32) -> Frame,
    with_audio: bool,
) -> Result<(), String> {
    let _ = std::fs::remove_file(out_path);
    let audio = if with_audio && preset.audio.is_some() {
        Some(AudioStreamSpec {
            sample_rate: 48_000,
            channels: 2,
        })
    } else {
        None
    };
    let audio_samples = audio.map(|a| {
        let n = (a.sample_rate as u64 * frames / FPS.num as u64) as usize * a.channels as usize;
        vec![0.0f32; n]
    });
    let resolved = ResolvedExport {
        width: W,
        height: H,
        frame_rate: FPS,
        audio,
        out_path: out_path.to_path_buf(),
        colorimetry: Colorimetry::BT709_LIMITED,
        prefer_hardware: false,
        encoder_speed: None,
        raw_encoder_args: vec![],
        burn_in_timecode: false,
        two_pass: false,
    };
    let cancel = AtomicBool::new(false);
    let mut saw_done = false;
    export_frames(
        tools,
        preset,
        &resolved,
        frames,
        |i| frame_fn(i, W, H),
        audio_samples,
        &cancel,
        |e| {
            if matches!(e, ExportEvent::Done) {
                saw_done = true;
            }
        },
    )
    .map_err(|e| format!("export of {:?} failed: {e}", preset.name))?;
    if !saw_done {
        return Err(format!("export of {:?} did not emit Done", preset.name));
    }
    // Image-sequence exports write to a printf pattern (`frame_%06d.png`); that
    // literal path is never a real file — the per-frame expansions land in the
    // parent directory instead. Assert *those* exist rather than the pattern.
    if out_path.to_string_lossy().contains('%') {
        let parent = out_path
            .parent()
            .ok_or_else(|| "pattern path has no parent".to_string())?;
        let n = std::fs::read_dir(parent)
            .map_err(|e| format!("read_dir {parent:?}: {e}"))?
            .filter_map(|e| e.ok())
            .count();
        if n == 0 {
            return Err(format!(
                "no image-sequence frames written under {:?}",
                parent
            ));
        }
    } else if !out_path.exists() {
        return Err(format!("{:?} was not written", out_path));
    }
    Ok(())
}

fn run_export(
    tools: &FfmpegTools,
    preset: &photonic_video::export::presets::ExportPreset,
    out_path: &Path,
    frames: u64,
    frame_fn: impl Fn(u64, u32, u32) -> Frame,
    with_audio: bool,
) {
    try_export(tools, preset, out_path, frames, frame_fn, with_audio)
        .unwrap_or_else(|e| panic!("{e}"));
}

// ── H.264 (Social family, representative of all three) ──────────────────────

#[test]
fn export_h264_social_e2e_ffprobe_and_psnr() {
    let tools = tools_or_skip!();
    let preset = preset_by_name("Social 16:9");
    let out = tmp_path("social.mp4");

    run_export(&tools, &preset, &out, N, synthetic_frame, true);

    let json = ffprobe_json(&tools, &out);
    let v = video_stream(&json);
    assert_eq!(v["codec_name"], "h264");
    assert_eq!(v["width"], W);
    assert_eq!(v["height"], H);
    let duration: f64 = json["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .expect("duration present");
    let expected = N as f64 / (FPS.num as f64 / FPS.den as f64);
    assert!(
        (duration - expected).abs() < 0.5,
        "duration {duration} vs expected {expected}"
    );

    let (raw, pix_fmt) = build_reference_raw(&preset, N, W, H, synthetic_frame);
    let psnr = psnr_against_reference(&tools, &raw, pix_fmt, W, H, FPS, &out);
    assert!(psnr > 35.0, "H.264 PSNR too low: {psnr} dB");

    let _ = std::fs::remove_file(&out);
}

// ── AV1 (SVT-AV1 on this workstation) ────────────────────────────────────────

#[test]
fn export_master_av1_high_e2e_ffprobe_and_psnr() {
    let tools = tools_or_skip!();
    // Ubuntu apt ffmpeg may list libsvtav1/librav1e but still fail at encode
    // (Broken pipe). Probe-encode one tiny frame and skip if the encoder is
    // unusable rather than red-ing the whole workspace suite.
    let caps = EncoderCapabilities::probe(&tools).expect("ffmpeg -encoders");
    if !caps.has("libsvtav1") && !caps.has("librav1e") {
        eprintln!("ffmpeg has no AV1 encoder (libsvtav1/librav1e) — skipping AV1 export e2e");
        return;
    }
    let preset = preset_by_name("Master AV1 High");
    let probe_out = tmp_path("master_av1_probe.mkv");
    if let Err(e) = try_export(&tools, &preset, &probe_out, 2, synthetic_frame, false) {
        eprintln!(
            "AV1 encoder present but unusable ({e}) — skipping AV1 export e2e \
             (install a working libsvtav1/librav1e build, or set PHOTONIC_FFMPEG_DIR)"
        );
        let _ = std::fs::remove_file(&probe_out);
        return;
    }
    let _ = std::fs::remove_file(&probe_out);

    let out = tmp_path("master_av1.mkv");
    run_export(&tools, &preset, &out, N, synthetic_frame, true);

    let json = ffprobe_json(&tools, &out);
    let v = video_stream(&json);
    assert_eq!(v["codec_name"], "av1");
    assert_eq!(v["width"], W);
    assert_eq!(v["height"], H);
    let has_audio = json["streams"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["codec_type"] == "audio" && s["codec_name"] == "opus");
    assert!(has_audio, "expected an opus audio stream");

    let (raw, pix_fmt) = build_reference_raw(&preset, N, W, H, synthetic_frame);
    let psnr = psnr_against_reference(&tools, &raw, pix_fmt, W, H, FPS, &out);
    assert!(psnr > 35.0, "AV1 PSNR too low: {psnr} dB");

    let _ = std::fs::remove_file(&out);
}

// ── Web H.264 (bitrate mode, not CRF) ────────────────────────────────────────

#[test]
fn export_web_h264_bitrate_mode_e2e_ffprobe() {
    let tools = tools_or_skip!();
    let preset = preset_by_name("Web H.264");
    let out = tmp_path("web_h264.mp4");

    run_export(&tools, &preset, &out, N, synthetic_frame, true);

    let json = ffprobe_json(&tools, &out);
    let v = video_stream(&json);
    assert_eq!(v["codec_name"], "h264");
    assert_eq!(v["width"], W);
    assert_eq!(v["height"], H);

    let (raw, pix_fmt) = build_reference_raw(&preset, N, W, H, synthetic_frame);
    let psnr = psnr_against_reference(&tools, &raw, pix_fmt, W, H, FPS, &out);
    assert!(
        psnr > 30.0,
        "Web H.264 (6Mbps target) PSNR too low: {psnr} dB"
    );

    let _ = std::fs::remove_file(&out);
}

// ── VP9 + alpha (WebM), CAP-021 alpha round-trip ─────────────────────────────

#[test]
fn export_webm_vp9_alpha_e2e_ffprobe_and_alpha_roundtrip() {
    let tools = tools_or_skip!();
    let preset = preset_by_name("WebM VP9 Alpha");
    let out = tmp_path("vp9_alpha.webm");

    run_export(&tools, &preset, &out, N, synthetic_alpha_frame, true);

    let json = ffprobe_json(&tools, &out);
    let v = video_stream(&json);
    assert_eq!(v["codec_name"], "vp9");
    assert_eq!(v["width"], W);
    assert_eq!(v["height"], H);
    assert_eq!(
        v["tags"]["alpha_mode"], "1",
        "VP9 alpha side-channel tag must be present (CAP-021)"
    );

    // Decode frame 0 back to yuva420p and sample the known transparent
    // (x=0) / opaque (x=w-1) columns (05 §8's "sample known
    // transparent/opaque regions, assert exact channel values").
    let raw_out = tmp_path("vp9_alpha_decoded.raw");
    // The alpha channel of a VP9/WebM file is carried as a *side-data* stream
    // (the `alpha_mode=1` tag above). ffmpeg's default/native VP9 decoder
    // silently drops it and returns opaque (alpha=255); only the `libvpx-vp9`
    // decoder exposes the alpha plane. Force it so the round-trip actually
    // sees the encoded alpha (the encoder side is unchanged/correct).
    let status = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-nostdin", "-loglevel", "error"])
        .args(["-c:v", "libvpx-vp9"])
        .arg("-i")
        .arg(&out)
        .args(["-vframes", "1", "-f", "rawvideo", "-pix_fmt", "yuva420p"])
        .arg(&raw_out)
        .status()
        .expect("ffmpeg decode spawns");
    assert!(status.success(), "decode-back failed");
    let bytes = std::fs::read(&raw_out).expect("read decoded frame");
    let _ = std::fs::remove_file(&raw_out);

    // yuva420p layout: Y(w*h), Cb(cw*ch), Cr(cw*ch), A(w*h) — alpha plane is
    // the tail, full resolution.
    let (w, h) = (W as usize, H as usize);
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let a_offset = w * h + 2 * cw * ch;
    let a_plane = &bytes[a_offset..a_offset + w * h];
    let row = h / 2;
    let left = a_plane[row * w] as i32; // shift=0 at frame 0 -> x=0 -> transparent
    let right = a_plane[row * w + (w - 1)] as i32; // x=w-1 -> opaque
    assert!(left < 20, "left edge should be ~transparent, got {left}");
    assert!(right > 235, "right edge should be ~opaque, got {right}");

    let _ = std::fs::remove_file(&out);
}

// ── ProRes 4444 (MOV), alpha on by default ───────────────────────────────────

#[test]
fn export_prores_mezzanine_e2e_ffprobe() {
    let tools = tools_or_skip!();
    let preset = preset_by_name("ProRes Mezzanine");
    let out = tmp_path("prores.mov");

    run_export(&tools, &preset, &out, N, synthetic_prores_ramp_frame, true);

    let json = ffprobe_json(&tools, &out);
    let v = video_stream(&json);
    assert_eq!(v["codec_name"], "prores");
    assert_eq!(v["width"], W);
    assert_eq!(v["height"], H);
    assert_eq!(v["pix_fmt"], "yuva444p12le");
    let decoded = Command::new(&tools.ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(&out)
        .args([
            "-frames:v",
            "1",
            "-pix_fmt",
            "yuva444p12le",
            "-f",
            "rawvideo",
            "-",
        ])
        .output()
        .expect("decode ProRes samples");
    assert!(
        decoded.status.success(),
        "{}",
        String::from_utf8_lossy(&decoded.stderr)
    );
    assert_eq!(decoded.stdout.len(), (W * H * 8) as usize);
    let y_samples = &decoded.stdout[..(W * H * 2) as usize];
    let unique_y = y_samples
        .chunks_exact(2)
        .map(|sample| u16::from_le_bytes([sample[0], sample[1]]))
        .collect::<std::collections::HashSet<_>>();
    assert!(
        unique_y.len() > 256,
        "decoded ProRes has only {} luma codes",
        unique_y.len()
    );
    let alpha_start = (W * H * 6) as usize;
    let alpha = &decoded.stdout[alpha_start..];
    let alpha_at = |x: usize| u16::from_le_bytes([alpha[x * 2], alpha[x * 2 + 1]]);
    assert!(alpha_at(0) < 128, "transparent end gained alpha");
    assert!(alpha_at((W - 1) as usize) > 3967, "opaque end lost alpha");
    let details = probe_details(&tools, &out).expect("probe encoded source");
    let format = PixFmt::for_source(details.pixel_format.as_deref(), details.has_alpha);
    assert_eq!(format, PixFmt::Yuva444p16le);
    let keyframes = KeyframeIndex::build(&tools, &out).expect("keyframe index");
    let params = SourceParams {
        input: out.clone(),
        width: W,
        height: H,
        pix_fmt: format,
        pts_kind: PtsKind::Cfr(FPS),
        keyframes,
    };
    let mut source = DecodeSource::new(tools.clone(), params, SharedRing::preview());
    let frame = source.seek(Tick::ZERO).expect("decode high-depth source");
    let distinct = frame
        .planes
        .y()
        .chunks_exact(2)
        .map(|sample| u16::from_le_bytes([sample[0], sample[1]]))
        .collect::<std::collections::HashSet<_>>();
    assert!(
        distinct.len() > 256,
        "sidecar decoded only {} luma levels",
        distinct.len()
    );
    let decoded_y = frame.planes.y();
    let code_at =
        |pixel: usize| u16::from_le_bytes([decoded_y[pixel * 2], decoded_y[pixel * 2 + 1]]);
    assert!((code_at(0) as i32 - 4096).abs() < 512);
    assert!((code_at((W * H - 1) as usize) as i32 - 60160).abs() < 512);
    let has_pcm = json["streams"].as_array().unwrap().iter().any(|s| {
        s["codec_type"] == "audio" && s["codec_name"].as_str().unwrap_or("").starts_with("pcm")
    });
    assert!(has_pcm, "expected a PCM audio stream");

    let _ = std::fs::remove_file(&out);
}

#[test]
fn export_opaque_prores_hq_decodes_more_than_8_bit_luma() {
    let tools = tools_or_skip!();
    let mut preset = preset_by_name("ProRes Mezzanine");
    preset.alpha = false;
    let out = tmp_path("prores_opaque_hq.mov");
    run_export(&tools, &preset, &out, 1, synthetic_prores_ramp_frame, false);

    let probe = ffprobe_json(&tools, &out);
    let stream = video_stream(&probe);
    assert_eq!(stream["codec_name"], "prores");
    assert_eq!(stream["pix_fmt"], "yuv422p10le");
    let decoded = Command::new(&tools.ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(&out)
        .args([
            "-frames:v",
            "1",
            "-pix_fmt",
            "yuv422p10le",
            "-f",
            "rawvideo",
            "-",
        ])
        .output()
        .expect("decode opaque ProRes samples");
    assert!(
        decoded.status.success(),
        "{}",
        String::from_utf8_lossy(&decoded.stderr)
    );
    let y = &decoded.stdout[..(W * H * 2) as usize];
    let unique = y
        .chunks_exact(2)
        .map(|sample| u16::from_le_bytes([sample[0], sample[1]]))
        .collect::<std::collections::HashSet<_>>();
    assert!(
        unique.len() > 256,
        "opaque ProRes retained only {} luma codes",
        unique.len()
    );
    let _ = std::fs::remove_file(out);
}

#[test]
fn lossless_sixteen_bit_420_decode_retains_subsampled_plane_codes() {
    lossless_sixteen_bit_subsampled_decode_retains_codes("yuv420p16le", PixFmt::Yuv420p16le);
}

#[test]
fn lossless_sixteen_bit_422_decode_retains_subsampled_plane_codes() {
    lossless_sixteen_bit_subsampled_decode_retains_codes("yuv422p16le", PixFmt::Yuv422p16le);
}

#[test]
fn lossless_sixteen_bit_444_decode_retains_full_resolution_plane_codes() {
    lossless_sixteen_bit_subsampled_decode_retains_codes("yuv444p16le", PixFmt::Yuv444p16le);
}

#[test]
fn ten_bit_420_source_expands_codes_without_eight_bit_quantization() {
    ten_bit_source_expands_codes("yuv420p10le", PixFmt::Yuv420p16le, 4);
}

#[test]
fn ten_bit_422_source_expands_codes_without_eight_bit_quantization() {
    ten_bit_source_expands_codes("yuv422p10le", PixFmt::Yuv422p16le, 2);
}

#[test]
fn ten_bit_444_source_expands_codes_without_eight_bit_quantization() {
    ten_bit_source_expands_codes("yuv444p10le", PixFmt::Yuv444p16le, 1);
}

fn ten_bit_source_expands_codes(pixel_format: &str, expected: PixFmt, chroma_divisor: u32) {
    let tools = tools_or_skip!();
    let raw = tmp_path(&format!("native_{pixel_format}.raw"));
    let encoded = tmp_path(&format!("native_{pixel_format}.mkv"));
    let (width, height) = (64u32, 64u32);
    let y: Vec<u16> = (0..width * height)
        .map(|index| 64 + (index % 877) as u16)
        .collect();
    let chroma_len = (width * height / chroma_divisor) as usize;
    let cb: Vec<u16> = (0..chroma_len)
        .map(|index| 64 + (index % 897) as u16)
        .collect();
    let cr = vec![512u16; chroma_len];
    let codes: Vec<u16> = y.iter().chain(&cb).chain(&cr).copied().collect();
    std::fs::write(
        &raw,
        codes
            .iter()
            .flat_map(|code| code.to_le_bytes())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let output = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo"])
        .args(["-pixel_format", pixel_format, "-video_size", "64x64"])
        .args(["-framerate", "10", "-i"])
        .arg(&raw)
        .args(["-frames:v", "1", "-c:v", "ffv1", "-pix_fmt", pixel_format])
        .arg(&encoded)
        .output()
        .expect("encode lossless 10-bit source");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let details = probe_details(&tools, &encoded).unwrap();
    assert_eq!(details.pixel_format.as_deref(), Some(pixel_format));
    assert!(PixFmt::supports_native_source(pixel_format));
    let format = PixFmt::for_source(details.pixel_format.as_deref(), details.has_alpha);
    assert_eq!(format, expected);
    let params = SourceParams {
        input: encoded.clone(),
        width,
        height,
        pix_fmt: format,
        pts_kind: PtsKind::Cfr(FPS),
        keyframes: KeyframeIndex::build(&tools, &encoded).unwrap(),
    };
    let mut decoder = DecodeSource::new(tools, params, SharedRing::preview());
    let frame = decoder.seek(Tick::ZERO).unwrap();
    let decoded: Vec<u16> = frame
        .planes
        .y()
        .iter()
        .chain(frame.planes.cb())
        .chain(frame.planes.cr())
        .copied()
        .collect::<Vec<_>>()
        .chunks_exact(2)
        .map(|sample| u16::from_le_bytes([sample[0], sample[1]]))
        .collect();
    assert_eq!(decoded.len(), codes.len());
    for (source, expanded) in codes.iter().zip(decoded.iter()) {
        assert_eq!(*expanded, *source << 6, "10-bit code {source}");
    }
    if let Some(gpu) = GpuContext::request_blocking() {
        let converter = YuvConverter::new(gpu.device());
        let texture = converter.convert_native(
            gpu.device(),
            gpu.queue(),
            &frame.planes.as_yuv_planes(),
            NativeYuvInput::Bt709Scene,
            Range::Limited,
            NativeChromaLocation::TopLeft,
        );
        let first = read_texture_rgba16f(&gpu, &texture, width, height)[0];
        let reference = decode_video_sample_to_ap1(
            [y[0] << 6, cb[0] << 6, cr[0] << 6],
            16,
            true,
            NativeVideoInput::Bt709,
        )
        .unwrap();
        for channel in 0..3 {
            assert!((f64::from(first[channel]) - reference[channel]).abs() < 0.004);
        }
    }
    let _ = std::fs::remove_file(raw);
    let _ = std::fs::remove_file(encoded);
}

#[test]
fn lossless_eight_bit_422_decode_keeps_full_height_chroma() {
    lossless_eight_bit_decode_keeps_native_chroma("yuv422p", PixFmt::Yuv422p);
}

#[test]
fn lossless_eight_bit_444_decode_keeps_full_resolution_chroma() {
    lossless_eight_bit_decode_keeps_native_chroma("yuv444p", PixFmt::Yuv444p);
}

fn lossless_eight_bit_decode_keeps_native_chroma(pixel_format: &str, expected: PixFmt) {
    let tools = tools_or_skip!();
    let raw = tmp_path(&format!("native_{pixel_format}.raw"));
    let encoded = tmp_path(&format!("native_{pixel_format}.mkv"));
    let (width, height) = (64u32, 64u32);
    let chroma_samples = width * height / if expected == PixFmt::Yuv422p { 2 } else { 1 };
    let y = vec![128_u8; (width * height) as usize];
    let cb: Vec<u8> = (0..chroma_samples)
        .map(|index| [16_u8, 128, 240][index as usize % 3])
        .collect();
    let cr = vec![128_u8; chroma_samples as usize];
    std::fs::write(&raw, [y.as_slice(), cb.as_slice(), cr.as_slice()].concat()).unwrap();
    let output = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo"])
        .args(["-pixel_format", pixel_format, "-video_size", "64x64"])
        .args(["-framerate", "10", "-i"])
        .arg(&raw)
        .args(["-frames:v", "1", "-c:v", "ffv1", "-pix_fmt", pixel_format])
        .arg(&encoded)
        .output()
        .expect("encode lossless 8-bit fixture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let details = probe_details(&tools, &encoded).expect("probe lossless source");
    let format = PixFmt::for_source(details.pixel_format.as_deref(), details.has_alpha);
    assert_eq!(format, expected);
    let keyframes = KeyframeIndex::build(&tools, &encoded).expect("keyframe index");
    let params = SourceParams {
        input: encoded.clone(),
        width,
        height,
        pix_fmt: format,
        pts_kind: PtsKind::Cfr(FPS),
        keyframes,
    };
    let mut decoder = DecodeSource::new(tools, params, SharedRing::preview());
    let frame = decoder.seek(Tick::ZERO).expect("decode 8-bit source");
    assert!(matches!(
        (&frame.planes, expected),
        (
            photonic_video::decode::DecodedPlanes::Yuv422 { .. },
            PixFmt::Yuv422p
        ) | (
            photonic_video::decode::DecodedPlanes::Yuv444 { .. },
            PixFmt::Yuv444p
        )
    ));
    assert_eq!(frame.planes.y(), y);
    assert_eq!(frame.planes.cb(), cb);
    assert_eq!(frame.planes.cr(), cr);
    let _ = std::fs::remove_file(raw);
    let _ = std::fs::remove_file(encoded);
}

fn lossless_sixteen_bit_subsampled_decode_retains_codes(pixel_format: &str, expected: PixFmt) {
    let tools = tools_or_skip!();
    let raw = tmp_path(&format!("native_{pixel_format}.raw"));
    let encoded = tmp_path(&format!("native_{pixel_format}.mkv"));
    let (width, height) = (64u32, 64u32);
    let chroma_divisor = match expected {
        PixFmt::Yuv420p16le => 4,
        PixFmt::Yuv422p16le => 2,
        PixFmt::Yuv444p16le => 1,
        _ => unreachable!("16-bit planar fixture uses a supported sampling layout"),
    };
    let chroma_samples = (width * height / chroma_divisor) as usize;
    let y: Vec<u16> = (0..width * height)
        .map(|index| 4096 + ((index * 13) % 56064) as u16)
        .collect();
    let cb: Vec<u16> = (0..chroma_samples)
        .map(|index| [4096_u16, 32768, 60160, 4097][index % 4])
        .collect();
    let cr = vec![32768_u16; chroma_samples];
    let source_codes: Vec<u16> = y.iter().chain(&cb).chain(&cr).copied().collect();
    let source: Vec<u8> = source_codes
        .iter()
        .flat_map(|code| code.to_le_bytes())
        .collect();
    std::fs::write(&raw, &source).expect("write native subsampled source");
    let output = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo"])
        .args(["-pixel_format", pixel_format, "-video_size", "64x64"])
        .args(["-framerate", "10", "-i"])
        .arg(&raw)
        .args([
            "-frames:v",
            "1",
            "-c:v",
            "ffv1",
            "-slices",
            "4",
            "-pix_fmt",
            pixel_format,
        ])
        .arg(&encoded)
        .output()
        .expect("encode lossless 16-bit subsampled fixture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let details = probe_details(&tools, &encoded).expect("probe lossless source");
    let format = PixFmt::for_source(details.pixel_format.as_deref(), details.has_alpha);
    assert_eq!(format, expected);
    let keyframes = KeyframeIndex::build(&tools, &encoded).expect("keyframe index");
    let params = SourceParams {
        input: encoded.clone(),
        width,
        height,
        pix_fmt: format,
        pts_kind: PtsKind::Cfr(FPS),
        keyframes,
    };
    let mut decoder = DecodeSource::new(tools, params, SharedRing::preview());
    let frame = decoder
        .seek(Tick::ZERO)
        .expect("decode 16-bit 4:2:0 source");
    assert!(matches!(
        (&frame.planes, expected),
        (
            photonic_video::decode::DecodedPlanes::Yuv420P16 { .. },
            PixFmt::Yuv420p16le
        ) | (
            photonic_video::decode::DecodedPlanes::Yuv422P16 { .. },
            PixFmt::Yuv422p16le
        ) | (
            photonic_video::decode::DecodedPlanes::Yuv444P16 { .. },
            PixFmt::Yuv444p16le
        )
    ));
    let decoded: Vec<u16> = frame
        .planes
        .y()
        .iter()
        .chain(frame.planes.cb())
        .chain(frame.planes.cr())
        .copied()
        .collect::<Vec<_>>()
        .chunks_exact(2)
        .map(|sample| u16::from_le_bytes([sample[0], sample[1]]))
        .collect();
    assert_eq!(decoded, source_codes);
    if let Some(gpu) = GpuContext::request_blocking() {
        let converter = YuvConverter::new(gpu.device());
        let texture = converter.convert_native(
            gpu.device(),
            gpu.queue(),
            &frame.planes.as_yuv_planes(),
            NativeYuvInput::Bt709Scene,
            Range::Limited,
            NativeChromaLocation::TopLeft,
        );
        let first = read_texture_rgba16f(&gpu, &texture, width, height)[0];
        let expected =
            decode_video_sample_to_ap1([y[0], cb[0], cr[0]], 16, true, NativeVideoInput::Bt709)
                .unwrap();
        for channel in 0..3 {
            assert!((f64::from(first[channel]) - expected[channel]).abs() < 0.004);
        }
    }
    let _ = std::fs::remove_file(raw);
    let _ = std::fs::remove_file(encoded);
}

// ── GIF (paletted) ────────────────────────────────────────────────────────────

#[test]
fn export_gif_e2e_ffprobe() {
    let tools = tools_or_skip!();
    let preset = preset_by_name("GIF");
    let out = tmp_path("out.gif");

    run_export(&tools, &preset, &out, N, synthetic_frame, false);

    let json = ffprobe_json(&tools, &out);
    let v = video_stream(&json);
    assert_eq!(v["codec_name"], "gif");
    assert_eq!(v["width"], W);
    assert_eq!(v["height"], H);

    let _ = std::fs::remove_file(&out);
}

// ── PNG Sequence (always-on alpha, straight, lossless round-trip) ───────────

#[test]
fn export_png_sequence_e2e_alpha_roundtrip() {
    let tools = tools_or_skip!();
    let preset = preset_by_name("PNG Sequence");
    let dir = tmp_path("pngseq_dir");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out_pattern = dir.join("frame_%06d.png");

    run_export(
        &tools,
        &preset,
        &out_pattern,
        N,
        synthetic_alpha_frame,
        false,
    );

    let files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "png").unwrap_or(false))
        .collect();
    assert_eq!(files.len(), N as usize, "one PNG per frame");

    // Decode frame 1 back to straight rgba and check the alpha ramp exactly
    // (PNG is lossless, so this should be near bit-exact modulo our own
    // quantization, not lossy-codec noise).
    let frame1 = dir.join("frame_000001.png");
    let raw_out = tmp_path("pngseq_decoded.raw");
    let status = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-nostdin", "-loglevel", "error"])
        .arg("-i")
        .arg(&frame1)
        .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
        .arg(&raw_out)
        .status()
        .expect("ffmpeg decode spawns");
    assert!(status.success());
    let decoded = std::fs::read(&raw_out).expect("read decoded png");
    let _ = std::fs::remove_file(&raw_out);

    let reference = working_frame_to_rgba8(&synthetic_alpha_frame(0, W, H).rgba_premult, W, H);
    let ref_bytes = reference.to_bytes();
    assert_eq!(decoded.len(), ref_bytes.len());
    let mut max_diff = 0i32;
    for (a, b) in decoded.iter().zip(ref_bytes.iter()) {
        max_diff = max_diff.max((*a as i32 - *b as i32).abs());
    }
    assert!(
        max_diff <= 2,
        "PNG round-trip should be ~bit-exact, max channel diff {max_diff}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
