//! Lens calibration and projection for D-12 (22 §6.4, 23 §9.2).
//!
//! Two camera models, one interface:
//!
//! - **Pinhole** — rectilinear lenses. Straight lines stay straight.
//! - **Fisheye** — the Kannala-Brandt generic camera model, the standard
//!   wide-angle/fisheye formulation and the one action-cam and drone lens
//!   calibrations are published in.
//!
//! ## Why the output camera reuses the source lens
//!
//! The warp is `output pixel → ray → rotate → source pixel`, and both the
//! unprojection and the reprojection go through the **same** lens model. That
//! is a deliberate choice with a useful consequence: when the correction
//! rotation is identity the warp is *exactly* the identity map, to floating
//! point. Any drift from that is a bug the tests can catch outright, which is
//! not true if the output were an idealized pinhole camera with an
//! independently chosen field of view.
//!
//! It also means stabilization preserves the lens's character. De-fishing is a
//! separate creative decision, not something stabilization should impose.
//!
//! ## Provenance
//!
//! The projection equations are from Kannala & Brandt, "A Generic Camera Model
//! and Calibration Method for Conventional, Wide-Angle, and Fish-Eye Lenses,"
//! *IEEE TPAMI* 28(8), 2006 — a published, patent-free formulation. No
//! `GPL-3.0` source was consulted (23 §9.3, §14 D-12 disposition).

use glam::DVec3;

/// Which projection the calibration describes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum DistortionModel {
    /// Rectilinear: `u = x/z`, then intrinsics.
    Pinhole,
    /// Kannala-Brandt equidistant-family fisheye with four radial terms.
    #[default]
    Fisheye,
}

