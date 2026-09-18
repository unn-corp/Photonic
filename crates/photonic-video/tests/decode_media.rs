//! Integration tests for P3 decode/media against the committed fixture corpus
//! (`tests/fixtures/`, documented in `tests/fixtures/README.md`).
//!
//! Every test that shells out to ffmpeg **skips with a message** when the
//! toolchain is absent (mirroring the GPU-adapter skip convention in
//! `pool.rs`), so the suite is green on a machine without ffmpeg and exercises
//! real decode where ffmpeg is present (locally + CI).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use photonic_core::timeline::{FrameRate, TICKS_PER_SECOND};
use photonic_video::decode::scheduler::{PtsKind, SourceParams};
use photonic_video::decode::{DecodeSource, DecodedPlanes, PixFmt, SharedRing};
use photonic_video::media::ffmpeg_locate::{locate_for_test, FfmpegTools};
use photonic_video::media::keyframe_index::{KeyframeIndex, PtsIndex};
use photonic_video::media::probe::probe_details;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fixture(name: &str) -> PathBuf {
    fixtures_dir().join(name)
}

/// Resolve ffmpeg or skip the calling test with a printed message (mirrors the
/// GPU-adapter skip convention in `pool.rs`).
macro_rules! tools_or_skip {
    () => {
        match locate_for_test() {
            Some(t) => t,
            None => {
                eprintln!(
                    "ffmpeg/ffprobe not found — skipping decode/media test at {}:{} \
                     (set PHOTONIC_FFMPEG_DIR or install ffmpeg)",
                    file!(),
                    line!()
                );
                return;
            }
        }
    };
}

const TOL_1S_PROBE: i64 = TICKS_PER_SECOND / 10; // 0.1s tolerance for durations

// ── 1. Probe per fixture (README's documented properties) ───────────────────

#[test]
fn probe_color_bars_matches_readme() {
    let tools: FfmpegTools = tools_or_skip!();
    let d = probe_details(&tools, &fixture("color_bars.mp4")).expect("probe color_bars");
    let v = d.probe.video.expect("has video stream");
    assert_eq!(v.width, 320);
    assert_eq!(v.height, 180);
    assert_eq!(v.frame_rate, FrameRate { num: 30, den: 1 });
    assert_eq!(d.probe.codec, "h264");
    assert_eq!(d.pixel_format.as_deref(), Some("yuv420p"));
    assert!(!d.has_alpha);
    // duration ~4s.
    assert!(
        (d.probe.duration.0 - 4 * TICKS_PER_SECOND).abs() < TOL_1S_PROBE,
        "duration {:?} not ~4s",
        d.probe.duration
    );
}

#[test]
fn probe_counter_matches_readme() {
    let tools = tools_or_skip!();
    let d = probe_details(&tools, &fixture("counter.mp4")).expect("probe counter");
    let v = d.probe.video.expect("has video stream");
    assert_eq!((v.width, v.height), (320, 180));
    assert_eq!(v.frame_rate, FrameRate { num: 30, den: 1 });
    assert_eq!(d.probe.codec, "h264");
    assert!(!d.is_vfr, "counter is CFR");
    assert!((d.probe.duration.0 - 10 * TICKS_PER_SECOND).abs() < TOL_1S_PROBE);
}

#[test]
fn probe_alpha_gradient_reports_alpha() {
    let tools = tools_or_skip!();
    let d = probe_details(&tools, &fixture("alpha_gradient.mov")).expect("probe alpha_gradient");
    let v = d.probe.video.expect("has video stream");
    assert_eq!((v.width, v.height), (160, 90));
    assert_eq!(d.probe.codec, "qtrle");
    assert_eq!(d.pixel_format.as_deref(), Some("argb"));
    assert!(d.has_alpha, "argb source carries alpha");
    assert_eq!(PixFmt::for_alpha(d.has_alpha), PixFmt::Yuva444p);
}

#[test]
fn probe_multi_audio_reads_first_audio_stream() {
    let tools = tools_or_skip!();
    let d = probe_details(&tools, &fixture("multi_audio.mkv")).expect("probe multi_audio");
    assert!(d.probe.video.is_none(), "audio-only container");
    let a = d.probe.audio.expect("has audio stream");
    assert_eq!(a.sample_rate, 48_000);
    assert_eq!(a.channels, 1, "first stream (dialogue) is mono");
    assert_eq!(a.codec, "opus");
    assert!(d.probe.container.contains("matroska"));
}

#[test]
fn probe_vfr_sample_is_flagged_variable() {
    let tools = tools_or_skip!();
    let d = probe_details(&tools, &fixture("vfr_sample.mp4")).expect("probe vfr_sample");
    // r_frame_rate is the container's rational approximation (24/1); the true
    // average differs, so the VFR heuristic fires.
    assert!(d.is_vfr, "vfr_sample should be flagged VFR");
}

