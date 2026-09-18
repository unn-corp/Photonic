//! Joins the two halves of the video pipeline over **real media**: decode a
//! committed fixture, carry it through the working colour space, and hand it
//! to the P4 export engine.
//!
//! `tests/decode_media.rs` covers decode alone; `tests/export_synthetic.rs`
//! covers encode alone and says so explicitly — "run against **synthetic**
//! frames only ... these tests never touch the frame-graph/decode path",
//! because the evaluator was a concurrent build when it was written. Nothing
//! joined them, so a decoded frame had never been fed to the encoder.
//!
//! **What guards what.** `export::convert` documents itself as "the exact
//! inverse of [`photonic_render::color::yuv_to_working`]", and that claim is
//! testable to the byte — so `colour_contract_is_exact_inverse_on_real_media`
//! asserts it exactly, with no encoder in the loop. That precision is the
//! point: measured on this fixture, a range bug (Full read as Limited) leaves
//! a max luma error of 20 codes and a matrix bug (601 for 709) leaves 30,
//! while the correct transform leaves **0**.
//!
//! The end-to-end test deliberately does **not** police colour with PSNR.
//! A correct round trip through a second h264 generation measures ~26 dB on
//! this fixture — *below* the ~28.7 dB a range bug produces at the conversion
//! stage — so encoder loss swamps the defect signal and any threshold that
//! passed a clean run would also pass a broken transform. It asserts the
//! structural contract instead (the seam runs, and the container describes the
//! clip we asked for), and leaves colour to the exact test above.
//!
//! Both skip with a message when ffmpeg is absent, matching the sibling files.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use photonic_core::timeline::{FrameRate, Tick};
use photonic_render::color::{yuv_to_working, Colorimetry};
use photonic_video::decode::scheduler::{PtsKind, SourceParams};
use photonic_video::decode::{DecodeSource, DecodedFrame, DecodedPlanes, PixFmt, SharedRing};
use photonic_video::export::convert::{working_frame_to_yuv_planes, EncodePlanes};
use photonic_video::export::presets::built_in_presets;
use photonic_video::export::render_loop::{export_frames, ExportEvent, Frame, ResolvedExport};
use photonic_video::media::ffmpeg_locate::{locate_for_test, FfmpegTools};
use photonic_video::media::keyframe_index::KeyframeIndex;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

macro_rules! tools_or_skip {
    () => {
        match locate_for_test() {
            Some(t) => t,
            None => {
                eprintln!(
                    "ffmpeg/ffprobe not found — skipping decode→export test at {}:{} \
                     (set PHOTONIC_FFMPEG_DIR or install ffmpeg)",
                    file!(),
                    line!()
                );
                return;
            }
        }
    };
}

/// counter.mp4, per `tests/fixtures/frame_truth.json` + README.
const W: u32 = 320;
const H: u32 = 180;
const FPS: FrameRate = FrameRate { num: 30, den: 1 };
/// One second — enough to span real inter-frame prediction while staying
/// inside counter.mp4's first GOP (GOP size 60), so the test stays quick.
const N_FRAMES: u64 = 30;

/// Decode `counter.mp4` frame `n` (0-based).
fn decode_frame(tools: &FfmpegTools, n: u64) -> DecodedFrame {
    let src = fixture("counter.mp4");
    let keyframes = KeyframeIndex::build(tools, &src).expect("keyframe index");
    let params = SourceParams {
        input: src,
        width: W,
        height: H,
        pix_fmt: PixFmt::Yuv420p,
        pts_kind: PtsKind::Cfr(FPS),
        keyframes,
    };
    let mut source = DecodeSource::new(tools.clone(), params, SharedRing::preview());
    let tick = Tick(n as i64 * FPS.ticks_per_frame().0);
    (*source.seek(tick).expect("decode frame")).clone()
}

/// Decoded YUV 4:2:0 → the export engine's working format (linear,
/// premultiplied RGBA f32). Chroma is 4:2:0, so one chroma sample covers a
/// 2x2 luma quad; this replicates it rather than interpolating, which is what
/// makes the inverse below exact per-pixel.
fn planes_to_working_frame(planes: &DecodedPlanes) -> Frame {
    let DecodedPlanes::Yuv420 { .. } = planes else {
        panic!("counter.mp4 is yuv420p; got a non-4:2:0 plane set");
    };
    let (width, height) = planes.dims();
    let (w, h) = (width as usize, height as usize);
    let (y, cb, cr) = (planes.y(), planes.cb(), planes.cr());
    let cw = w.div_ceil(2);
    let mut rgba = vec![0.0f32; w * h * 4];
    for row in 0..h {
        for col in 0..w {
            let ci = (row / 2) * cw + (col / 2);
            let px = yuv_to_working(
                y[row * w + col] as f32 / 255.0,
                cb[ci] as f32 / 255.0,
                cr[ci] as f32 / 255.0,
                1.0,
                Colorimetry::BT709_LIMITED,
            );
            rgba[(row * w + col) * 4..(row * w + col) * 4 + 4].copy_from_slice(&px);
        }
    }
    Frame {
        width,
        height,
        rgba_premult: rgba,
    }
}