/// Why a lens profile could not be used.
#[derive(Debug, thiserror::Error)]
pub enum LensError {
    #[error("could not read lens profile: {0}")]
    Io(#[source] std::io::Error),
    #[error("lens profile is not valid JSON: {0}")]
    Json(#[source] serde_json::Error),
    #[error("lens profile camera_matrix is not 3x3")]
    BadCameraMatrix,
    #[error("lens profile has a non-positive focal length ({fx}, {fy})")]
    NonPositiveFocal { fx: f64, fy: f64 },
    #[error("lens profile calibration dimensions are zero")]
    ZeroCalibDimension,
    #[error("lens profile distortion_coeffs must have 4 entries, found {found}")]
    BadCoeffCount { found: usize },
    #[error("lens profile contains a non-finite value")]
    NonFinite,
    #[error("unsupported distortion model {0:?} — refusing to guess")]
    UnsupportedModel(String),
}

/// A calibrated lens.
///
/// Intrinsics are stored at `calib_width`/`calib_height` and rescaled on use,
/// because a profile calibrated at 1920×1080 must still apply to the same
/// camera mode delivered at 3840×2160.
#[derive(Clone, Debug, PartialEq)]
pub struct LensProfile {
    pub model: DistortionModel,
    /// Focal length in pixels at calibration resolution.
    pub fx: f64,
    pub fy: f64,
    /// Principal point in pixels at calibration resolution.
    pub cx: f64,
    pub cy: f64,
    /// Radial coefficients `k1..k4`. Unused (and zero) for [`DistortionModel::Pinhole`].
    pub k: [f64; 4],
    pub calib_width: f64,
    pub calib_height: f64,
    /// Sensor readout duration, seconds.
    ///
    /// Parsed and carried but **not consumed**: rolling-shutter correction is
    /// deferred (22 §6.8). Plumbing it costs nothing and means enabling that
    /// later needs no format work.
    pub frame_readout_time_s: Option<f64>,
    pub global_shutter: bool,
    /// Free-text identity for diagnostics and the inspector.
    pub name: String,
}

impl LensProfile {
    /// An ideal rectilinear lens for `width`×`height` with a horizontal field
    /// of view of `hfov_deg`.
    ///
    /// Used for synthetic fixtures and as the geometry behind rotation-only
    /// mode, where no calibration exists but rays still need *a* consistent
    /// camera to be defined in.
    pub fn ideal_pinhole(width: f64, height: f64, hfov_deg: f64) -> Self {
        let f = (width / 2.0) / (hfov_deg.to_radians() / 2.0).tan();
        LensProfile {
            model: DistortionModel::Pinhole,
            fx: f,
            fy: f,
            cx: width / 2.0,
            cy: height / 2.0,
            k: [0.0; 4],
            calib_width: width,
            calib_height: height,
            frame_readout_time_s: None,
            global_shutter: false,
            name: format!("ideal pinhole {hfov_deg}° hfov"),
        }
    }

    /// Intrinsics rescaled from calibration resolution to `width`×`height`.
    ///
    /// Returns `(fx, fy, cx, cy)`. Non-uniform scaling is honoured rather than
    /// averaged: a profile calibrated at 16:9 applied to a 4:3 delivery of the
    /// same sensor really does have different x and y scale factors.
    pub fn intrinsics_for(&self, width: f64, height: f64) -> (f64, f64, f64, f64) {
        let sx = width / self.calib_width;
        let sy = height / self.calib_height;
        (self.fx * sx, self.fy * sy, self.cx * sx, self.cy * sy)
    }

    /// Project a camera-frame ray onto the image plane at `width`×`height`.
    ///
    /// Returns `None` for rays behind the camera under [`DistortionModel::Pinhole`],
    /// which have no forward projection.
    pub fn project(&self, ray: DVec3, width: f64, height: f64) -> Option<(f64, f64)> {
        let (fx, fy, cx, cy) = self.intrinsics_for(width, height);
        match self.model {
            DistortionModel::Pinhole => {
                if ray.z <= 1e-12 {
                    return None;
                }
                Some((fx * (ray.x / ray.z) + cx, fy * (ray.y / ray.z) + cy))
            }
            DistortionModel::Fisheye => {
                let r = (ray.x * ray.x + ray.y * ray.y).sqrt();
                // On the optical axis the direction is undefined but the
                // projection is not: it is the principal point.
                if r < 1e-12 {
                    return Some((cx, cy));
                }
                let theta = r.atan2(ray.z);
                let theta_d = distort_theta(theta, &self.k);
                let scale = theta_d / r;
                Some((fx * ray.x * scale + cx, fy * ray.y * scale + cy))
            }
        }
    }

    /// Unproject an image point at `width`×`height` into a unit camera-frame ray.
    pub fn unproject(&self, px: f64, py: f64, width: f64, height: f64) -> DVec3 {
        let (fx, fy, cx, cy) = self.intrinsics_for(width, height);
        let u = (px - cx) / fx;
        let v = (py - cy) / fy;
        match self.model {
            DistortionModel::Pinhole => DVec3::new(u, v, 1.0).normalize(),
            DistortionModel::Fisheye => {
                let theta_d = (u * u + v * v).sqrt();
                if theta_d < 1e-12 {
                    return DVec3::Z;
                }
                let theta = undistort_theta(theta_d, &self.k);
                let (s, c) = (theta.sin(), theta.cos());
                DVec3::new(s * u / theta_d, s * v / theta_d, c)
            }
        }
    }

    fn validate(&self) -> Result<(), LensError> {
        let finite = [
            self.fx,
            self.fy,
            self.cx,
            self.cy,
            self.calib_width,
            self.calib_height,
        ]
        .iter()
        .chain(self.k.iter())
        .all(|v| v.is_finite());
        if !finite {
            return Err(LensError::NonFinite);
        }
        if self.fx <= 0.0 || self.fy <= 0.0 {
            return Err(LensError::NonPositiveFocal {
                fx: self.fx,
                fy: self.fy,
            });
        }
        if self.calib_width <= 0.0 || self.calib_height <= 0.0 {
            return Err(LensError::ZeroCalibDimension);
        }
        Ok(())
    }

    /// Parse a lens-profile JSON document.
    ///
    /// Accepts the widely-used community shape (`fisheye_params.camera_matrix`
    /// + `distortion_coeffs`, `calib_dimension`) so a user can point Photonic
    ///   at a profile they already have. Reading a *format* carries no licence
    ///   obligation; bundling profile *data* does, which is why no snapshot ships
    ///   until 23 §9.2 per-entry intake passes.
    pub fn from_json(text: &str) -> Result<Self, LensError> {
        let v: serde_json::Value = serde_json::from_str(text).map_err(LensError::Json)?;

        // The distortion block lives under `fisheye_params` in the common
        // shape; fall back to the document root so a minimal hand-authored
        // profile also works.
        let params = v.get("fisheye_params").unwrap_or(&v);

        let model = match v.get("distortion_model").and_then(|m| m.as_str()) {
            None | Some("fisheye") | Some("kannala_brandt") => DistortionModel::Fisheye,
            Some("pinhole") | Some("rectilinear") => DistortionModel::Pinhole,
            Some(other) => return Err(LensError::UnsupportedModel(other.to_string())),
        };

        let m = params
            .get("camera_matrix")
            .and_then(|m| m.as_array())
            .ok_or(LensError::BadCameraMatrix)?;
        if m.len() != 3 {
            return Err(LensError::BadCameraMatrix);
        }
        let row = |i: usize, j: usize| -> Result<f64, LensError> {
            m[i].as_array()
                .and_then(|r| r.get(j))
                .and_then(|x| x.as_f64())
                .ok_or(LensError::BadCameraMatrix)
        };
        let (fx, fy, cx, cy) = (row(0, 0)?, row(1, 1)?, row(0, 2)?, row(1, 2)?);

        let mut k = [0.0f64; 4];
        if model == DistortionModel::Fisheye {
            let coeffs = params
                .get("distortion_coeffs")
                .and_then(|c| c.as_array())
                .ok_or(LensError::BadCoeffCount { found: 0 })?;
            if coeffs.len() != 4 {
                return Err(LensError::BadCoeffCount {
                    found: coeffs.len(),
                });
            }
            for (i, c) in coeffs.iter().enumerate() {
                k[i] = c.as_f64().ok_or(LensError::NonFinite)?;
            }
        }

        let dim = v.get("calib_dimension");
        let calib_width = dim
            .and_then(|d| d.get("w"))
            .and_then(|x| x.as_f64())
            .unwrap_or(cx * 2.0);
        let calib_height = dim
            .and_then(|d| d.get("h"))
            .and_then(|x| x.as_f64())
            .unwrap_or(cy * 2.0);

        let profile = LensProfile {
            model,
            fx,
            fy,
            cx,
            cy,
            k,
            calib_width,
            calib_height,
            // A declared 0.0 means "unmeasured" in practice, not "instantaneous
            // readout"; treat it as absent so nothing downstream trusts it.
            frame_readout_time_s: v
                .get("frame_readout_time")
                .and_then(|x| x.as_f64())
                .filter(|t| *t > 0.0),
            global_shutter: v
                .get("global_shutter")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
            name: v
                .get("name")
                .and_then(|x| x.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    let brand = v.get("camera_brand").and_then(|x| x.as_str()).unwrap_or("");
                    let model = v.get("camera_model").and_then(|x| x.as_str()).unwrap_or("");
                    format!("{brand} {model}").trim().to_string()
                }),
        };
        profile.validate()?;
        Ok(profile)
    }

    /// Read and parse a profile from disk.
    pub fn from_path(path: &std::path::Path) -> Result<Self, LensError> {
        Self::from_json(&std::fs::read_to_string(path).map_err(LensError::Io)?)
    }
}

/// `θ_d = θ(1 + k1θ² + k2θ⁴ + k3θ⁶ + k4θ⁸)` — Kannala-Brandt forward distortion.
fn distort_theta(theta: f64, k: &[f64; 4]) -> f64 {
    let t2 = theta * theta;
    let t4 = t2 * t2;
    let t6 = t4 * t2;
    let t8 = t4 * t4;
    theta * (1.0 + k[0] * t2 + k[1] * t4 + k[2] * t6 + k[3] * t8)
}

/// Invert [`distort_theta`] by Newton's method.
///
/// The series is monotonic over the range any real lens covers, so Newton from
/// `θ = θ_d` converges in a handful of iterations. The iteration count is fixed
/// rather than tolerance-driven so the CPU reference and the GPU shader perform
/// *identical* arithmetic — a convergence-dependent loop would make CPU/GPU
/// parity depend on rounding.
fn undistort_theta(theta_d: f64, k: &[f64; 4]) -> f64 {
    let mut theta = theta_d;
    for _ in 0..UNDISTORT_ITERATIONS {
        let t2 = theta * theta;
        let t4 = t2 * t2;
        let t6 = t4 * t2;
        let t8 = t4 * t4;
        let f = theta * (1.0 + k[0] * t2 + k[1] * t4 + k[2] * t6 + k[3] * t8) - theta_d;
        let df = 1.0 + 3.0 * k[0] * t2 + 5.0 * k[1] * t4 + 7.0 * k[2] * t6 + 9.0 * k[3] * t8;
        if df.abs() < 1e-12 {
            break;
        }
        theta -= f / df;
    }
    theta
}

/// Newton iterations for [`undistort_theta`]. Shared with the WGSL twin so both
/// sides do the same work.
pub const UNDISTORT_ITERATIONS: u32 = 8;

#[cfg(test)]
mod tests {
    use super::*;