// ── 2. Keyframe index (counter.mp4: keyframes at frames 0/60/120/180/240) ────

#[test]
fn keyframe_index_counter_matches_readme() {
    let tools = tools_or_skip!();
    let idx = KeyframeIndex::build(&tools, &fixture("counter.mp4")).expect("build keyframe index");
    let rate = FrameRate { num: 30, den: 1 };
    // Convert each keyframe pts to a frame index and compare to the README.
    let frames: Vec<i64> = idx.keyframes.iter().map(|t| rate.frame_at(*t)).collect();
    assert_eq!(
        frames,
        vec![0, 60, 120, 180, 240],
        "keyframe frame indices (README: GOP=60)"
    );
    // keyframe_before a mid-GOP tick (frame 75 = 2.5s) is the frame-60 keyframe.
    let tpf = rate.ticks_per_frame().0;
    let kf = idx.keyframe_before(photonic_core::timeline::Tick(75 * tpf));
    assert_eq!(rate.frame_at(kf), 60);
}

// ── 3. Decode: seek counter.mp4 to a mid-GOP frame; pts is exact ─────────────

#[test]
fn decode_counter_mid_gop_seek_is_pts_exact() {
    let tools = tools_or_skip!();
    let path = fixture("counter.mp4");
    let idx = KeyframeIndex::build(&tools, &path).expect("keyframe index");
    let rate = FrameRate { num: 30, den: 1 };
    let tpf = rate.ticks_per_frame().0;

    let params = SourceParams {
        input: path,
        width: 320,
        height: 180,
        pix_fmt: PixFmt::Yuv420p,
        pts_kind: PtsKind::Cfr(rate),
        keyframes: idx,
    };
    let ring = SharedRing::preview();
    let mut src = DecodeSource::new(tools, params, ring);

    // Seek to frame 75 (mid-GOP: keyframe_before is frame 60).
    let target = photonic_core::timeline::Tick(75 * tpf);
    let t0 = Instant::now();
    let f75 = src.seek(target).expect("seek to frame 75");
    let cold_seek = t0.elapsed();
    eprintln!(
        "cold seek counter.mp4 → frame 75 (mid-GOP, decode-forward from frame 60): {cold_seek:?}"
    );

    // pts-exact: the decoded frame's presentation tick equals frame 75's tick.
    assert_eq!(f75.pts, target, "mid-GOP seek lands pts-exact on frame 75");
    assert_eq!(f75.planes.dims(), (320, 180));
    assert!(matches!(&f75.planes, DecodedPlanes::Yuv420 { .. }));
    assert_eq!(f75.planes.y().len(), 320 * 180);
    assert_eq!(f75.planes.cb().len(), 160 * 90);
    assert_eq!(f75.planes.cr().len(), 160 * 90);

    // Frame-accuracy: an adjacent frame differs; re-seeking is deterministic.
    let f76 = src
        .seek(photonic_core::timeline::Tick(76 * tpf))
        .expect("seek 76");
    assert_ne!(
        plane_y(&f75.planes),
        plane_y(&f76.planes),
        "adjacent counter frames differ (burn-in advances)"
    );
    let f75_again = src.seek(target).expect("re-seek 75");
    assert_eq!(
        plane_y(&f75.planes),
        plane_y(&f75_again.planes),
        "re-seeking the same frame is deterministic (SS-3)"
    );

    // Sequential fill: pump a few frames and confirm monotonically increasing pts.
    src.seek(target).unwrap();
    let n = src.pump(4).expect("pump");
    assert!(n >= 1, "pump decoded at least one forward frame");
    let ring = src.ring();
    let next = ring
        .frame_covering(photonic_core::timeline::Tick(76 * tpf))
        .expect("frame 76 in ring after pump");
    assert_eq!(next.pts, photonic_core::timeline::Tick(76 * tpf));
}

// ── 4. VFR: pts spacing changes at the documented 24→30 fps boundary ─────────

