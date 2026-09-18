//! Gyro-metadata stabilization recipe (D-12, 22 §6.3).
//!
//! This module owns the **persisted** half of stabilization: where the motion
//! data comes from, how its clock maps onto video time, which lens calibration
//! applies, and the user's strength/crop recipe. It is pure data — no parsing,
//! no integration, no warp. The analysis pipeline lives in
//! `photonic-video::graph::stabilize`, and its outputs (resampled orientation,
//! crop path) stay in the versioned analysis cache keyed by
//! [`StabilizationSpec::analysis_key`], never in the project file (22 §6.3).
//!
//! ## Why the split
//!
//! A gyro series is tens of thousands of samples per minute. Persisting it
//! would bloat every `.photon` and make undo snapshots enormous, so the project
//! stores only the *identity* of the motion source plus the recipe, and the
//! derived path is regenerated (or read from cache) on demand. 22 §6.5 makes
//! this explicit: the analysis cache is **generation, not history** — removing
//! stabilization restores the source path without deleting metadata or cache.
//!
//! ## Clean-room note
//!
//! The stabilization math this recipe drives is Photonic-authored per
//! 23 §9.3; no `GPL-3.0` stabilization source was consulted. The `.gcsv`
//! source format is read from its published specification, and the lens model
//! is the Kannala-Brandt generic camera model (TPAMI 28(8), 2006).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::ids::AssetId;
use super::time::Tick;
use super::unknown::UnknownTag;

/// Serialized wire format of a motion-metadata source (22 §6.3's adapter
/// dialects). Open-ended per `docs/format-versions.md`: an unrecognized
/// dialect is preserved verbatim and re-emitted on save, and **hard-fails at
/// parse time rather than being guessed at** (22 §6.6).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MotionFormat {
    /// The documented Photonic gyro JSON interchange — the required test
    /// adapter (22 §6.3). Dependency-free, and the format synthetic fixtures
    /// are authored in.
    PhotonicJson,
    /// The published `.gcsv` IMU-log text format. Read clean-room from its
    /// public specification; widely emitted by third-party loggers.
    Gcsv,
    /// Telemetry embedded in the media container itself (e.g. a camera's
    /// private MP4 box). Requires a dialect adapter that has passed its own
    /// 23 §9.1 intake gate before it can claim support for a device.
    Embedded,
    /// Forward-compat (39 §2.2): a dialect this build does not know. The
    /// serialized tag is preserved verbatim. Declared last so serde tries the
    /// known snake_case tags first.
    #[serde(untagged)]
    Unknown(UnknownTag),
}

impl MotionFormat {
    /// The preserved tag if this is an unknown (forward-compat) dialect.
    pub fn unknown_tag(self) -> Option<UnknownTag> {
        match self {
            MotionFormat::Unknown(t) => Some(t),
            _ => None,
        }
    }

    /// True if this is a dialect this build does not understand.
    pub fn is_unknown(self) -> bool {
        matches!(self, MotionFormat::Unknown(_))
    }
}

/// Where a clip's motion metadata comes from (22 §6.3 `MotionSourceRef`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "motion_source", rename_all = "snake_case")]
pub enum MotionSourceRef {
    /// Telemetry carried inside the clip's own media asset.
    Embedded { asset: AssetId },
    /// A sidecar file. `rel_path` mirrors [`AssetSource::File`]'s relink
    /// strategy so a moved project still resolves.
    ///
    /// [`AssetSource::File`]: super::media::AssetSource::File
    Sidecar {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rel_path: Option<PathBuf>,
        format: MotionFormat,
    },
}

impl MotionSourceRef {
    /// The declared wire format, for adapter dispatch.
    pub fn format(&self) -> MotionFormat {
        match self {
            MotionSourceRef::Embedded { .. } => MotionFormat::Embedded,
            MotionSourceRef::Sidecar { format, .. } => *format,
        }
    }
}

/// One `(video_tick, sensor_time_ns)` correspondence (22 §6.4).
///
/// Sensor time is **never** assumed equal to video PTS — most cameras record
/// motion outside the video pipeline, so the two clocks differ by an offset
/// and, over a long clip, by rate.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MotionSyncAnchor {
    /// Source-video position, in the media timeline. The analysis range carries
    /// the clip's trim/speed mapping so corrections remain paired with frames
    /// after slipping, trimming, or reversing a clip.
    pub video_tick: Tick,
    /// The sensor timestamp that corresponds to `video_tick`.
    pub sensor_time_ns: i64,
}

/// Gyro-to-video clock mapping (22 §6.4).
///
/// Anchor count selects the model, deliberately: zero anchors means the dialect
/// declares its own alignment (some cameras embed a fixed, known relationship);
/// one anchor is a pure offset; two or more fit an affine map and yield a drift
/// diagnostic. Optical-flow auto-sync is out of scope for v1 (22 §6.8).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MotionSync {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<MotionSyncAnchor>,
}