    fn gopro_like() -> LensProfile {
        // Representative wide action-cam calibration shape; coefficients chosen
        // to be strongly barrel-distorting so round-trips are a real test.
        LensProfile {
            model: DistortionModel::Fisheye,
            fx: 900.0,
            fy: 900.0,
            cx: 960.0,
            cy: 540.0,
            k: [0.02, -0.004, 0.0007, -0.00005],
            calib_width: 1920.0,
            calib_height: 1080.0,
            frame_readout_time_s: Some(0.0125),
            global_shutter: false,
            name: "test fisheye".into(),
        }
    }

    #[test]
    fn fisheye_project_unproject_round_trips() {
        let lens = gopro_like();
        let (w, h) = (1920.0, 1080.0);
        for &(px, py) in &[
            (960.0, 540.0),
            (100.0, 100.0),
            (1820.0, 980.0),
            (0.0, 540.0),
            (1919.0, 1079.0),
        ] {
            let ray = lens.unproject(px, py, w, h);
            assert!(
                (ray.length() - 1.0).abs() < 1e-9,
                "unproject returns a unit ray"
            );
            let (qx, qy) = lens.project(ray, w, h).unwrap();
            assert!(
                (qx - px).abs() < 1e-6 && (qy - py).abs() < 1e-6,
                "round trip drifted: ({px},{py}) -> ({qx},{qy})"
            );
        }
    }

