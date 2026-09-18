//! Gyro-metadata stabilization analysis (D-12, 22 §6.4).
//!
//! The whole pipeline, in the order 22 §6.4 lays it out:
//!
//! 1. normalize axes and units — done by the adapter ([`crate::media::motion`]);
//! 2. estimate gyro bias ([`orientation::estimate_bias`]);
//! 3. resample onto video frame times ([`sync`] + [`orientation::OrientationCurve::sample`]);
//! 4. integrate orientation ([`orientation::integrate`]);
//! 5. smooth into the desired path ([`orientation::smooth`]);
//! 6. blend gravity/horizon correction ([`orientation::apply_horizon_lock`]);
//! 7. compute the per-frame correction rotation (here);
//! 8. solve the crop path ([`crop::solve`]).
//!
//! Output is a [`StabilizationAnalysis`]: one rotation and one zoom per frame,
//! plus diagnostics. That is everything the warp needs and nothing it does not
//! — the analysis is a pure function of (samples, lens, recipe), which is what
//! lets it be cached by content hash and recomputed for free after an undo.
//!
//! ## What the rotation means
//!
//! `FrameCorrection::rotation` maps a ray in the **virtual (stabilized) camera**
//! into the **real camera** that captured the frame, so the warp can look up
//! where each output pixel's light actually landed. It is `q_raw⁻¹ · q_smooth`.
//! When the smoothed path equals the raw one it is the identity, and the warp
//! is then exactly a copy — an invariant the tests pin down.
//!
//! ## Provenance
//!
//! Photonic-authored per 23 §9.3, from published rigid-body attitude and camera
//! geometry references named in [`orientation`] and [`lens`]. No `GPL-3.0`
//! source was consulted.

pub mod crop;
pub mod lens;
pub mod orientation;
pub mod sync;

use glam::{DMat3, DQuat};

use crate::graph::ir::StabilizeWarp;
use photonic_core::timeline::{
    Clip, MotionSample, StabilizationCropMode, StabilizationError, StabilizationSpec,
    TICKS_PER_SECOND,
};

pub use crop::{CropMode, CropSolution};
pub use lens::{DistortionModel, LensError, LensProfile};
pub use orientation::{BiasEstimate, OrientationCurve};
pub use sync::{ClockMap, SyncFit};

/// One frame's correction.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FrameCorrection {
    /// Row-major 3×3 rotation, virtual-camera ray → real-camera ray.
    pub rotation: [f32; 9],
    /// Zoom applied about the frame centre, `>= 1.0`.
    pub zoom: f32,
}

impl FrameCorrection {
    /// The do-nothing correction.
    pub const IDENTITY: FrameCorrection = FrameCorrection {
        rotation: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        zoom: 1.0,
    };

    /// True when this correction leaves the image untouched.
    pub fn is_identity(&self) -> bool {
        self.zoom == 1.0
            && self
                .rotation
                .iter()
                .zip(Self::IDENTITY.rotation.iter())
                .all(|(a, b)| (a - b).abs() < 1e-7)
    }
}

/// What the analysis observed, for the inspector and for diagnosis.
#[derive(Clone, Debug, PartialEq)]
pub struct StabilizationDiagnostics {
    pub bias: BiasEstimate,
    pub sample_rate_hz: Option<f64>,
    /// Clock rate error against video time, parts per million.
    pub clock_drift_ppm: f64,
    /// Worst anchor disagreement, nanoseconds.
    pub sync_residual_ns: f64,
    pub anchors_used: usize,
    /// Largest zoom any frame required before clamping.
    pub max_required_zoom: f32,
    /// Frames the crop solver could not cover within `max_zoom`.
    pub infeasible_range: Option<(usize, usize)>,
    /// Mean gravity confidence, or `None` without accelerometer data.
    pub mean_gravity_confidence: Option<f64>,
    /// True when horizon lock was requested but no usable accelerometer
    /// data existed — the setting silently doing nothing would be worse.
    pub horizon_lock_unavailable: bool,
}