#[test]
fn vfr_pts_index_spacing_changes_at_boundary() {
    let tools = tools_or_skip!();
    let idx = PtsIndex::build(&tools, &fixture("vfr_sample.mp4")).expect("build pts index");
    // README: 96 frames @24fps then 120 @30fps = 216 total.
    assert_eq!(idx.pts.len(), 216, "total frame count");

    let tpf_24 = FrameRate { num: 24, den: 1 }.ticks_per_frame().0; // 29_400_000
    let tpf_30 = FrameRate { num: 30, den: 1 }.ticks_per_frame().0; // 23_520_000
    assert_eq!(tpf_24, 29_400_000);
    assert_eq!(tpf_30, 23_520_000);

    let delta = |i: usize| idx.pts[i].0 - idx.pts[i - 1].0;

    // Container timestamps carry a small timebase-rounding jitter (sub-µs), so
    // classify each interval against the midpoint between the two rates rather
    // than demanding an exact tick match, and require it near its nominal.
    let midpoint = (tpf_24 + tpf_30) / 2;
    let near = |got: i64, nominal: i64| (got - nominal).abs() < TICKS_PER_SECOND / 1000; // <1ms
    let is_24 = |i: usize| delta(i) > midpoint;

    // Inside the first (24fps) segment.
    assert!(
        is_24(1) && near(delta(1), tpf_24),
        "first-segment spacing ~1/24 s (got {})",
        delta(1)
    );
    assert!(is_24(50) && near(delta(50), tpf_24));
    // Second (30fps) segment. The concat boundary places frame 96 at exactly
    // 4.0s (== 96/24), so the interval that shortens is frame 96→97 onward.
    assert!(
        !is_24(97) && near(delta(97), tpf_30),
        "second-segment spacing ~1/30 s (got {})",
        delta(97)
    );
    assert!(!is_24(150) && near(delta(150), tpf_30));

    // The first interval classified as 30fps is the documented boundary (96→97).
    let first_change = (1..idx.pts.len())
        .find(|&i| !is_24(i))
        .expect("a spacing change exists");
    assert_eq!(
        first_change, 97,
        "spacing changes at the 24→30 fps boundary (frame 96→97)"
    );
}

#[test]
fn decode_vfr_is_pts_true_via_table() {
    let tools = tools_or_skip!();
    let path = fixture("vfr_sample.mp4");
    let pts_index = Arc::new(PtsIndex::build(&tools, &path).expect("pts index"));
    let kf_index = KeyframeIndex::build(&tools, &path).expect("keyframe index");

    // Target a frame inside the 30fps region (frame 150).
    let target = pts_index.pts[150];
    // The pts-true expectation: the smallest table entry >= target.
    let expected = *pts_index
        .pts
        .iter()
        .find(|t| **t >= target)
        .expect("a frame at/after target");

    let params = SourceParams {
        input: path,
        width: 320,
        height: 180,
        pix_fmt: PixFmt::Yuv420p,
        pts_kind: PtsKind::Vfr(Arc::clone(&pts_index)),
        keyframes: kf_index,
    };
    let mut src = DecodeSource::new(tools, params, SharedRing::preview());
    let frame = src.seek(target).expect("VFR seek");

    assert_eq!(
        frame.pts, expected,
        "VFR decode assigns pts from the ground-truth table (pts-true), not a constant rate"
    );
    assert!(frame.pts >= target);
}

// ── 5. Alpha: yuva path, alpha ramps left→right (alpha_gradient.mov) ─────────

#[test]
fn decode_alpha_gradient_ramps_across_row() {
    let tools = tools_or_skip!();
    let path = fixture("alpha_gradient.mov");
    let d = probe_details(&tools, &path).expect("probe");
    assert!(d.has_alpha);

    let idx = KeyframeIndex::build(&tools, &path).expect("keyframe index");
    let params = SourceParams {
        input: path,
        width: 160,
        height: 90,
        pix_fmt: PixFmt::for_alpha(d.has_alpha), // Yuva444p
        pts_kind: PtsKind::Cfr(FrameRate { num: 30, den: 1 }),
        keyframes: idx,
    };
    let mut src = DecodeSource::new(tools, params, SharedRing::preview());

    // Decode frame 0.
    let f0 = src
        .seek(photonic_core::timeline::Tick(0))
        .expect("seek frame 0");
    let (a, w) = match &f0.planes {
        DecodedPlanes::Yuva444 { .. } => {
            assert_eq!(f0.planes.dims(), (160, 90));
            (
                f0.planes.a().expect("YUVA frame has alpha").to_vec(),
                160usize,
            )
        }
        _ => panic!("expected yuva444 planes for an alpha source"),
    };

    // README frame 0 (straight alpha): x=0 → 0x00, x=80 → ~0x7f, x=159 → ~0xfd.
    // Sample a middle row (row 45) to avoid any edge ambiguity.
    let row = 45usize;
    let at = |x: usize| a[row * w + x] as i32;
    assert!(at(0) < 10, "left edge ~transparent, got {}", at(0));
    assert!((at(80) - 127).abs() <= 6, "mid ~half alpha, got {}", at(80));
    assert!(at(159) > 240, "right edge ~opaque, got {}", at(159));
    // Monotonic ramp across the row.
    assert!(at(0) < at(80) && at(80) < at(159), "alpha ramps left→right");
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn plane_y(p: &DecodedPlanes) -> &[u8] {
    p.y()
}