    #[test]
    fn pinhole_project_unproject_round_trips() {
        let lens = LensProfile::ideal_pinhole(1920.0, 1080.0, 90.0);
        for &(px, py) in &[(960.0, 540.0), (10.0, 20.0), (1900.0, 1000.0)] {
            let ray = lens.unproject(px, py, 1920.0, 1080.0);
            let (qx, qy) = lens.project(ray, 1920.0, 1080.0).unwrap();
            assert!((qx - px).abs() < 1e-9 && (qy - py).abs() < 1e-9);
        }
    }

    #[test]
    fn ideal_pinhole_hfov_is_exact() {
        // At 90° hfov the image half-width subtends 45°, so the ray through the
        // left edge is 45° off axis.
        let lens = LensProfile::ideal_pinhole(1920.0, 1080.0, 90.0);
        let ray = lens.unproject(0.0, 540.0, 1920.0, 1080.0);
        let angle = ray.x.atan2(ray.z).abs().to_degrees();
        assert!((angle - 45.0).abs() < 1e-9, "got {angle}°");
    }

    #[test]
    fn pinhole_rejects_rays_behind_the_camera() {
        let lens = LensProfile::ideal_pinhole(1920.0, 1080.0, 90.0);
        assert!(lens
            .project(DVec3::new(0.0, 0.0, -1.0), 1920.0, 1080.0)
            .is_none());
    }

    #[test]
    fn optical_axis_maps_to_the_principal_point() {
        let lens = gopro_like();
        let (x, y) = lens.project(DVec3::Z, 1920.0, 1080.0).unwrap();
        assert!((x - 960.0).abs() < 1e-9 && (y - 540.0).abs() < 1e-9);
    }