/// The documented contract, asserted to the byte on a real decoded h264 frame:
/// `working_frame_to_yuv_planes` is the exact inverse of `yuv_to_working`.
///
/// This is the colour guard for the decode→export seam. It is deterministic
/// and has no encoder in the loop, so it discriminates cleanly — measured on
/// this fixture, a Full/Limited range swap leaves a max luma error of 20 codes
/// and a BT.601/BT.709 matrix swap leaves 30, against 0 when correct.
#[test]
fn colour_contract_is_exact_inverse_on_real_media() {
    let tools: FfmpegTools = tools_or_skip!();
    let frame = decode_frame(&tools, 0);
    let DecodedPlanes::Yuv420 { .. } = &frame.planes else {
        panic!("counter.mp4 is yuv420p");
    };
    let (y, cb, cr) = (frame.planes.y(), frame.planes.cb(), frame.planes.cr());

    let working = planes_to_working_frame(&frame.planes);
    let planes = working_frame_to_yuv_planes(
        &working.rgba_premult,
        W,
        H,
        Colorimetry::BT709_LIMITED,
        false,
        false,
    );
    let EncodePlanes::Yuv420 {
        y: y2,
        cb: cb2,
        cr: cr2,
        ..
    } = &planes
    else {
        panic!("H.264 delivery is 4:2:0 without alpha");
    };

    for (name, src, out) in [
        ("luma", y, y2),
        ("chroma Cb", cb, cb2),
        ("chroma Cr", cr, cr2),
    ] {
        assert_eq!(src.len(), out.len(), "{name} plane length changed");
        let worst = src
            .iter()
            .zip(out.iter())
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap_or(0);
        assert_eq!(
            worst, 0,
            "{name} is not byte-exact after yuv → working → yuv (max error {worst} codes); \
             convert.rs claims to be the exact inverse of yuv_to_working"
        );
    }
}

/// The seam itself: real decoded frames survive the trip into the export
/// encoder and produce a file describing the clip we asked for.
///
/// Structural only — see this file's module docs for why colour is guarded by
/// the exact test above rather than by PSNR here.
#[test]
fn decoded_media_exports_to_a_well_formed_clip() {
    let tools: FfmpegTools = tools_or_skip!();

    let src = fixture("counter.mp4");
    let keyframes = KeyframeIndex::build(&tools, &src).expect("keyframe index");
    let tpf = FPS.ticks_per_frame().0;
    let params = SourceParams {
        input: src,
        width: W,
        height: H,
        pix_fmt: PixFmt::Yuv420p,
        pts_kind: PtsKind::Cfr(FPS),
        keyframes,
    };
    let mut source = DecodeSource::new(tools.clone(), params, SharedRing::preview());

    // Decode up front: `export_frames` may pull a frame more than once and
    // must always get the same pixels.
    let frames: Vec<Frame> = (0..N_FRAMES)
        .map(|i| {
            let f = source
                .seek(Tick(i as i64 * tpf))
                .unwrap_or_else(|e| panic!("decode frame {i}: {e:?}"));
            assert_eq!(f.planes.dims(), (W, H), "frame {i} dims");
            planes_to_working_frame(&f.planes)
        })
        .collect();

    let preset = built_in_presets()
        .into_iter()
        .find(|p| p.name == "Web H.264")
        .expect("built-in Web H.264 preset");

    let out_path = std::env::temp_dir().join("photonic_decode_export_roundtrip.mp4");
    let _ = std::fs::remove_file(&out_path);
    let resolved = ResolvedExport {
        width: W,
        height: H,
        frame_rate: FPS,
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
    let mut done = false;
    export_frames(
        &tools,
        &preset,
        &resolved,
        N_FRAMES,
        |i| frames[i as usize].clone(),
        None,
        &cancel,
        |ev| {
            if matches!(ev, ExportEvent::Done) {
                done = true;
            }
        },
    )
    .expect("export decoded frames");
    assert!(done, "export never reported Done");
    assert!(out_path.exists(), "export produced no file");

    let probe = Command::new(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,nb_frames,r_frame_rate,codec_name",
            "-of",
            "default=nw=1",
        ])
        .arg(&out_path)
        .output()
        .expect("ffprobe the export");
    let info = String::from_utf8_lossy(&probe.stdout);
    for expected in [
        "codec_name=h264",
        "width=320",
        "height=180",
        "r_frame_rate=30/1",
        &format!("nb_frames={N_FRAMES}"),
    ] {
        assert!(
            info.contains(expected),
            "exported clip missing {expected:?} in:\n{info}"
        );
    }

    let _ = std::fs::remove_file(&out_path);
}
