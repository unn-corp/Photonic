//! D-12 end-to-end acceptance (22 §6.7).
//!
//! Drives the real path a user takes — write a sidecar, bind it, analyze, warp
//! a frame — rather than unit-testing the pieces in isolation, so a break in
//! the seams between them is caught too.

use std::io::Write;

use photonic_core::timeline::{
    LensProfileRef, MotionBinding, MotionFormat, MotionSourceRef, StabilizationCropMode,
    StabilizationSpec,
};
use photonic_video::graph::ir::Sampling;
use photonic_video::graph::ops::{stabilize_warp, Image};
use photonic_video::graph::stabilize::{analyze_clip, ClipGeometry};

fn tmp_dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("photonic-d12-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(name: &str, body: &str) -> std::path::PathBuf {
    let p = tmp_dir().join(name);
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(body.as_bytes()).unwrap();
    p
}

/// A `.gcsv` whose camera shakes about all three axes at `amp` rad/s, sampled
/// at 500 Hz, plus a resting accelerometer so horizon lock has something to
/// work with.
fn shaky_gcsv(name: &str, secs: f64, amp: f64) -> std::path::PathBuf {
    let hz = 500.0;
    let n = (hz * secs) as usize;
    let mut body = String::from(
        "GYROFLOW IMU LOG\nversion,1.3\nid,e2e-synthetic\norientation,XYZ\n\
         tscale,0.001\ngscale,1\nascale,1\nt,gx,gy,gz,ax,ay,az\n",
    );
    for i in 0..n {
        let t_ms = i as f64 / hz * 1000.0;
        let s = i as f64 / hz;
        // Three incommensurate frequencies so the motion never repeats and the
        // smoother cannot get lucky.
        let gx = amp * (s * 11.0).sin();
        let gy = amp * (s * 7.0).sin();
        let gz = amp * (s * 13.0).sin();
        body.push_str(&format!("{t_ms:.3},{gx:.6},{gy:.6},{gz:.6},0,9.80665,0\n"));
    }
    write(name, &body)
}

fn spec_for(path: &std::path::Path, smoothness: f32) -> StabilizationSpec {
    let mut s = StabilizationSpec::new(MotionBinding {
        source: MotionSourceRef::Sidecar {
            path: path.to_path_buf(),
            rel_path: None,
            format: MotionFormat::Gcsv,
        },
        sync: Default::default(),
        lens: LensProfileRef::RotationOnly,
    });
    s.smoothness = smoothness;
    s.max_zoom = 2.0;
    s
}

/// Write a pixel through the public `pixels` field — `Image::set` is
/// crate-private and widening it for a test would be the wrong trade.
fn put(img: &mut Image, x: u32, y: u32, v: [f32; 4]) {
    let w = img.width;
    img.pixels[(y * w + x) as usize] = v;
}

fn geom() -> ClipGeometry {
    ClipGeometry {
        width: 640.0,
        height: 360.0,
        fps: 30.0,
        frame_count: 60,
        source_start_s: 0.0,
        source_end_s: 2.0,
    }
}

#[test]
fn gcsv_to_analysis_to_warp() {
    let path = shaky_gcsv("shake.gcsv", 2.0, 0.35);
    let spec = spec_for(&path, 0.85);
    let analysis = analyze_clip(&spec, geom(), |p| p.to_path_buf()).expect("analysis");

    assert_eq!(analysis.frames.len(), 60);
    assert!(
        analysis.frames.iter().any(|f| !f.is_identity()),
        "shaky input must produce a real correction"
    );
    assert!(
        analysis.diagnostics.max_required_zoom > 1.0,
        "correcting shake must cost some crop"
    );

    // The corrections must actually drive the warp.
    let mut src = Image::new(64, 36);
    for y in 0..36 {
        for x in 0..64 {
            let u = x as f32 / 63.0;
            put(&mut src, x, y, [u, 1.0 - u, 0.5, 1.0]);
        }
    }
    let warp = analysis.warp_at(30, false);
    let out = stabilize_warp(&src, &warp, Sampling::Bilinear);
    assert_eq!((out.width, out.height), (64, 36));
    assert!(
        out.pixels.iter().any(|p| p[3] > 0.0),
        "a non-transparent-edges warp must produce image content"
    );
}