    #[test]
    fn intrinsics_rescale_with_delivery_resolution() {
        let lens = gopro_like();
        let (fx, fy, cx, cy) = lens.intrinsics_for(3840.0, 2160.0);
        assert!((fx - 1800.0).abs() < 1e-9);
        assert!((fy - 1800.0).abs() < 1e-9);
        assert!((cx - 1920.0).abs() < 1e-9);
        assert!((cy - 1080.0).abs() < 1e-9);
        // And the projection is resolution-consistent: the same ray lands at
        // the same *relative* position.
        let ray = lens.unproject(480.0, 270.0, 1920.0, 1080.0);
        let (px, py) = lens.project(ray, 3840.0, 2160.0).unwrap();
        assert!((px - 960.0).abs() < 1e-6 && (py - 540.0).abs() < 1e-6);
    }

    #[test]
    fn theta_distortion_inverts() {
        let k = [0.02, -0.004, 0.0007, -0.00005];
        for deg in [0.5_f64, 5.0, 20.0, 45.0, 80.0, 100.0] {
            let theta = deg.to_radians();
            let back = undistort_theta(distort_theta(theta, &k), &k);
            assert!((back - theta).abs() < 1e-10, "{deg}° -> {back}");
        }
    }

    #[test]
    fn parses_community_profile_shape() {
        let json = r#"{
            "name": "DJI Test 4K",
            "camera_brand": "DJI",
            "camera_model": "TEST",
            "calib_dimension": { "w": 1920, "h": 1080 },
            "frame_readout_time": 0.0125,
            "global_shutter": false,
            "fisheye_params": {
                "camera_matrix": [[900.0,0.0,960.0],[0.0,900.0,540.0],[0.0,0.0,1.0]],
                "distortion_coeffs": [0.02,-0.004,0.0007,-0.00005]
            }
        }"#;
        let lens = LensProfile::from_json(json).unwrap();
        assert_eq!(lens.model, DistortionModel::Fisheye);
        assert_eq!(lens.fx, 900.0);
        assert_eq!(lens.cy, 540.0);
        assert_eq!(lens.k[0], 0.02);
        assert_eq!(lens.calib_width, 1920.0);
        assert_eq!(lens.frame_readout_time_s, Some(0.0125));
        assert_eq!(lens.name, "DJI Test 4K");
    }

    #[test]
    fn zero_readout_time_is_treated_as_unmeasured() {
        // Many published profiles carry 0.0 meaning "nobody measured this",
        // not "this sensor reads out instantaneously".
        let json = r#"{
            "calib_dimension": { "w": 1920, "h": 1080 },
            "frame_readout_time": 0.0,
            "fisheye_params": {
                "camera_matrix": [[900,0,960],[0,900,540],[0,0,1]],
                "distortion_coeffs": [0,0,0,0]
            }
        }"#;
        assert_eq!(
            LensProfile::from_json(json).unwrap().frame_readout_time_s,
            None
        );
    }

    #[test]
    fn malformed_profiles_are_rejected() {
        let bad_matrix =
            r#"{"fisheye_params":{"camera_matrix":[[1,2]],"distortion_coeffs":[0,0,0,0]}}"#;
        assert!(matches!(
            LensProfile::from_json(bad_matrix),
            Err(LensError::BadCameraMatrix)
        ));

        let wrong_coeffs = r#"{"calib_dimension":{"w":1920,"h":1080},
            "fisheye_params":{"camera_matrix":[[900,0,960],[0,900,540],[0,0,1]],
            "distortion_coeffs":[0,0]}}"#;
        assert!(matches!(
            LensProfile::from_json(wrong_coeffs),
            Err(LensError::BadCoeffCount { found: 2 })
        ));

        let zero_focal = r#"{"calib_dimension":{"w":1920,"h":1080},
            "fisheye_params":{"camera_matrix":[[0,0,960],[0,0,540],[0,0,1]],
            "distortion_coeffs":[0,0,0,0]}}"#;
        assert!(matches!(
            LensProfile::from_json(zero_focal),
            Err(LensError::NonPositiveFocal { .. })
        ));

        let unknown_model = r#"{"distortion_model":"some_future_model",
            "fisheye_params":{"camera_matrix":[[900,0,960],[0,900,540],[0,0,1]],
            "distortion_coeffs":[0,0,0,0]}}"#;
        assert!(
            matches!(
                LensProfile::from_json(unknown_model),
                Err(LensError::UnsupportedModel(_))
            ),
            "an unknown lens model must hard-fail, not fall back to fisheye"
        );
    }
}