impl MotionSync {
    /// The dialect declares its own alignment; no user anchors supplied.
    pub fn dialect_declared() -> Self {
        MotionSync {
            anchors: Vec::new(),
        }
    }

    /// True when two or more anchors are present, so an affine (offset + rate)
    /// mapping can be fitted and drift reported.
    pub fn fits_affine(&self) -> bool {
        self.anchors.len() >= 2
    }
}

/// Which lens calibration applies (23 §9.2's three supported sources).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "lens", rename_all = "snake_case")]
pub enum LensProfileRef {
    /// No calibrated lens: correct rotation only, do not undistort.
    ///
    /// 22 §6.6 permits this **only when explicitly selected** — it is never an
    /// implicit fallback for a missing profile, because silently skipping
    /// undistortion produces a plausible-looking but geometrically wrong
    /// result. Full acceptance requires a calibrated lens.
    RotationOnly,
    /// A user-installed profile, treated as user data (23 §9.2). Carries no
    /// redistribution obligation because Photonic never ships it.
    UserFile {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rel_path: Option<PathBuf>,
    },
    /// An entry from a bundled, per-entry-reviewed snapshot, by stable id.
    ///
    /// No snapshot ships until 23 §9.2 per-entry intake passes, so a document
    /// referencing one resolves to "profile unavailable" (a diagnostic) rather
    /// than silently degrading to [`LensProfileRef::RotationOnly`].
    Bundled { id: String },
}

impl LensProfileRef {
    /// True when the recipe asks for geometric undistortion.
    pub fn is_calibrated(&self) -> bool {
        !matches!(self, LensProfileRef::RotationOnly)
    }
}

/// Motion source + clock mapping + lens identity (22 §6.3 `MotionBinding`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MotionBinding {
    pub source: MotionSourceRef,
    #[serde(default)]
    pub sync: MotionSync,
    pub lens: LensProfileRef,
}

/// One IMU reading (22 §6.3 `MotionSample`).
///
/// Runtime type: samples live in the analysis cache, never in the project file.
/// `f64` throughout because integration accumulates over tens of thousands of
/// samples and `f32` drift is visible as horizon creep by the end of a clip.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MotionSample {
    /// Sensor clock, nanoseconds. Not video PTS — see [`MotionSync`].
    pub sensor_time_ns: i64,
    /// Angular velocity, radians/second, in the normalized Photonic axis frame.
    pub gyro_rad_s: [f64; 3],
    /// Specific force, m/s². Present only when the dialect carries it; required
    /// for gravity-referenced horizon lock.
    pub accel_mps2: Option<[f64; 3]>,
    /// Pre-integrated orientation `[x, y, z, w]`, when the camera records it
    /// directly. Preferred over integrating when present and valid.
    pub orientation: Option<[f64; 4]>,
}

impl MotionSample {
    /// True when every present component is finite (22 §6.6: NaN samples are
    /// discarded and counted, never silently propagated into the integrator).
    pub fn is_finite(&self) -> bool {
        self.gyro_rad_s.iter().all(|v| v.is_finite())
            && self
                .accel_mps2
                .is_none_or(|a| a.iter().all(|v| v.is_finite()))
            && self.orientation.is_none_or(|q| {
                // A quaternion of zero length carries no rotation and would
                // normalize to NaN; reject it here rather than downstream.
                q.iter().all(|v| v.is_finite()) && q.iter().any(|v| *v != 0.0)
            })
    }
}

/// How the stabilizer hides the source edges the correction rotation exposes
/// (22 §6.3 `StabilizationCropMode`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StabilizationCropMode {
    /// One fixed zoom for the whole clip, chosen so no frame ever exposes an
    /// edge. Predictable and the safest default; costs the most field of view.
    #[default]
    StaticSafe,
    /// Zoom tracks the per-frame requirement, smoothed to avoid visible
    /// pumping. Keeps more field of view on calm footage.
    Dynamic,
    /// Do not zoom; leave uncovered pixels transparent for downstream
    /// compositing to handle.
    TransparentEdges,
    /// Forward-compat (39 §2.2): preserved verbatim, re-emitted on save.
    /// Declared last so serde tries the known snake_case tags first.
    #[serde(untagged)]
    Unknown(UnknownTag),
}

impl StabilizationCropMode {
    /// The preserved tag if this is an unknown (forward-compat) variant.
    pub fn unknown_tag(self) -> Option<UnknownTag> {
        match self {
            StabilizationCropMode::Unknown(t) => Some(t),
            _ => None,
        }
    }

    /// True if this is a variant this build does not understand.
    pub fn is_unknown(self) -> bool {
        matches!(self, StabilizationCropMode::Unknown(_))
    }
}