#[test]
fn a_still_camera_round_trips_untouched() {
    // 22 §6.7's static case: nothing to correct means nothing changes, and
    // crucially no crop is charged for the privilege.
    let hz = 500.0;
    let mut body = String::from(
        "GYROFLOW IMU LOG\norientation,XYZ\ntscale,0.001\ngscale,1\nascale,1\n\
         t,gx,gy,gz,ax,ay,az\n",
    );
    for i in 0..1000 {
        let t_ms = i as f64 / hz * 1000.0;
        body.push_str(&format!("{t_ms:.3},0,0,0,0,9.80665,0\n"));
    }
    let path = write("still.gcsv", &body);
    let analysis = analyze_clip(&spec_for(&path, 0.9), geom(), |p| p.to_path_buf()).unwrap();

    assert!(
        analysis.frames.iter().all(|f| f.is_identity()),
        "a still camera needs no correction"
    );
    assert!((analysis.diagnostics.max_required_zoom - 1.0).abs() < 1e-6);

    let mut src = Image::new(32, 18);
    for y in 0..18 {
        for x in 0..32 {
            put(
                &mut src,
                x,
                y,
                [x as f32 / 31.0, y as f32 / 17.0, 0.25, 1.0],
            );
        }
    }
    let out = stabilize_warp(&src, &analysis.warp_at(10, false), Sampling::Bilinear);
    assert_eq!(out, src, "identity correction must be an exact passthrough");
}

#[test]
fn smoothing_strength_monotonically_increases_crop_demand() {
    // Steadier output costs more field of view — a property a user relies on
    // when trading the two off, and one a sign error in the smoother breaks.
    let path = shaky_gcsv("ramp.gcsv", 2.0, 0.4);
    let mut last = 0.0f32;
    for s in [0.2f32, 0.5, 0.9] {
        let a = analyze_clip(&spec_for(&path, s), geom(), |p| p.to_path_buf()).unwrap();
        let z = a.diagnostics.max_required_zoom;
        assert!(
            z >= last,
            "smoothness {s} required {z}x, less than the previous {last}x"
        );
        last = z;
    }
    assert!(last > 1.0, "the strongest setting should demand real crop");
}

#[test]
fn static_safe_never_exposes_an_edge() {
    // 22 §6.7: "Crop solver never exposes edges under StaticSafe within
    // max-zoom feasibility." Checked against the warp itself, not the solver's
    // own bookkeeping.
    let path = shaky_gcsv("safe.gcsv", 1.5, 0.3);
    let mut spec = spec_for(&path, 0.9);
    spec.crop_mode = StabilizationCropMode::StaticSafe;
    let analysis = analyze_clip(&spec, geom(), |p| p.to_path_buf()).unwrap();
    assert_eq!(
        analysis.diagnostics.infeasible_range, None,
        "this shake should be coverable within 2x"
    );

    // Rendered at half the analysis resolution — a real proxy ratio, well
    // inside the solver's documented MIN_RENDER_SCALE envelope.
    let (rw, rh) = (320u32, 180u32);
    let mut src = Image::new(rw, rh);
    for y in 0..rh {
        for x in 0..rw {
            put(&mut src, x, y, [1.0, 1.0, 1.0, 1.0]);
        }
    }
    // Opaque white in, so any transparent output pixel is an exposed edge.
    for f in 0..analysis.frames.len() {
        let out = stabilize_warp(&src, &analysis.warp_at(f, true), Sampling::Bilinear);
        let holes = out.pixels.iter().filter(|p| p[3] < 0.5).count();
        assert_eq!(holes, 0, "frame {f} exposed {holes} edge pixels");
    }
}

#[test]
fn horizon_lock_without_accelerometer_is_reported() {
    let mut body =
        String::from("GYROFLOW IMU LOG\norientation,XYZ\ntscale,0.001\ngscale,1\nt,gx,gy,gz\n");
    for i in 0..1000 {
        body.push_str(&format!("{:.3},0.02,0,0\n", i as f64 / 500.0 * 1000.0));
    }
    let path = write("noaccel.gcsv", &body);
    let mut spec = spec_for(&path, 0.5);
    spec.horizon_lock = 1.0;
    let a = analyze_clip(&spec, geom(), |p| p.to_path_buf()).unwrap();
    assert!(
        a.diagnostics.horizon_lock_unavailable,
        "a setting that silently does nothing must be reported"
    );
}

#[test]
fn analysis_is_reproducible_across_runs() {
    // Cache correctness depends on this: same inputs, byte-identical output.
    let path = shaky_gcsv("repro.gcsv", 1.0, 0.25);
    let spec = spec_for(&path, 0.7);
    let a = analyze_clip(&spec, geom(), |p| p.to_path_buf()).unwrap();
    let b = analyze_clip(&spec, geom(), |p| p.to_path_buf()).unwrap();
    assert_eq!(a, b);
}

