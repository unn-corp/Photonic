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

use photonic_core::timeline::FrameRate;
use photonic_render::color::Colorimetry;
use photonic_video::export::convert::{working_frame_to_rgba8, working_frame_to_yuv_planes};
use photonic_video::export::encoder::{
    plane_kind_for, AudioStreamSpec, EncoderCapabilities, PlaneKind,
};
use photonic_video::export::presets::built_in_presets;
use photonic_video::export::render_loop::{export_frames, ExportEvent, Frame, ResolvedExport};
use photonic_video::media::ffmpeg_locate::{locate_for_test, FfmpegTools};

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

    run_export(&tools, &preset, &out, N, synthetic_alpha_frame, true);

    let json = ffprobe_json(&tools, &out);
    let v = video_stream(&json);
    assert_eq!(v["codec_name"], "prores");
    assert_eq!(v["width"], W);
    assert_eq!(v["height"], H);
    assert!(
        v["pix_fmt"]
            .as_str()
            .unwrap_or_default()
            .starts_with("yuva"),
        "expected an alpha-carrying pix_fmt, got {:?}",
        v["pix_fmt"]
    );
    let has_pcm = json["streams"].as_array().unwrap().iter().any(|s| {
        s["codec_type"] == "audio" && s["codec_name"].as_str().unwrap_or("").starts_with("pcm")
    });
    assert!(has_pcm, "expected a PCM audio stream");

    let _ = std::fs::remove_file(&out);
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