/// Why a [`StabilizationSpec`] cannot be applied.
///
/// Separate from the editor-facing `ops::EditError` so importers, the MCP
/// layer, and the analysis worker can all validate a recipe before building a
/// command — the same split [`SpeedMapError`] makes.
///
/// [`SpeedMapError`]: super::clip::SpeedMapError
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StabilizationError {
    /// `smoothness` outside `0.0..=1.0`, or not finite.
    SmoothnessOutOfRange,
    /// `horizon_lock` outside `0.0..=1.0`, or not finite.
    HorizonLockOutOfRange,
    /// `max_zoom` below `1.0` (a "zoom" that shrinks would expose edges by
    /// construction), or not finite.
    MaxZoomOutOfRange,
    /// Anchors are not strictly increasing in video time, so the clock map
    /// would be non-monotonic.
    NonMonotonicSyncAnchors,
    /// Two anchors share a sensor timestamp, so the affine rate is undefined.
    DegenerateSyncAnchors,
    /// The recipe names a dialect or crop mode this build does not understand.
    /// 22 §6.6: hard-fail, never guess.
    UnknownDialect,
}

fn default_smoothness() -> f32 {
    0.5
}

fn default_max_zoom() -> f32 {
    1.3
}

/// The persisted stabilization recipe for one clip (22 §6.3).
///
/// Hangs off `Clip.stabilization` as a serde-additive `Option`, so a document
/// written by a build without D-12 round-trips unchanged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StabilizationSpec {
    pub binding: MotionBinding,
    /// How hard to smooth the orientation path, `0.0..=1.0`. `0.0` follows the
    /// raw camera motion exactly (a no-op); `1.0` is maximally smooth and
    /// demands the most crop.
    #[serde(default = "default_smoothness")]
    pub smoothness: f32,
    /// Strength of gravity-referenced horizon leveling, `0.0..=1.0`. `0.0`
    /// leaves roll alone; `1.0` pins the horizon level regardless of airframe
    /// roll. Requires accelerometer data to have any effect.
    #[serde(default)]
    pub horizon_lock: f32,
    #[serde(default)]
    pub crop_mode: StabilizationCropMode,
    /// Ceiling on the zoom the crop solver may apply, `>= 1.0`.
    #[serde(default = "default_max_zoom")]
    pub max_zoom: f32,
    /// Handle into the analysis cache for the derived orientation/crop path.
    /// `None` means "not analyzed yet"; the engine falls back to passthrough
    /// rather than guessing (22 §6.6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis_key: Option<String>,
}

impl StabilizationSpec {
    /// A recipe bound to `binding` with the default strengths.
    pub fn new(binding: MotionBinding) -> Self {
        StabilizationSpec {
            binding,
            smoothness: default_smoothness(),
            horizon_lock: 0.0,
            crop_mode: StabilizationCropMode::default(),
            max_zoom: default_max_zoom(),
            analysis_key: None,
        }
    }