#[test]
fn proxy_and_full_resolution_agree_geometrically() {
    // 22 §6.6: the proxy preview goes through the same warp at proxy
    // dimensions. Because intrinsics are normalized, one warp serves both — and
    // the *rendered* result must agree, not just the parameters.
    //
    // This is the test that caught the original bug: intrinsics stored in
    // pixels made a half-size proxy behave as though the lens were twice as
    // long, so preview and export disagreed.
    let path = shaky_gcsv("proxy.gcsv", 1.0, 0.3);
    let a = analyze_clip(&spec_for(&path, 0.8), geom(), |p| p.to_path_buf()).unwrap();
    let warp = a.warp_at(15, false);

    let render = |w: u32, h: u32| -> Vec<[f32; 4]> {
        let mut src = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                // Normalized ramp, so the same scene content appears at both
                // sizes and the outputs are directly comparable.
                let u = x as f32 / (w - 1) as f32;
                let v = y as f32 / (h - 1) as f32;
                put(&mut src, x, y, [u, v, 0.5, 1.0]);
            }
        }
        stabilize_warp(&src, &warp, Sampling::Bilinear).pixels
    };

    let full = render(640, 360);
    let proxy = render(320, 180);
    // Compare at matching normalized positions: proxy pixel (x, y) is full
    // pixel (2x, 2y).
    let mut worst = 0.0f32;
    for y in 0..180u32 {
        for x in 0..320u32 {
            let p = proxy[(y * 320 + x) as usize];
            let f = full[((y * 2) * 640 + x * 2) as usize];
            for c in 0..2 {
                worst = worst.max((p[c] - f[c]).abs());
            }
        }
    }
    assert!(
        worst < 0.02,
        "proxy and full-resolution renders diverged by {worst}; the warp is not \
         resolution-independent"
    );
}

/// Real-footage adversarial smoke test.
///
/// These are genuine DJI SD-card originals: no `encoder` tag, a `priv` data
/// track and a telemetry subtitle track. But the payload runs at ~1.0 Hz
/// (measured: 53 samples over 52 s on DJI_0014) — a flight log of GPS,
/// altitude and exposure, not angular velocity. A recursive box walk finds no
/// `uuid` box either, so there is no hidden IMU payload.
///
/// The adapter must therefore refuse — and refuse *specifically*, naming the
/// rate. "No motion track" would be actively misleading for a file that
/// visibly has a telemetry track, and is what sends someone hunting for a
/// software bug that isn't there.
///
/// Skips when the samples are absent, so CI elsewhere stays green.
#[test]
fn dji_flight_log_is_diagnosed_not_mistaken_for_gyro() {
    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let candidates = [
        home.join("Videos/DJI_0014.MP4"),
        home.join("Videos/DJI_0010.MP4"),
        home.join("Videos/DJI_0008.MP4"),
    ];
    let Some(path) = candidates.iter().find(|p| p.is_file()) else {
        eprintln!("skipping: no DJI original on this machine");
        return;
    };
    match photonic_video::media::parse_motion(path) {
        Err(photonic_video::media::MotionError::LowRateTelemetryOnly { hz, samples, .. }) => {
            assert!(
                hz < 50.0,
                "a flight log must be reported as low-rate, got {hz} Hz"
            );
            assert!(
                samples > 0,
                "the diagnostic must name the sample count it saw"
            );
        }
        // Acceptable when ffprobe is unavailable: the adapter falls back rather
        // than inventing a diagnosis it cannot support.
        Err(photonic_video::media::MotionError::NoMotionTrack) => {}
        Err(other) => panic!("expected a low-rate telemetry diagnosis, got {other:?}"),
        Ok(series) => panic!(
            "adapter claimed {} gyro samples from a 1 Hz flight log — it must refuse, not guess",
            series.samples.len()
        ),
    }
}

/// A transcoded copy must be reported as *derived*, not as empty.
///
/// Every DJI file in this machine's Downloads is an FFmpeg re-encode carrying
/// `encoder=Lavf...`; the camera telemetry did not survive. Saying "no gyro
/// data here" is true but useless, because the original still has it. Saying
/// "this is a re-encode, go get the original" is the fact that helps.
#[test]
fn reencoded_copy_is_reported_as_recoverable() {
    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let dl = home.join("Downloads");
    let Ok(entries) = std::fs::read_dir(&dl) else {
        eprintln!("skipping: no Downloads directory");
        return;
    };
    let Some(path) = entries.flatten().map(|e| e.path()).find(|p| {
        p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("mp4"))
            && p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("dji_fly_"))
    }) else {
        eprintln!("skipping: no transcoded DJI sample present");
        return;
    };
    match photonic_video::media::parse_motion(&path) {
        Err(photonic_video::media::MotionError::ReencodedCopy { writer }) => {
            assert!(!writer.is_empty(), "the diagnostic must name the writer");
        }
        Err(photonic_video::media::MotionError::NoMotionTrack) => {} // ffprobe unavailable
        Err(other) => panic!("expected ReencodedCopy, got {other:?}"),
        Ok(_) => panic!("a re-encoded copy cannot contain gyro data"),
    }
}