/// The analysis result: one correction per frame plus diagnostics.
///
/// Carries the camera geometry it was computed against, so the compiler can
/// build a [`StabilizeWarp`] for any delivery resolution without re-reading the
/// lens profile on the per-frame compile path.
#[derive(Clone, Debug, PartialEq)]
pub struct StabilizationAnalysis {
    pub frames: Vec<FrameCorrection>,
    pub diagnostics: StabilizationDiagnostics,
    /// Frame rate the corrections are indexed at.
    pub fps: f64,
    /// Inclusive source-time range covered by `frames`, in seconds.
    pub source_start_s: f64,
    pub source_end_s: f64,
    /// Resolution the intrinsics below are expressed at.
    pub width: f32,
    pub height: f32,
    /// Source intrinsics normalized by frame size: `[fx/w, fy/h, cx/w, cy/h]`.
    pub intrinsics: [f32; 4],
    /// Kannala-Brandt coefficients; all zero for a pinhole.
    pub k: [f32; 4],
    pub fisheye: bool,
}

impl StabilizationAnalysis {
    /// The correction for `frame`, or the identity past the end.
    ///
    /// Out of range yields identity rather than panicking or clamping to the
    /// last frame: a clip retimed longer than its analysis should degrade to
    /// unstabilized, not freeze on the final correction.
    pub fn at(&self, frame: usize) -> FrameCorrection {
        self.frames
            .get(frame)
            .copied()
            .unwrap_or(FrameCorrection::IDENTITY)
    }

    /// The frame index covering source time `seconds`.
    pub fn frame_index(&self, seconds: f64) -> usize {
        if !seconds.is_finite() || seconds <= self.source_start_s {
            return 0;
        }
        ((seconds - self.source_start_s) * self.fps)
            .round()
            .max(0.0) as usize
    }

    /// Build the resolved warp for `frame`.
    ///
    /// Carries no delivery size, because [`StabilizeWarp::intrinsics`] are
    /// normalized and the evaluator scales them to whatever it is actually
    /// rendering. That is what lets a proxy preview and a full-resolution
    /// export describe identical geometry, as 22 §6.6 requires.
    pub fn warp_at(&self, frame: usize, transparent_edges: bool) -> StabilizeWarp {
        let c = self.at(frame);
        StabilizeWarp {
            rotation: c.rotation,
            zoom: c.zoom,
            intrinsics: self.intrinsics,
            k: self.k,
            fisheye: self.fisheye,
            transparent_edges,
        }
    }
}

/// Warm per-clip analyses, read lock-free by the compiler.
///
/// Mirrors `LutCache`'s shape and contract: warmed off the hot path, then read
/// during compile without locking. Analyses are keyed by clip because the
/// recipe (smoothness, crop mode, lens) is per-clip — two clips cut from the
/// same media with different settings need different corrections.
#[derive(Default)]
pub struct StabilizationCache {
    entries: std::collections::HashMap<
        photonic_core::timeline::ClipId,
        (String, std::sync::Arc<StabilizationAnalysis>),
    >,
    /// Diagnostics from the most recent analyses, surfaced alongside the frame.
    pub failures: Vec<String>,
}

impl StabilizationCache {
    /// Store (or replace) the analysis for `clip`.
    pub fn insert(
        &mut self,
        clip: photonic_core::timeline::ClipId,
        key: String,
        analysis: StabilizationAnalysis,
    ) {
        self.entries
            .insert(clip, (key, std::sync::Arc::new(analysis)));
    }