    /// Validate the recipe. Called before storage and before analysis.
    ///
    /// Rejects rather than clamps: a silently clamped `max_zoom` would produce
    /// a stabilized result the user did not ask for, and 22 §6.6 requires the
    /// impossible case be *reported*, not absorbed.
    pub fn validate(&self) -> Result<(), StabilizationError> {
        if !self.smoothness.is_finite() || !(0.0..=1.0).contains(&self.smoothness) {
            return Err(StabilizationError::SmoothnessOutOfRange);
        }
        if !self.horizon_lock.is_finite() || !(0.0..=1.0).contains(&self.horizon_lock) {
            return Err(StabilizationError::HorizonLockOutOfRange);
        }
        if !self.max_zoom.is_finite() || self.max_zoom < 1.0 {
            return Err(StabilizationError::MaxZoomOutOfRange);
        }
        if self.crop_mode.is_unknown() || self.binding.source.format().is_unknown() {
            return Err(StabilizationError::UnknownDialect);
        }
        let anchors = &self.binding.sync.anchors;
        for pair in anchors.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if b.video_tick <= a.video_tick {
                return Err(StabilizationError::NonMonotonicSyncAnchors);
            }
            if b.sensor_time_ns == a.sensor_time_ns {
                return Err(StabilizationError::DegenerateSyncAnchors);
            }
        }
        Ok(())
    }

    /// True when the recipe would leave the image untouched, so the compiler
    /// can skip emitting a warp op entirely.
    pub fn is_identity(&self) -> bool {
        self.analysis_key.is_none() || (self.smoothness == 0.0 && self.horizon_lock == 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> MotionBinding {
        MotionBinding {
            source: MotionSourceRef::Sidecar {
                path: PathBuf::from("/clips/flight.gcsv"),
                rel_path: None,
                format: MotionFormat::Gcsv,
            },
            sync: MotionSync::default(),
            lens: LensProfileRef::RotationOnly,
        }
    }

    #[test]
    fn default_recipe_validates() {
        assert_eq!(StabilizationSpec::new(binding()).validate(), Ok(()));
    }

    #[test]
    fn out_of_range_strengths_are_rejected_not_clamped() {
        let mut s = StabilizationSpec::new(binding());
        s.smoothness = 1.5;
        assert_eq!(
            s.validate(),
            Err(StabilizationError::SmoothnessOutOfRange),
            "an out-of-range strength must be reported, never silently clamped"
        );

        let mut s = StabilizationSpec::new(binding());
        s.horizon_lock = f32::NAN;
        assert_eq!(s.validate(), Err(StabilizationError::HorizonLockOutOfRange));

        let mut s = StabilizationSpec::new(binding());
        s.max_zoom = 0.9;
        assert_eq!(
            s.validate(),
            Err(StabilizationError::MaxZoomOutOfRange),
            "a sub-unity zoom exposes edges by construction"
        );
    }

    #[test]
    fn unknown_dialect_hard_fails() {
        let mut s = StabilizationSpec::new(binding());
        s.binding.source = MotionSourceRef::Sidecar {
            path: PathBuf::from("/clips/flight.xyz"),
            rel_path: None,
            format: MotionFormat::Unknown(UnknownTag::intern("some_future_dialect")),
        };
        assert_eq!(
            s.validate(),
            Err(StabilizationError::UnknownDialect),
            "22 §6.6: unknown dialect hard-fails, never guesses"
        );
    }

    #[test]
    fn sync_anchors_must_be_monotonic_and_non_degenerate() {
        let mut s = StabilizationSpec::new(binding());
        s.binding.sync.anchors = vec![
            MotionSyncAnchor {
                video_tick: Tick(1000),
                sensor_time_ns: 0,
            },
            MotionSyncAnchor {
                video_tick: Tick(500),
                sensor_time_ns: 1_000_000,
            },
        ];
        assert_eq!(
            s.validate(),
            Err(StabilizationError::NonMonotonicSyncAnchors)
        );

        s.binding.sync.anchors = vec![
            MotionSyncAnchor {
                video_tick: Tick(0),
                sensor_time_ns: 42,
            },
            MotionSyncAnchor {
                video_tick: Tick(1000),
                sensor_time_ns: 42,
            },
        ];
        assert_eq!(
            s.validate(),
            Err(StabilizationError::DegenerateSyncAnchors),
            "two anchors sharing a sensor timestamp leave the affine rate undefined"
        );
    }

    #[test]
    fn anchor_count_selects_the_clock_model() {
        let mut sync = MotionSync::dialect_declared();
        assert!(!sync.fits_affine(), "zero anchors: dialect-declared");
        sync.anchors.push(MotionSyncAnchor {
            video_tick: Tick(0),
            sensor_time_ns: 0,
        });
        assert!(!sync.fits_affine(), "one anchor: offset only");
        sync.anchors.push(MotionSyncAnchor {
            video_tick: Tick(1000),
            sensor_time_ns: 1_000_000,
        });
        assert!(sync.fits_affine(), "two anchors: affine offset + rate");
    }

    #[test]
    fn non_finite_samples_are_rejected() {
        let good = MotionSample {
            sensor_time_ns: 0,
            gyro_rad_s: [0.1, 0.2, 0.3],
            accel_mps2: Some([0.0, 0.0, 9.81]),
            orientation: None,
        };
        assert!(good.is_finite());

        let nan_gyro = MotionSample {
            gyro_rad_s: [f64::NAN, 0.0, 0.0],
            ..good
        };
        assert!(!nan_gyro.is_finite());

        let zero_quat = MotionSample {
            orientation: Some([0.0, 0.0, 0.0, 0.0]),
            ..good
        };
        assert!(
            !zero_quat.is_finite(),
            "a zero-length quaternion normalizes to NaN; reject at the door"
        );
    }

    #[test]
    fn unknown_crop_mode_round_trips_verbatim() {
        // 39 §2.2: a variant a newer build wrote must survive a load/save cycle
        // through this build byte-for-byte.
        let json = "\"some_future_mode\"";
        let mode: StabilizationCropMode = serde_json::from_str(json).unwrap();
        assert!(mode.is_unknown());
        assert_eq!(serde_json::to_string(&mode).unwrap(), json);
    }

    #[test]
    fn spec_round_trips_through_json() {
        let mut s = StabilizationSpec::new(binding());
        s.smoothness = 0.75;
        s.horizon_lock = 0.25;
        s.crop_mode = StabilizationCropMode::Dynamic;
        s.analysis_key = Some("abc123".into());
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<StabilizationSpec>(&json).unwrap(), s);
    }
}