    /// Drop a clip's analysis, e.g. when its recipe changed.
    pub fn invalidate(&mut self, clip: photonic_core::timeline::ClipId) {
        self.entries.remove(&clip);
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl crate::graph::compile::StabilizationProvider for StabilizationCache {
    fn analysis(
        &self,
        clip: photonic_core::timeline::ClipId,
        key: &str,
        source_start_s: f64,
        source_end_s: f64,
    ) -> Option<std::sync::Arc<StabilizationAnalysis>> {
        let (cached_key, analysis) = self.entries.get(&clip)?;
        if cached_key != key
            || (analysis.source_start_s - source_start_s).abs() > 1e-9
            || (analysis.source_end_s - source_end_s).abs() > 1e-9
        {
            return None;
        }
        Some(std::sync::Arc::clone(analysis))
    }
}

/// Why analysis could not run.
#[derive(Debug, thiserror::Error)]
pub enum AnalyzeError {
    #[error("stabilization recipe is invalid: {0:?}")]
    Recipe(StabilizationError),
    #[error("no motion samples")]
    NoSamples,
    #[error("frame count must be positive")]
    NoFrames,
    #[error("frame rate must be positive and finite")]
    BadFrameRate,
    #[error("frame dimensions must be positive")]
    BadDimensions,
    #[error("source-time range must be finite and non-decreasing")]
    BadSourceRange,
    #[error("motion metadata: {0}")]
    Motion(String),
    #[error("lens profile: {0}")]
    Lens(String),
}

/// Everything the analysis needs about the clip itself.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ClipGeometry {
    pub width: f64,
    pub height: f64,
    pub fps: f64,
    pub frame_count: usize,
    /// Inclusive source-time range covered by the requested clip, in seconds.
    pub source_start_s: f64,
    pub source_end_s: f64,
}

/// The source-time interval sampled by a clip, including trims, reverse playback,
/// and speed ramps. The compiler and analysis path must use the same interval or
/// a cached correction can be silently paired with the wrong source frame.
pub fn source_time_range(clip: &Clip) -> (f64, f64) {
    let (lo, hi) = clip.speed.source_delta_range(clip.duration);
    let source_in = clip.source_in.as_seconds_f64();
    let ticks = TICKS_PER_SECOND as f64;
    (source_in + lo / ticks, source_in + hi / ticks)
}

/// Build analysis geometry for one clip at the sequence's delivery rate.
pub fn geometry_for_clip(width: f64, height: f64, fps: f64, clip: &Clip) -> ClipGeometry {
    let (source_start_s, source_end_s) = source_time_range(clip);
    let source_span_s = (source_end_s - source_start_s).max(0.0);
    let frame_count = ((source_span_s * fps).ceil() as usize).max(1);
    ClipGeometry {
        width,
        height,
        fps,
        frame_count,
        source_start_s,
        source_end_s,
    }
}

/// End-to-end: read the motion source and lens profile named by `spec`, then
/// analyze. The one entry point both the GUI's Analyze action and the MCP
/// `analyze_stabilization` job call, so they cannot drift apart.
///
/// `resolve` maps a stored sidecar path to something openable — the caller owns
/// project-relative resolution and relink, which this module has no business
/// knowing about.
pub fn analyze_clip(
    spec: &StabilizationSpec,
    geom: ClipGeometry,
    resolve: impl Fn(&std::path::Path) -> std::path::PathBuf,
) -> Result<StabilizationAnalysis, AnalyzeError> {
    use photonic_core::timeline::{LensProfileRef, MotionSourceRef};

    spec.validate().map_err(AnalyzeError::Recipe)?;

    let series = match &spec.binding.source {
        MotionSourceRef::Sidecar { path, .. } => crate::media::motion::parse_motion(&resolve(path))
            .map_err(|e| AnalyzeError::Motion(e.to_string()))?,
        MotionSourceRef::Embedded { .. } => {
            // 23 §9.1's dependency audit has not cleared, so no container
            // adapter exists to call. Fail loudly rather than pretending.
            return Err(AnalyzeError::Motion(
                "container-embedded telemetry is not supported in this build".into(),
            ));
        }
    };

    let lens = match &spec.binding.lens {
        // No calibration: rays still need *a* consistent camera to be defined
        // in, so use an ideal rectilinear one. This corrects rotation only,
        // which is exactly what RotationOnly promises.
        LensProfileRef::RotationOnly => LensProfile::ideal_pinhole(geom.width, geom.height, 90.0),
        LensProfileRef::UserFile { path, .. } => {
            LensProfile::from_path(&resolve(path)).map_err(|e| AnalyzeError::Lens(e.to_string()))?
        }
        LensProfileRef::Bundled { id } => {
            // No snapshot ships until 23 §9.2 per-entry intake passes, so a
            // document referencing one resolves to "unavailable" rather than
            // silently degrading to uncalibrated.
            return Err(AnalyzeError::Lens(format!(
                "bundled lens profile {id:?} is not available in this build"
            )));
        }
    };

    analyze(&series.samples, spec, &lens, geom)
}

/// Run the full analysis (22 §6.4 steps 2-8).
///
/// `samples` must already be axis- and unit-normalized — that is the adapter's
/// job, and doing it here instead would mean every dialect's quirks leaking
/// into the math.
pub fn analyze(
    samples: &[MotionSample],
    spec: &StabilizationSpec,
    lens: &LensProfile,
    geom: ClipGeometry,
) -> Result<StabilizationAnalysis, AnalyzeError> {
    spec.validate().map_err(AnalyzeError::Recipe)?;
    if samples.is_empty() {
        return Err(AnalyzeError::NoSamples);
    }
    if geom.frame_count == 0 {
        return Err(AnalyzeError::NoFrames);
    }
    if !geom.fps.is_finite() || geom.fps <= 0.0 {
        return Err(AnalyzeError::BadFrameRate);
    }
    if !(geom.width > 0.0 && geom.height > 0.0) {
        return Err(AnalyzeError::BadDimensions);
    }
    if !geom.source_start_s.is_finite()
        || !geom.source_end_s.is_finite()
        || geom.source_end_s < geom.source_start_s
    {
        return Err(AnalyzeError::BadSourceRange);
    }

    // 2 — bias.
    let bias = orientation::estimate_bias(samples);

    // 4 — integrate at native rate.
    let curve = orientation::integrate(samples, &bias);

    // 3 — resample onto frame times through the fitted clock map.
    let fit = sync::fit(&spec.binding.sync.anchors);
    let dt_s = 1.0 / geom.fps;
    let raw: Vec<DQuat> = (0..geom.frame_count)
        .map(|f| {
            let video_ns = (geom.source_start_s + f as f64 * dt_s) * 1e9;
            curve.sample(fit.map.sensor_ns(video_ns) as i64)
        })
        .collect();

    // 5 — smooth into the desired path.
    let mut desired = orientation::smooth(&raw, dt_s, spec.smoothness as f64);

    // 6 — horizon lock, where gravity is measurable.
    let mut confidence_sum = 0.0;
    let mut confidence_n = 0usize;
    let mut horizon_lock_unavailable = false;
    if spec.horizon_lock > 0.0 {
        let mut any_accel = false;
        for (f, q) in desired.iter_mut().enumerate() {
            let video_ns = (geom.source_start_s + f as f64 * dt_s) * 1e9;
            let t = fit.map.sensor_ns(video_ns) as i64;
            let Some(accel) = nearest_accel(samples, t) else {
                continue;
            };
            any_accel = true;
            let conf = orientation::gravity_confidence(accel);
            confidence_sum += conf;
            confidence_n += 1;
            *q = orientation::apply_horizon_lock(*q, accel, spec.horizon_lock as f64, conf);
        }
        horizon_lock_unavailable = !any_accel;
    }

    // 7 — correction rotation: virtual-camera ray into the real camera.
    let rotations: Vec<DMat3> = raw
        .iter()
        .zip(desired.iter())
        .map(|(r, d)| DMat3::from_quat(r.inverse() * *d))
        .collect();

    // 8 — crop path.
    let max_zoom = spec.max_zoom as f64;
    let mode = match spec.crop_mode {
        StabilizationCropMode::StaticSafe => CropMode::StaticSafe,
        StabilizationCropMode::Dynamic => CropMode::Dynamic,
        StabilizationCropMode::TransparentEdges => CropMode::TransparentEdges,
        // `validate()` above rejects unknown modes, so this is unreachable in
        // practice; the safest fallback is still the one that cannot expose an
        // edge. The enum is `#[non_exhaustive]`, so a wildcard is required
        // here regardless.
        _ => CropMode::StaticSafe,
    };
    let per_frame: Vec<Option<f64>> = rotations
        .iter()
        .map(|r| crop::required_zoom(*r, lens, geom.width, geom.height, max_zoom))
        .collect();
    let solution = crop::solve(&per_frame, mode, max_zoom, geom.fps);

    let frames = rotations
        .iter()
        .zip(solution.zoom.iter())
        .map(|(r, z)| FrameCorrection {
            rotation: mat3_row_major(*r),
            zoom: *z,
        })
        .collect();

    let (fx, fy, cx, cy) = lens.intrinsics_for(geom.width, geom.height);
    Ok(StabilizationAnalysis {
        frames,
        source_start_s: geom.source_start_s,
        source_end_s: geom.source_end_s,
        fps: geom.fps,
        width: geom.width as f32,
        height: geom.height as f32,
        intrinsics: [
            (fx / geom.width) as f32,
            (fy / geom.height) as f32,
            (cx / geom.width) as f32,
            (cy / geom.height) as f32,
        ],
        k: [
            lens.k[0] as f32,
            lens.k[1] as f32,
            lens.k[2] as f32,
            lens.k[3] as f32,
        ],
        fisheye: lens.model == DistortionModel::Fisheye,
        diagnostics: StabilizationDiagnostics {
            bias,
            sample_rate_hz: mean_rate_hz(samples),
            clock_drift_ppm: fit.map.drift_ppm(),
            sync_residual_ns: fit.max_residual_ns,
            anchors_used: fit.anchors_used,
            max_required_zoom: solution.max_required,
            infeasible_range: solution.infeasible,
            mean_gravity_confidence: (confidence_n > 0)
                .then(|| confidence_sum / confidence_n as f64),
            horizon_lock_unavailable,
        },
    })
}

/// `glam` stores matrices column-major; the IR and WGSL want row-major.
fn mat3_row_major(m: DMat3) -> [f32; 9] {
    [
        m.x_axis.x as f32,
        m.y_axis.x as f32,
        m.z_axis.x as f32,
        m.x_axis.y as f32,
        m.y_axis.y as f32,
        m.z_axis.y as f32,
        m.x_axis.z as f32,
        m.y_axis.z as f32,
        m.z_axis.z as f32,
    ]
}

fn mean_rate_hz(samples: &[MotionSample]) -> Option<f64> {
    let (first, last) = (samples.first()?, samples.last()?);
    let span = (last.sensor_time_ns - first.sensor_time_ns) as f64;
    (span > 0.0).then(|| (samples.len() - 1) as f64 * 1e9 / span)
}

/// Accelerometer reading nearest `t_ns`, if any sample carries one.
fn nearest_accel(samples: &[MotionSample], t_ns: i64) -> Option<[f64; 3]> {
    let idx = match samples.binary_search_by_key(&t_ns, |s| s.sensor_time_ns) {
        Ok(i) => i,
        Err(i) => i.min(samples.len().saturating_sub(1)),
    };
    // Walk outward: the nearest sample may not carry acceleration even when
    // others nearby do.
    let mut lo = idx as isize;
    let mut hi = idx;
    while lo >= 0 || hi < samples.len() {
        if lo >= 0 {
            if let Some(a) = samples[lo as usize].accel_mps2 {
                return Some(a);
            }
            lo -= 1;
        }
        if hi < samples.len() {
            if let Some(a) = samples[hi].accel_mps2 {
                return Some(a);
            }
            hi += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::{
        ClipId, ClipSource, LensProfileRef, MotionBinding, MotionFormat, MotionSourceRef,
        MotionSync, Ratio, SpeedMap, Tick,
    };
    use std::path::PathBuf;

    fn geom() -> ClipGeometry {
        ClipGeometry {
            width: 1920.0,
            height: 1080.0,
            fps: 30.0,
            frame_count: 60,
            source_start_s: 0.0,
            source_end_s: 2.0,
        }
    }

    fn spec(smoothness: f32) -> StabilizationSpec {
        let mut s = StabilizationSpec::new(MotionBinding {
            source: MotionSourceRef::Sidecar {
                path: PathBuf::from("/x.gcsv"),
                rel_path: None,
                format: MotionFormat::Gcsv,
            },
            sync: MotionSync::default(),
            lens: LensProfileRef::RotationOnly,
        });
        s.smoothness = smoothness;
        s.max_zoom = 3.0;
        s
    }

    fn samples(gyro: [f64; 3], accel: Option<[f64; 3]>, hz: f64, secs: f64) -> Vec<MotionSample> {
        let n = (hz * secs) as usize;
        (0..=n)
            .map(|i| MotionSample {
                sensor_time_ns: (i as f64 / hz * 1e9) as i64,
                gyro_rad_s: gyro,
                accel_mps2: accel,
                orientation: None,
            })
            .collect()
    }

    #[test]
    fn geometry_tracks_trim_and_playback_speed_source_range() {
        let mut clip = Clip::new(
            ClipSource::SolidColor {
                color: photonic_core::Color::WHITE,
            },
            Tick::ZERO,
            Tick::from_seconds(2),
        );
        clip.source_in = Tick::from_seconds(10);
        clip.speed = SpeedMap::Constant(Ratio::new(2, 1));

        let geometry = geometry_for_clip(1920.0, 1080.0, 30.0, &clip);
        assert!((geometry.source_start_s - 10.0).abs() < 1e-9);
        assert!((geometry.source_end_s - 14.0).abs() < 1e-9);
        assert_eq!(geometry.frame_count, 120);
    }

    #[test]
    fn frame_index_is_relative_to_the_analyzed_source_range() {
        let mut geometry = geom();
        geometry.source_start_s = 10.0;
        geometry.source_end_s = 12.0;
        let samples = samples([0.0; 3], None, 500.0, 2.0);
        let analysis = analyze(
            &samples,
            &spec(0.5),
            &LensProfile::ideal_pinhole(1920.0, 1080.0, 90.0),
            geometry,
        )
        .unwrap();

        assert_eq!(analysis.source_start_s, 10.0);
        assert_eq!(analysis.source_end_s, 12.0);
        assert_eq!(analysis.frame_index(10.0), 0);
        assert_eq!(analysis.frame_index(11.0), 30);
    }

    #[test]
    fn a_perfectly_still_camera_needs_no_correction() {
        let s = samples([0.0; 3], None, 500.0, 2.0);
        let a = analyze(
            &s,
            &spec(0.8),
            &LensProfile::ideal_pinhole(1920.0, 1080.0, 90.0),
            geom(),
        )
        .unwrap();
        assert_eq!(a.frames.len(), 60);
        for (i, f) in a.frames.iter().enumerate() {
            assert!(f.is_identity(), "frame {i} was not identity: {f:?}");
        }
        assert!((a.diagnostics.max_required_zoom - 1.0).abs() < 1e-6);
        assert_eq!(a.diagnostics.infeasible_range, None);
    }

    #[test]
    fn zero_smoothness_leaves_the_footage_alone() {
        // With no smoothing the desired path *is* the raw path, so the
        // correction is identity even though the camera is moving hard.
        let s = samples([0.0, 0.5, 0.0], None, 500.0, 2.0);
        let a = analyze(
            &s,
            &spec(0.0),
            &LensProfile::ideal_pinhole(1920.0, 1080.0, 90.0),
            geom(),
        )
        .unwrap();
        for f in &a.frames {
            assert!(f.is_identity());
        }
    }

    #[test]
    fn shake_produces_a_real_correction() {
        // A camera that jitters should be corrected, and that correction should
        // cost some zoom.
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut rng = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        };
        let s: Vec<MotionSample> = (0..=1000)
            .map(|i| MotionSample {
                sensor_time_ns: (i as f64 / 500.0 * 1e9) as i64,
                gyro_rad_s: [rng() * 0.6, rng() * 0.6, rng() * 0.6],
                accel_mps2: None,
                orientation: None,
            })
            .collect();
        let a = analyze(
            &s,
            &spec(0.9),
            &LensProfile::ideal_pinhole(1920.0, 1080.0, 90.0),
            geom(),
        )
        .unwrap();
        assert!(
            a.frames.iter().any(|f| !f.is_identity()),
            "shaky footage must produce a non-identity correction"
        );
        assert!(a.diagnostics.max_required_zoom > 1.0);
    }

    #[test]
    fn horizon_lock_without_accelerometer_is_reported_not_silent() {
        let mut sp = spec(0.5);
        sp.horizon_lock = 1.0;
        let s = samples([0.0, 0.1, 0.0], None, 500.0, 2.0);
        let a = analyze(
            &s,
            &sp,
            &LensProfile::ideal_pinhole(1920.0, 1080.0, 90.0),
            geom(),
        )
        .unwrap();
        assert!(
            a.diagnostics.horizon_lock_unavailable,
            "a setting that silently does nothing is worse than one that says so"
        );
        assert_eq!(a.diagnostics.mean_gravity_confidence, None);
    }

    #[test]
    fn horizon_lock_reports_confidence_when_accel_is_present() {
        let mut sp = spec(0.5);
        sp.horizon_lock = 1.0;
        let s = samples(
            [0.0, 0.05, 0.0],
            Some([0.0, orientation::G0, 0.0]),
            500.0,
            2.0,
        );
        let a = analyze(
            &s,
            &sp,
            &LensProfile::ideal_pinhole(1920.0, 1080.0, 90.0),
            geom(),
        )
        .unwrap();
        assert!(!a.diagnostics.horizon_lock_unavailable);
        let c = a.diagnostics.mean_gravity_confidence.unwrap();
        assert!(
            (c - 1.0).abs() < 1e-9,
            "resting accel should be fully trusted, got {c}"
        );
    }

    #[test]
    fn bias_is_reported_in_diagnostics() {
        let s = samples([0.008, -0.004, 0.002], None, 500.0, 3.0);
        let a = analyze(
            &s,
            &spec(0.5),
            &LensProfile::ideal_pinhole(1920.0, 1080.0, 90.0),
            geom(),
        )
        .unwrap();
        assert!(a.diagnostics.bias.estimated);
        assert!((a.diagnostics.bias.bias_rad_s[0] - 0.008).abs() < 1e-6);
    }

    #[test]
    fn clock_drift_is_reported() {
        use photonic_core::timeline::{MotionSyncAnchor, Tick, TICKS_PER_SECOND};
        let mut sp = spec(0.5);
        sp.binding.sync.anchors = vec![
            MotionSyncAnchor {
                video_tick: Tick(0),
                sensor_time_ns: 0,
            },
            MotionSyncAnchor {
                video_tick: Tick(2 * TICKS_PER_SECOND),
                sensor_time_ns: 2_002_000_000,
            },
        ];
        let s = samples([0.0; 3], None, 500.0, 3.0);
        let a = analyze(
            &s,
            &sp,
            &LensProfile::ideal_pinhole(1920.0, 1080.0, 90.0),
            geom(),
        )
        .unwrap();
        assert!(
            (a.diagnostics.clock_drift_ppm - 1000.0).abs() < 1.0,
            "got {} ppm",
            a.diagnostics.clock_drift_ppm
        );
        assert_eq!(a.diagnostics.anchors_used, 2);
    }

    #[test]
    fn analysis_is_deterministic() {
        // Cacheability depends on this: the same inputs must give bit-identical
        // output, or the analysis key lies.
        let s = samples(
            [0.05, -0.02, 0.01],
            Some([0.0, orientation::G0, 0.0]),
            500.0,
            2.0,
        );
        let l = LensProfile::ideal_pinhole(1920.0, 1080.0, 90.0);
        let a = analyze(&s, &spec(0.7), &l, geom()).unwrap();
        let b = analyze(&s, &spec(0.7), &l, geom()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn cached_analysis_requires_matching_generation_and_source_range() {
        let s = samples([0.0; 3], None, 500.0, 2.0);
        let analysis = analyze(
            &s,
            &spec(0.5),
            &LensProfile::ideal_pinhole(1920.0, 1080.0, 90.0),
            geom(),
        )
        .unwrap();
        let clip = ClipId::new();
        let key = "generation-1".to_owned();
        let mut cache = StabilizationCache::default();
        cache.insert(clip, key.clone(), analysis);
        let provider: &dyn crate::graph::compile::StabilizationProvider = &cache;

        assert!(provider.analysis(clip, &key, 0.0, 2.0).is_some());
        assert!(provider.analysis(clip, "generation-0", 0.0, 2.0).is_none());
        assert!(provider.analysis(clip, &key, 0.0, 3.0).is_none());
    }

    #[test]
    fn out_of_range_frames_degrade_to_identity() {
        let s = samples([0.0; 3], None, 500.0, 2.0);
        let a = analyze(
            &s,
            &spec(0.5),
            &LensProfile::ideal_pinhole(1920.0, 1080.0, 90.0),
            geom(),
        )
        .unwrap();
        assert!(
            a.at(9_999).is_identity(),
            "past the end must not freeze or panic"
        );
    }

    #[test]
    fn invalid_inputs_are_rejected() {
        let s = samples([0.0; 3], None, 500.0, 1.0);
        let l = LensProfile::ideal_pinhole(1920.0, 1080.0, 90.0);
        assert!(matches!(
            analyze(&[], &spec(0.5), &l, geom()),
            Err(AnalyzeError::NoSamples)
        ));
        assert!(matches!(
            analyze(
                &s,
                &spec(0.5),
                &l,
                ClipGeometry {
                    frame_count: 0,
                    ..geom()
                }
            ),
            Err(AnalyzeError::NoFrames)
        ));
        assert!(matches!(
            analyze(&s, &spec(0.5), &l, ClipGeometry { fps: 0.0, ..geom() }),
            Err(AnalyzeError::BadFrameRate)
        ));
        assert!(matches!(
            analyze(
                &s,
                &spec(0.5),
                &l,
                ClipGeometry {
                    width: 0.0,
                    ..geom()
                }
            ),
            Err(AnalyzeError::BadDimensions)
        ));
        let mut bad = spec(0.5);
        bad.max_zoom = 0.5;
        assert!(matches!(
            analyze(&s, &bad, &l, geom()),
            Err(AnalyzeError::Recipe(_))
        ));
    }

    #[test]
    fn row_major_conversion_is_correct() {
        // A 90° yaw maps +Z to +X in row-major terms; getting this transposed
        // would mirror every stabilized frame.
        let m = DMat3::from_quat(DQuat::from_rotation_y(std::f64::consts::FRAC_PI_2));
        let r = mat3_row_major(m);
        let v = glam::DVec3::Z;
        let expect = m * v;
        let got = glam::DVec3::new(
            r[0] as f64 * v.x + r[1] as f64 * v.y + r[2] as f64 * v.z,
            r[3] as f64 * v.x + r[4] as f64 * v.y + r[5] as f64 * v.z,
            r[6] as f64 * v.x + r[7] as f64 * v.y + r[8] as f64 * v.z,
        );
        assert!((got - expect).length() < 1e-12, "{got:?} vs {expect:?}");
    }
}
