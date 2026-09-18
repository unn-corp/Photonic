//! Motion-metadata (gyro/IMU) ingest for D-12 stabilization (22 §6.3).
//!
//! Sibling to [`probe`](super::probe): where `probe` asks `ffprobe` what the
//! *pixels* look like, this module reads what the *camera was doing* while it
//! recorded them. The output is a [`MotionSeries`] — angular velocity resampled
//! into a single normalized convention — which
//! `graph::stabilize` integrates into an orientation path.
//!
//! ## Normalization is this module's job
//!
//! Every dialect stores motion differently: different axis orders, different
//! handedness, different units (raw ADC counts, degrees/second, radians/second),
//! different clocks. Downstream math must not care. So an adapter's contract is
//! to emit samples already in the **Photonic motion frame**:
//!
//! - `+X` right, `+Y` down, `+Z` forward along the optical axis (right-handed,
//!   the usual computer-vision camera frame).
//! - Angular velocity in **radians per second**, positive by the right-hand rule.
//! - Specific force in **m/s²**.
//! - Timestamps in **nanoseconds on the sensor clock** — deliberately *not*
//!   video PTS. Mapping the two is [`photonic_core::timeline::MotionSync`]'s
//!   job, and 22 §6.4 forbids assuming they are equal.
//!
//! ## Hard-fail, never guess
//!
//! 22 §6.6 is explicit: "Unknown dialect/axis/units: hard fail, never guess."
//! A misread axis order does not produce an obviously broken result — it
//! produces a *plausible* one that stabilizes the wrong way, which is far worse
//! because it survives review. Every ambiguity in this module therefore returns
//! [`MotionError`] rather than picking a default.
//!
//! ## Untrusted input
//!
//! 22 §6.6 requires treating metadata as untrusted binary: bounded sizes and
//! sample counts, checked arithmetic, no `unsafe`. The bounds live in
//! [`limits`] and are enforced before allocation, not after.
//!
//! ## Clean-room provenance
//!
//! The `.gcsv` reader is written from that format's **published specification**
//! (an open interchange format documented for third-party loggers), not from
//! reading any `GPL-3.0` implementation. See 23 §9 and the §14 D-12 disposition.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use photonic_core::timeline::{MotionFormat, MotionSample};

/// Bounds applied to untrusted metadata before allocation (22 §6.6).
pub mod limits {
    /// Largest sidecar file this module will read into memory.
    ///
    /// A `.gcsv` at 8 kHz with 10 fields per row runs roughly 60 MB/hour of
    /// flight, so 512 MiB covers any realistic clip while still refusing a file
    /// crafted to exhaust memory.
    pub const MAX_SIDECAR_BYTES: u64 = 512 * 1024 * 1024;

    /// Largest sample count accepted from any dialect.
    ///
    /// 16M samples is ~33 minutes at 8 kHz — beyond any single clip — and caps
    /// the `Vec<MotionSample>` allocation at a few hundred MB.
    pub const MAX_SAMPLES: usize = 16_000_000;

    /// Longest single header line in a text dialect.
    pub const MAX_HEADER_LINE_BYTES: usize = 8 * 1024;

    /// Fraction of samples that may be dropped as non-finite before the series
    /// is rejected outright (22 §6.6: "excessive invalid data fails").
    pub const MAX_INVALID_FRACTION: f64 = 0.05;
}

/// Why motion metadata could not be read.
#[derive(Debug, thiserror::Error)]
pub enum MotionError {
    #[error("could not read motion sidecar: {0}")]
    Io(#[source] std::io::Error),
    #[error("motion sidecar exceeds the {} MiB limit", limits::MAX_SIDECAR_BYTES / 1024 / 1024)]
    TooLarge,
    #[error("sample count {count} exceeds the {} limit", limits::MAX_SAMPLES)]
    TooManySamples { count: usize },
    #[error("header line exceeds {} bytes", limits::MAX_HEADER_LINE_BYTES)]
    HeaderLineTooLong,
    #[error("not a recognized motion-metadata dialect")]
    UnrecognizedDialect,
    #[error("malformed {dialect} at line {line}: {detail}")]
    Malformed {
        dialect: &'static str,
        line: usize,
        detail: String,
    },
    /// The file parsed, but declares something this build cannot interpret.
    /// Never downgraded to a default (22 §6.6).
    #[error("unsupported {field} {value:?} — refusing to guess")]
    Unsupported { field: &'static str, value: String },
    #[error("motion series contains no usable samples")]
    Empty,
    #[error("{dropped} of {total} samples were non-finite, above the {}% tolerance",
        (limits::MAX_INVALID_FRACTION * 100.0) as u32)]
    TooManyInvalid { dropped: usize, total: usize },
    #[error("timestamps are not monotonically increasing (first regression at sample {index})")]
    NonMonotonic { index: usize },
    /// The container carries no motion track. Distinct from
    /// [`MotionError::UnrecognizedDialect`] so the GUI can say "this file has no
    /// gyro data" rather than "this file is corrupt".
    #[error("no gyro/IMU track found in this media file")]
    NoMotionTrack,
    /// The container carries telemetry, but at a rate far too low to be an IMU
    /// stream — a flight log (GPS, altitude, exposure), not angular velocity.
    ///
    /// Separate from [`MotionError::NoMotionTrack`] because the two need
    /// opposite responses from the user, and because "no track found" is
    /// actively misleading when the file visibly *has* a telemetry track. A
    /// consumer drone recording a 1 Hz flight log is the single most common
    /// reason gyro stabilization is impossible for a given clip.
    #[error(
        "this clip carries a {hz:.1} Hz telemetry track ({samples} samples over {duration_s:.0}s) \
         — a flight log, not gyro. Stabilization needs angular velocity at hundreds of Hz. \
         Re-record with in-camera stabilization (RockSteady/HorizonSteady) OFF on a camera that \
         logs IMU data, copy the original off the SD card without re-encoding, or supply a \
         .gcsv sidecar."
    )]
    LowRateTelemetryOnly {
        hz: f64,
        samples: u64,
        duration_s: f64,
    },
    /// The file was written by a re-encoder, so any telemetry the camera
    /// recorded has already been stripped.
    ///
    /// Worth its own variant because it is *recoverable*: the original still
    /// exists somewhere, and telling the user that is far more useful than
    /// telling them this copy is empty.
    #[error(
        "this file was re-encoded by {writer} and no longer carries camera telemetry. \
         Gyro data does not survive transcoding — use the original file straight off \
         the camera's SD card."
    )]
    ReencodedCopy { writer: String },
    #[error("{0}")]
    Unimplemented(&'static str),
}

/// How strongly an adapter claims a file (22 §6.3 `sniff`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AdapterConfidence {
    /// Definitely not this dialect.
    No,
    /// Extension or context suggests it, but content was not confirmed.
    Weak,
    /// Content carries this dialect's magic/signature.
    Strong,
}

/// A parsed, normalized motion series.
#[derive(Clone, Debug, PartialEq)]
pub struct MotionSeries {
    /// Samples in the Photonic motion frame, ascending by `sensor_time_ns`.
    pub samples: Vec<MotionSample>,
    /// The dialect this came from.
    pub format: MotionFormat,
    /// Logger/camera identity the file declared, for diagnostics.
    pub id: String,
    /// Sensor readout duration the dialect declared, seconds.
    ///
    /// Parsed and carried because it is cheap and lossy to re-derive, but **not
    /// consumed**: rolling-shutter correction is deferred (22 §6.8). Plumbing
    /// it now means enabling that later needs no format work.
    pub frame_readout_time_s: Option<f64>,
    /// A lens profile the file named, if any. A hint for the UI to preselect;
    /// never auto-applied, since the file cannot vouch for the profile's rights.
    pub lens_hint: Option<String>,
    /// Non-finite samples discarded during parse (22 §6.6 requires the count be
    /// reported, not silently absorbed).
    pub dropped_invalid: usize,
}

impl MotionSeries {
    /// True when accelerometer data is present on every sample, which
    /// gravity-referenced horizon lock requires.
    pub fn has_accel(&self) -> bool {
        !self.samples.is_empty() && self.samples.iter().all(|s| s.accel_mps2.is_some())
    }

    /// Mean sample rate in Hz, or `None` for a series too short to measure.
    pub fn sample_rate_hz(&self) -> Option<f64> {
        let (first, last) = (self.samples.first()?, self.samples.last()?);
        let span_ns = last.sensor_time_ns.checked_sub(first.sensor_time_ns)?;
        if span_ns <= 0 {
            return None;
        }
        Some((self.samples.len() - 1) as f64 * 1e9 / span_ns as f64)
    }

    /// Reject a series that is empty, mostly invalid, or non-monotonic.
    ///
    /// Monotonicity matters because the resampler and integrator both assume
    /// ascending time; a regression would silently integrate a negative `dt`
    /// and tilt the whole orientation path.
    fn finish(mut self, total_seen: usize) -> Result<Self, MotionError> {
        if self.samples.is_empty() {
            return Err(MotionError::Empty);
        }
        if total_seen > 0 {
            let frac = self.dropped_invalid as f64 / total_seen as f64;
            if frac > limits::MAX_INVALID_FRACTION {
                return Err(MotionError::TooManyInvalid {
                    dropped: self.dropped_invalid,
                    total: total_seen,
                });
            }
        }
        for (i, pair) in self.samples.windows(2).enumerate() {
            if pair[1].sensor_time_ns < pair[0].sensor_time_ns {
                return Err(MotionError::NonMonotonic { index: i + 1 });
            }
        }
        self.samples.shrink_to_fit();
        Ok(self)
    }
}

/// The stable boundary between dialects and the stabilizer (22 §6.3, 23 §9.1).
pub trait MotionMetadataAdapter {
    /// Human-readable dialect name, for diagnostics.
    fn name(&self) -> &'static str;
    /// Cheap check for whether this adapter should attempt `source`.
    fn sniff(&self, source: &Path) -> AdapterConfidence;
    /// Parse and normalize. Must not panic on malformed input.
    fn parse(&self, source: &Path) -> Result<MotionSeries, MotionError>;
}

// ── axis normalization ──────────────────────────────────────────────────────

/// A permutation-with-sign mapping a dialect's axes onto the Photonic frame.
///
/// Encoded the way text dialects express it: three characters, one per output
/// axis, naming the source axis (`x`/`y`/`z`) with case carrying sign —
/// uppercase positive, lowercase negated. `"XYZ"` is identity; `"YxZ"` means
/// output-X takes source-Y, output-Y takes negated source-X, output-Z takes
/// source-Z.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AxisMap {
    src: [usize; 3],
    neg: [bool; 3],
}

impl AxisMap {
    /// Source axes already match the Photonic frame.
    pub const IDENTITY: AxisMap = AxisMap {
        src: [0, 1, 2],
        neg: [false, false, false],
    };

    /// Parse a three-character orientation string.
    ///
    /// Rejects anything that is not a permutation: a repeated axis would
    /// collapse a dimension and silently discard rotation about it.
    pub fn parse(s: &str) -> Result<Self, MotionError> {
        let unsupported = || MotionError::Unsupported {
            field: "orientation",
            value: s.to_string(),
        };
        let chars: Vec<char> = s.trim().chars().collect();
        if chars.len() != 3 {
            return Err(unsupported());
        }
        let mut src = [0usize; 3];
        let mut neg = [false; 3];
        for (i, c) in chars.iter().enumerate() {
            src[i] = match c.to_ascii_lowercase() {
                'x' => 0,
                'y' => 1,
                'z' => 2,
                _ => return Err(unsupported()),
            };
            neg[i] = c.is_ascii_lowercase();
        }
        let mut seen = [false; 3];
        for &axis in &src {
            if std::mem::replace(&mut seen[axis], true) {
                return Err(unsupported());
            }
        }
        Ok(AxisMap { src, neg })
    }

    /// Apply the mapping to a source-frame vector.
    pub fn apply(&self, v: [f64; 3]) -> [f64; 3] {
        let mut out = [0.0; 3];
        for i in 0..3 {
            out[i] = if self.neg[i] {
                -v[self.src[i]]
            } else {
                v[self.src[i]]
            };
        }
        out
    }
}

// ── Photonic gyro JSON ──────────────────────────────────────────────────────

/// The documented Photonic gyro JSON interchange (22 §6.3's required test
/// adapter, 23 §9.3 release-sequence step 1).
///
/// Dependency-free and the format synthetic fixtures are authored in, so the
/// whole stabilization pipeline is testable without any camera, container
/// parsing, or third-party crate. Shape:
///
/// ```json
/// {
///   "photonic_gyro": 1,
///   "id": "synthetic-constant-yaw",
///   "orientation": "XYZ",
///   "time_units": "ns",
///   "gyro_units": "rad/s",
///   "accel_units": "m/s^2",
///   "frame_readout_time_s": 0.0,
///   "samples": [
///     { "t": 0,      "gyro": [0.0, 0.0, 0.0], "accel": [0.0, 9.81, 0.0] },
///     { "t": 1000000, "gyro": [0.0, 0.1, 0.0] }
///   ]
/// }
/// ```
///
/// Units are named explicitly rather than assumed, so a file that omits them or
/// names one this build does not know is rejected instead of guessed at.
pub struct PhotonicJsonAdapter;

#[derive(serde::Deserialize)]
struct RawJsonSeries {
    #[serde(default)]
    photonic_gyro: u32,
    #[serde(default)]
    id: String,
    orientation: String,
    time_units: String,
    gyro_units: String,
    #[serde(default)]
    accel_units: Option<String>,
    #[serde(default)]
    frame_readout_time_s: Option<f64>,
    #[serde(default)]
    lens_profile: Option<String>,
    samples: Vec<RawJsonSample>,
}

#[derive(serde::Deserialize)]
struct RawJsonSample {
    t: f64,
    gyro: [f64; 3],
    #[serde(default)]
    accel: Option<[f64; 3]>,
    #[serde(default)]
    orientation: Option<[f64; 4]>,
}

/// Seconds-per-unit for a named time unit.
fn time_scale(units: &str) -> Result<f64, MotionError> {
    match units {
        "ns" | "nanoseconds" => Ok(1e-9),
        "us" | "microseconds" => Ok(1e-6),
        "ms" | "milliseconds" => Ok(1e-3),
        "s" | "seconds" => Ok(1.0),
        other => Err(MotionError::Unsupported {
            field: "time_units",
            value: other.to_string(),
        }),
    }
}

/// Radians-per-second per unit for a named angular-rate unit.
fn gyro_scale(units: &str) -> Result<f64, MotionError> {
    match units {
        "rad/s" | "rad_s" | "radians_per_second" => Ok(1.0),
        "deg/s" | "deg_s" | "degrees_per_second" => Ok(std::f64::consts::PI / 180.0),
        other => Err(MotionError::Unsupported {
            field: "gyro_units",
            value: other.to_string(),
        }),
    }
}

/// m/s² per unit for a named specific-force unit.
fn accel_scale(units: &str) -> Result<f64, MotionError> {
    match units {
        "m/s^2" | "m_s2" | "mps2" => Ok(1.0),
        // Standard gravity, CODATA/SI exact.
        "g" | "g0" => Ok(9.806_65),
        other => Err(MotionError::Unsupported {
            field: "accel_units",
            value: other.to_string(),
        }),
    }
}

fn read_bounded(source: &Path) -> Result<String, MotionError> {
    let meta = fs::metadata(source).map_err(MotionError::Io)?;
    if meta.len() > limits::MAX_SIDECAR_BYTES {
        return Err(MotionError::TooLarge);
    }
    // Re-check while reading: the file could grow between stat and read.
    let file = fs::File::open(source).map_err(MotionError::Io)?;
    let mut buf = String::new();
    file.take(limits::MAX_SIDECAR_BYTES + 1)
        .read_to_string(&mut buf)
        .map_err(MotionError::Io)?;
    if buf.len() as u64 > limits::MAX_SIDECAR_BYTES {
        return Err(MotionError::TooLarge);
    }
    Ok(buf)
}

impl MotionMetadataAdapter for PhotonicJsonAdapter {
    fn name(&self) -> &'static str {
        "photonic gyro JSON"
    }

    fn sniff(&self, source: &Path) -> AdapterConfidence {
        let ext_matches = source
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("json"));
        if !ext_matches {
            return AdapterConfidence::No;
        }
        // Confirm from content: a bare `.json` proves nothing.
        match fs::read_to_string(source) {
            Ok(text) if text.contains("\"photonic_gyro\"") => AdapterConfidence::Strong,
            Ok(_) => AdapterConfidence::Weak,
            Err(_) => AdapterConfidence::No,
        }
    }

    fn parse(&self, source: &Path) -> Result<MotionSeries, MotionError> {
        let text = read_bounded(source)?;
        let raw: RawJsonSeries =
            serde_json::from_str(&text).map_err(|e| MotionError::Malformed {
                dialect: "photonic gyro JSON",
                line: e.line(),
                detail: e.to_string(),
            })?;

        if raw.photonic_gyro != 1 {
            return Err(MotionError::Unsupported {
                field: "photonic_gyro version",
                value: raw.photonic_gyro.to_string(),
            });
        }
        if raw.samples.len() > limits::MAX_SAMPLES {
            return Err(MotionError::TooManySamples {
                count: raw.samples.len(),
            });
        }

        let axes = AxisMap::parse(&raw.orientation)?;
        let t_scale = time_scale(&raw.time_units)?;
        let g_scale = gyro_scale(&raw.gyro_units)?;
        // Only demanded when a sample actually carries acceleration, so a
        // gyro-only file need not declare a unit it never uses.
        let a_scale = match (
            &raw.accel_units,
            raw.samples.iter().any(|s| s.accel.is_some()),
        ) {
            (Some(u), _) => Some(accel_scale(u)?),
            (None, false) => None,
            (None, true) => {
                return Err(MotionError::Unsupported {
                    field: "accel_units",
                    value: "<missing, but samples carry accel>".into(),
                })
            }
        };

        let total = raw.samples.len();
        let mut samples = Vec::with_capacity(total);
        let mut dropped = 0usize;
        for s in raw.samples {
            let accel = match s.accel {
                Some(a) => {
                    let scale = a_scale.expect("accel_units checked above");
                    Some(axes.apply([a[0] * scale, a[1] * scale, a[2] * scale]))
                }
                None => None,
            };
            let sample = MotionSample {
                sensor_time_ns: (s.t * t_scale * 1e9).round() as i64,
                gyro_rad_s: axes.apply([
                    s.gyro[0] * g_scale,
                    s.gyro[1] * g_scale,
                    s.gyro[2] * g_scale,
                ]),
                accel_mps2: accel,
                orientation: s.orientation,
            };
            if sample.is_finite() {
                samples.push(sample);
            } else {
                dropped += 1;
            }
        }

        MotionSeries {
            samples,
            format: MotionFormat::PhotonicJson,
            id: raw.id,
            frame_readout_time_s: raw.frame_readout_time_s,
            lens_hint: raw.lens_profile,
            dropped_invalid: dropped,
        }
        .finish(total)
    }
}

// ── .gcsv ───────────────────────────────────────────────────────────────────

/// Reader for the published `.gcsv` IMU-log text format.
///
/// Structure: an identifier line, `key,value` header lines, a CSV column
/// header, then fixed-point or float rows. Header scale constants convert raw
/// integers into physical units — the format stores compact integers and the
/// multipliers separately.
///
/// Written clean-room from the public format specification; no `GPL-3.0`
/// implementation was consulted (23 §9, §14 D-12 disposition).
pub struct GcsvAdapter;

impl GcsvAdapter {
    /// Both identifier lines the format defines.
    const MAGICS: [&'static str; 2] = ["GYROFLOW IMU LOG", "CAMERA IMU LOG"];
}

impl MotionMetadataAdapter for GcsvAdapter {
    fn name(&self) -> &'static str {
        ".gcsv"
    }

    fn sniff(&self, source: &Path) -> AdapterConfidence {
        let ext_matches = source
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("gcsv"));
        let Ok(file) = fs::File::open(source) else {
            return AdapterConfidence::No;
        };
        let mut first = String::new();
        if BufReader::new(file.take(limits::MAX_HEADER_LINE_BYTES as u64))
            .read_line(&mut first)
            .is_err()
        {
            return AdapterConfidence::No;
        }
        let magic = Self::MAGICS.iter().any(|m| first.trim() == *m);
        match (magic, ext_matches) {
            (true, _) => AdapterConfidence::Strong,
            (false, true) => AdapterConfidence::Weak,
            (false, false) => AdapterConfidence::No,
        }
    }

    fn parse(&self, source: &Path) -> Result<MotionSeries, MotionError> {
        const DIALECT: &str = ".gcsv";
        let text = read_bounded(source)?;
        let mut lines = text.lines().enumerate();

        let (_, magic) = lines.next().ok_or(MotionError::Empty)?;
        if !Self::MAGICS.iter().any(|m| magic.trim() == *m) {
            return Err(MotionError::UnrecognizedDialect);
        }

        // Header: `key,value` pairs until the CSV column header appears.
        // Scale constants default to 1.0 (raw values already physical), which
        // the format permits; the axis orientation has no safe default and must
        // be declared.
        let mut orientation: Option<String> = None;
        let (mut tscale, mut gscale, mut ascale) = (1.0_f64, 1.0_f64, 1.0_f64);
        let mut id = String::new();
        let mut readout: Option<f64> = None;
        let mut lens_hint: Option<String> = None;
        let mut columns: Vec<String> = Vec::new();
        let mut header_end = 0usize;

        for (idx, line) in lines.by_ref() {
            if line.len() > limits::MAX_HEADER_LINE_BYTES {
                return Err(MotionError::HeaderLineTooLong);
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // The column header is the first line whose leading field is `t`.
            if line.split(',').next().map(str::trim) == Some("t") {
                columns = line
                    .split(',')
                    .map(|c| c.trim().to_ascii_lowercase())
                    .collect();
                header_end = idx;
                break;
            }
            let Some((key, value)) = line.split_once(',') else {
                return Err(MotionError::Malformed {
                    dialect: DIALECT,
                    line: idx + 1,
                    detail: format!("header line is not `key,value`: {line:?}"),
                });
            };
            let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
            let num = |field: &'static str| -> Result<f64, MotionError> {
                value.parse::<f64>().map_err(|_| MotionError::Unsupported {
                    field,
                    value: value.to_string(),
                })
            };
            match key.as_str() {
                "orientation" => orientation = Some(value.to_string()),
                "tscale" => tscale = num("tscale")?,
                "gscale" => gscale = num("gscale")?,
                "ascale" => ascale = num("ascale")?,
                "id" => id = value.to_string(),
                "frame_readout_time" => readout = Some(num("frame_readout_time")?),
                "lensprofile" => lens_hint = Some(value.to_string()),
                // `version`, `note`, `vendor`, `fwversion`, `timestamp`,
                // `videofilename`, `mscale`, `lens_info`, and
                // `frame_readout_direction` are recognized-but-unused: they
                // carry no information the stabilizer consumes. Ignoring a
                // known-irrelevant key is not guessing.
                _ => {}
            }
        }

        if columns.is_empty() {
            return Err(MotionError::Malformed {
                dialect: DIALECT,
                line: header_end + 1,
                detail: "no `t,gx,gy,gz…` column header found".into(),
            });
        }
        // 22 §6.6: no axis convention means no safe interpretation.
        let axes = AxisMap::parse(&orientation.ok_or(MotionError::Unsupported {
            field: "orientation",
            value: "<missing>".into(),
        })?)?;

        let col = |name: &str| columns.iter().position(|c| c == name);
        let (Some(ct), Some(cgx), Some(cgy), Some(cgz)) =
            (col("t"), col("gx"), col("gy"), col("gz"))
        else {
            return Err(MotionError::Malformed {
                dialect: DIALECT,
                line: header_end + 1,
                detail: format!("column header missing t/gx/gy/gz: {columns:?}"),
            });
        };
        let accel_cols = match (col("ax"), col("ay"), col("az")) {
            (Some(x), Some(y), Some(z)) => Some((x, y, z)),
            _ => None,
        };

        let mut samples = Vec::new();
        let mut dropped = 0usize;
        let mut total = 0usize;
        for (idx, line) in lines {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            total += 1;
            if total > limits::MAX_SAMPLES {
                return Err(MotionError::TooManySamples { count: total });
            }
            let fields: Vec<&str> = line.split(',').map(str::trim).collect();
            let get = |i: usize| -> Result<f64, MotionError> {
                fields
                    .get(i)
                    .ok_or(())
                    .and_then(|s| s.parse::<f64>().map_err(|_| ()))
                    .map_err(|_| MotionError::Malformed {
                        dialect: DIALECT,
                        line: idx + 1,
                        detail: format!("column {i} is not a number in {line:?}"),
                    })
            };
            let accel = match accel_cols {
                Some((x, y, z)) => {
                    Some(axes.apply([get(x)? * ascale, get(y)? * ascale, get(z)? * ascale]))
                }
                None => None,
            };
            let sample = MotionSample {
                sensor_time_ns: (get(ct)? * tscale * 1e9).round() as i64,
                gyro_rad_s: axes.apply([
                    get(cgx)? * gscale,
                    get(cgy)? * gscale,
                    get(cgz)? * gscale,
                ]),
                accel_mps2: accel,
                orientation: None,
            };
            if sample.is_finite() {
                samples.push(sample);
            } else {
                dropped += 1;
            }
        }

        MotionSeries {
            samples,
            format: MotionFormat::Gcsv,
            id,
            // `.gcsv` states readout time in seconds already.
            frame_readout_time_s: readout,
            lens_hint,
            dropped_invalid: dropped,
        }
        .finish(total)
    }
}

// ── embedded-container dialects ─────────────────────────────────────────────

/// Container-embedded telemetry (e.g. a camera's private MP4 track).
///
/// **Extraction is not implemented.** 23 §9.1 requires a dependency audit of any
/// binary telemetry parser — and of its git-sourced transitive dependencies —
/// *before* the dependency is added, and 23 §14's D-12 disposition leaves that
/// box unchecked.
///
/// What this adapter does instead is *diagnose*, and that turns out to matter
/// more than it sounds. The overwhelmingly common reason a clip cannot be
/// gyro-stabilized is not a parser gap; it is that the file never carried IMU
/// data in the first place — either because a consumer drone logged only a
/// ~1 Hz flight record, or because the copy in hand is a re-encode that dropped
/// the camera's telemetry. Both are recoverable situations, and both are
/// indistinguishable from "unsupported format" unless someone looks. So this
/// adapter looks, using the `ffprobe` the media pipeline already depends on,
/// and says which case it is.
pub struct EmbeddedContainerAdapter;

/// Below this rate a telemetry track is a flight log, not an IMU stream.
///
/// Real gyro runs at hundreds of Hz — 200 Hz is a low-end action cam, 8 kHz a
/// flight controller. Consumer drones log position/exposure at 1 Hz. Anything
/// under 50 Hz cannot resolve the motion stabilization exists to cancel, so
/// treating it as gyro would produce confident nonsense rather than a failure.
const MIN_GYRO_RATE_HZ: f64 = 50.0;

impl EmbeddedContainerAdapter {
    /// Ask `ffprobe` what non-media tracks this container carries.
    ///
    /// Returns `None` when ffprobe is unavailable or fails — in which case the
    /// caller falls back to the plain "no motion track" answer rather than
    /// inventing a diagnosis it cannot support.
    fn inspect(source: &Path) -> Option<ContainerTelemetry> {
        let tools = super::ffmpeg_locate::locate().ok()?;
        let out = std::process::Command::new(&tools.ffprobe)
            .args([
                "-v",
                "error",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(source)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;

        let duration_s = v
            .get("format")
            .and_then(|f| f.get("duration"))
            .and_then(|d| d.as_str())
            .and_then(|d| d.parse::<f64>().ok())
            .unwrap_or(0.0);

        // `encoder` is written by libavformat and friends; a camera original
        // has no such tag. This is the reliable "someone transcoded this" tell.
        let writer = v
            .get("format")
            .and_then(|f| f.get("tags"))
            .and_then(|t| t.get("encoder"))
            .and_then(|e| e.as_str())
            .map(str::to_string);

        // Widest sample count across any data/subtitle track: those are where
        // cameras park telemetry, and we want the fastest one.
        let mut samples = 0u64;
        if let Some(streams) = v.get("streams").and_then(|s| s.as_array()) {
            for s in streams {
                let kind = s.get("codec_type").and_then(|c| c.as_str()).unwrap_or("");
                if kind != "data" && kind != "subtitle" {
                    continue;
                }
                let n = s
                    .get("nb_frames")
                    .and_then(|n| n.as_str())
                    .and_then(|n| n.parse::<u64>().ok())
                    .unwrap_or(0);
                samples = samples.max(n);
            }
        }
        Some(ContainerTelemetry {
            duration_s,
            samples,
            writer,
        })
    }
}

/// What [`EmbeddedContainerAdapter::inspect`] found.
struct ContainerTelemetry {
    duration_s: f64,
    samples: u64,
    writer: Option<String>,
}

impl MotionMetadataAdapter for EmbeddedContainerAdapter {
    fn name(&self) -> &'static str {
        "embedded container telemetry"
    }

    fn sniff(&self, source: &Path) -> AdapterConfidence {
        let is_media = source
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| {
                ["mp4", "mov", "m4v", "insv", "lrv"]
                    .iter()
                    .any(|k| e.eq_ignore_ascii_case(k))
            });
        if is_media {
            AdapterConfidence::Weak
        } else {
            AdapterConfidence::No
        }
    }

    fn parse(&self, source: &Path) -> Result<MotionSeries, MotionError> {
        let Some(t) = Self::inspect(source) else {
            return Err(MotionError::NoMotionTrack);
        };

        let rate = if t.duration_s > 0.0 {
            t.samples as f64 / t.duration_s
        } else {
            0.0
        };

        // Order matters. A re-encode is reported first even if it happens to
        // retain a subtitle track, because "your copy is derived" is the
        // actionable fact — the original may still have everything.
        if let Some(writer) = t.writer {
            if rate < MIN_GYRO_RATE_HZ {
                return Err(MotionError::ReencodedCopy { writer });
            }
        }
        if t.samples > 0 && rate < MIN_GYRO_RATE_HZ {
            return Err(MotionError::LowRateTelemetryOnly {
                hz: rate,
                samples: t.samples,
                duration_s: t.duration_s,
            });
        }
        // A high-rate track may well be real IMU data — but extracting it needs
        // the 23 §9.1 parser gate, so say that plainly rather than pretending
        // the file is empty.
        if t.samples > 0 {
            return Err(MotionError::Unimplemented(
                "this clip carries a high-rate telemetry track that may contain gyro data, \
                 but container telemetry extraction is not enabled in this build \
                 (pending the 23 §9.1 parser dependency audit). Export a .gcsv sidecar \
                 and import that instead.",
            ));
        }
        Err(MotionError::NoMotionTrack)
    }
}

// ── dispatch ────────────────────────────────────────────────────────────────

/// Every adapter this build ships, strongest-claim-first at dispatch.
pub fn adapters() -> Vec<Box<dyn MotionMetadataAdapter>> {
    vec![
        Box::new(PhotonicJsonAdapter),
        Box::new(GcsvAdapter),
        Box::new(EmbeddedContainerAdapter),
    ]
}

/// Parse `source` with whichever adapter claims it most strongly.
///
/// Ties and `Weak`-only claims still parse, because a `Weak` claim means "the
/// extension fits but content was not confirmed" — the adapter's own parser is
/// the authority. Nothing here guesses *between* dialects: a file that no
/// adapter claims returns [`MotionError::UnrecognizedDialect`].
pub fn parse_motion(source: &Path) -> Result<MotionSeries, MotionError> {
    let all = adapters();
    let mut best: Option<(&dyn MotionMetadataAdapter, AdapterConfidence)> = None;
    for a in &all {
        let c = a.sniff(source);
        if c == AdapterConfidence::No {
            continue;
        }
        if best.is_none_or(|(_, bc)| c > bc) {
            best = Some((a.as_ref(), c));
        }
    }
    match best {
        Some((adapter, _)) => adapter.parse(source),
        None => Err(MotionError::UnrecognizedDialect),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("photonic-motion-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    // ── axis normalization ──────────────────────────────────────────────

    #[test]
    fn identity_axis_map_is_a_no_op() {
        let m = AxisMap::parse("XYZ").unwrap();
        assert_eq!(m.apply([1.0, 2.0, 3.0]), [1.0, 2.0, 3.0]);
    }

    #[test]
    fn axis_map_permutes_and_negates() {
        // out.x = src.y, out.y = -src.x, out.z = src.z
        let m = AxisMap::parse("YxZ").unwrap();
        assert_eq!(m.apply([1.0, 2.0, 3.0]), [2.0, -1.0, 3.0]);
    }

    #[test]
    fn non_permutation_axis_maps_are_rejected() {
        // A repeated axis would silently collapse a rotational dimension.
        assert!(AxisMap::parse("XXZ").is_err());
        assert!(AxisMap::parse("XY").is_err());
        assert!(AxisMap::parse("ABC").is_err());
    }

    // ── Photonic gyro JSON ──────────────────────────────────────────────

    const JSON_OK: &str = r#"{
        "photonic_gyro": 1,
        "id": "unit-test",
        "orientation": "XYZ",
        "time_units": "ns",
        "gyro_units": "rad/s",
        "samples": [
            { "t": 0,         "gyro": [0.0, 0.0, 0.0] },
            { "t": 1000000,   "gyro": [0.1, 0.0, 0.0] },
            { "t": 2000000,   "gyro": [0.2, 0.0, 0.0] }
        ]
    }"#;

    #[test]
    fn photonic_json_parses_and_normalizes() {
        let p = tmp("ok.json", JSON_OK);
        let s = PhotonicJsonAdapter.parse(&p).unwrap();
        assert_eq!(s.samples.len(), 3);
        assert_eq!(s.format, MotionFormat::PhotonicJson);
        assert_eq!(s.id, "unit-test");
        assert_eq!(s.samples[1].sensor_time_ns, 1_000_000);
        assert!((s.samples[2].gyro_rad_s[0] - 0.2).abs() < 1e-12);
        assert_eq!(s.dropped_invalid, 0);
        assert!(!s.has_accel());
    }

    #[test]
    fn photonic_json_converts_degrees_to_radians() {
        let body = JSON_OK.replace(r#""gyro_units": "rad/s""#, r#""gyro_units": "deg/s""#);
        let p = tmp("deg.json", &body);
        let s = PhotonicJsonAdapter.parse(&p).unwrap();
        // 0.2 deg/s in radians.
        assert!((s.samples[2].gyro_rad_s[0] - 0.2_f64.to_radians()).abs() < 1e-12);
    }

    #[test]
    fn unknown_units_hard_fail_rather_than_defaulting() {
        // 22 §6.6: refusing is the point — a silently assumed unit produces a
        // plausible-but-wrong stabilization.
        let body = JSON_OK.replace(r#""gyro_units": "rad/s""#, r#""gyro_units": "furlongs""#);
        let p = tmp("badunits.json", &body);
        assert!(matches!(
            PhotonicJsonAdapter.parse(&p),
            Err(MotionError::Unsupported {
                field: "gyro_units",
                ..
            })
        ));
    }

    #[test]
    fn accel_without_declared_units_is_refused() {
        let body = r#"{
            "photonic_gyro": 1, "orientation": "XYZ",
            "time_units": "ns", "gyro_units": "rad/s",
            "samples": [{ "t": 0, "gyro": [0,0,0], "accel": [0,9.81,0] }]
        }"#;
        let p = tmp("accel_nounits.json", body);
        assert!(matches!(
            PhotonicJsonAdapter.parse(&p),
            Err(MotionError::Unsupported {
                field: "accel_units",
                ..
            })
        ));
    }

    #[test]
    fn gyro_only_file_need_not_declare_accel_units() {
        let p = tmp("gyro_only.json", JSON_OK);
        assert!(PhotonicJsonAdapter.parse(&p).is_ok());
    }

    #[test]
    fn json_cannot_smuggle_a_non_finite_number() {
        // Worth pinning: JSON has no infinity/NaN literal, and serde_json
        // rejects an out-of-range exponent at the *parse* layer — before
        // `MotionSample::is_finite` is ever consulted. So for this dialect the
        // finiteness guard is defence in depth, not the first line; the
        // reachable non-finite paths are `.gcsv` text (below) and a
        // zero-length quaternion, which JSON *can* express.
        let body = r#"{
            "photonic_gyro": 1, "orientation": "XYZ",
            "time_units": "ns", "gyro_units": "rad/s",
            "samples": [{ "t": 0, "gyro": [1e999, 0, 0] }]
        }"#;
        let p = tmp("overflow.json", body);
        assert!(matches!(
            PhotonicJsonAdapter.parse(&p),
            Err(MotionError::Malformed { .. })
        ));
    }

    #[test]
    fn degenerate_quaternion_is_dropped_and_counted() {
        // `[0,0,0,0]` is valid JSON but normalizes to NaN — the one non-finite
        // shape this dialect *can* carry. One bad sample in 21 is under the
        // tolerance, so the series survives with a reported count.
        let mut rows: Vec<String> = (0..20)
            .map(|t| format!(r#"{{ "t": {t}, "gyro": [0,0,0] }}"#))
            .collect();
        rows.push(r#"{ "t": 20, "gyro": [0,0,0], "orientation": [0,0,0,0] }"#.into());
        let body = format!(
            r#"{{ "photonic_gyro": 1, "orientation": "XYZ",
                  "time_units": "ns", "gyro_units": "rad/s",
                  "samples": [{}] }}"#,
            rows.join(",")
        );
        let p = tmp("zero_quat.json", &body);
        let s = PhotonicJsonAdapter.parse(&p).unwrap();
        assert_eq!(s.dropped_invalid, 1);
        assert_eq!(s.samples.len(), 20, "the finite samples survive");
    }

    #[test]
    fn mostly_invalid_series_is_rejected_outright() {
        // 1 bad of 2 is 50%, far above the 5% tolerance.
        let body = r#"{
            "photonic_gyro": 1, "orientation": "XYZ",
            "time_units": "ns", "gyro_units": "rad/s",
            "samples": [
                { "t": 0, "gyro": [0,0,0] },
                { "t": 1, "gyro": [0,0,0], "orientation": [0,0,0,0] }
            ]
        }"#;
        let p = tmp("mostly_bad.json", body);
        assert!(matches!(
            PhotonicJsonAdapter.parse(&p),
            Err(MotionError::TooManyInvalid { .. })
        ));
    }

    #[test]
    fn gcsv_nan_rows_are_dropped_and_counted() {
        // Unlike JSON, the text format *can* carry `nan`/`inf`: Rust's f64
        // parser accepts both. This is the guard's real exposure.
        let mut body =
            String::from("GYROFLOW IMU LOG\norientation,XYZ\ntscale,1\ngscale,1\nt,gx,gy,gz\n");
        for t in 0..20 {
            body.push_str(&format!("{t},0,0,0\n"));
        }
        body.push_str("20,nan,0,0\n");
        let p = tmp("nan_row.gcsv", &body);
        let s = GcsvAdapter.parse(&p).unwrap();
        assert_eq!(s.dropped_invalid, 1);
        assert_eq!(s.samples.len(), 20);
    }

    #[test]
    fn gcsv_infinity_is_also_caught() {
        let mut body =
            String::from("GYROFLOW IMU LOG\norientation,XYZ\ntscale,1\ngscale,1\nt,gx,gy,gz\n");
        for t in 0..20 {
            body.push_str(&format!("{t},0,0,0\n"));
        }
        body.push_str("20,inf,0,0\n");
        let p = tmp("inf_row.gcsv", &body);
        assert_eq!(GcsvAdapter.parse(&p).unwrap().dropped_invalid, 1);
    }

    #[test]
    fn non_monotonic_timestamps_are_rejected() {
        let body = r#"{
            "photonic_gyro": 1, "orientation": "XYZ",
            "time_units": "ns", "gyro_units": "rad/s",
            "samples": [
                { "t": 0,  "gyro": [0,0,0] },
                { "t": 10, "gyro": [0,0,0] },
                { "t": 5,  "gyro": [0,0,0] }
            ]
        }"#;
        let p = tmp("backwards.json", body);
        assert!(matches!(
            PhotonicJsonAdapter.parse(&p),
            Err(MotionError::NonMonotonic { index: 2 })
        ));
    }

    #[test]
    fn empty_series_is_rejected() {
        let body = r#"{
            "photonic_gyro": 1, "orientation": "XYZ",
            "time_units": "ns", "gyro_units": "rad/s", "samples": []
        }"#;
        let p = tmp("empty.json", body);
        assert!(matches!(
            PhotonicJsonAdapter.parse(&p),
            Err(MotionError::Empty)
        ));
    }

    // ── .gcsv ───────────────────────────────────────────────────────────

    const GCSV_OK: &str = "GYROFLOW IMU LOG\n\
        version,1.3\n\
        id,test-logger\n\
        orientation,XYZ\n\
        tscale,0.001\n\
        gscale,0.0002\n\
        ascale,0.00048828125\n\
        frame_readout_time,0.0125\n\
        t,gx,gy,gz,ax,ay,az\n\
        0,0,0,0,0,2048,0\n\
        1,100,0,0,0,2048,0\n\
        2,200,0,0,0,2048,0\n";

    #[test]
    fn gcsv_parses_and_applies_header_scales() {
        let p = tmp("ok.gcsv", GCSV_OK);
        let s = GcsvAdapter.parse(&p).unwrap();
        assert_eq!(s.samples.len(), 3);
        assert_eq!(s.format, MotionFormat::Gcsv);
        assert_eq!(s.id, "test-logger");
        // tscale 0.001 s per unit -> t=1 is 1 ms is 1_000_000 ns.
        assert_eq!(s.samples[1].sensor_time_ns, 1_000_000);
        // gscale 0.0002 rad/s per count -> 100 counts is 0.02 rad/s.
        assert!((s.samples[1].gyro_rad_s[0] - 0.02).abs() < 1e-12);
        assert_eq!(s.frame_readout_time_s, Some(0.0125));
        assert!(s.has_accel());
    }

    #[test]
    fn gcsv_accepts_the_camera_magic_too() {
        let p = tmp(
            "camera.gcsv",
            &GCSV_OK.replace("GYROFLOW IMU LOG", "CAMERA IMU LOG"),
        );
        assert!(GcsvAdapter.parse(&p).is_ok());
    }

    #[test]
    fn gcsv_without_orientation_hard_fails() {
        let body = GCSV_OK.replace("orientation,XYZ\n", "");
        let p = tmp("noorient.gcsv", &body);
        assert!(
            matches!(
                GcsvAdapter.parse(&p),
                Err(MotionError::Unsupported {
                    field: "orientation",
                    ..
                })
            ),
            "no axis convention means no safe interpretation (22 §6.6)"
        );
    }

    #[test]
    fn gcsv_gyro_only_columns_are_accepted() {
        let body = "GYROFLOW IMU LOG\n\
            orientation,XYZ\n\
            tscale,1\n\
            gscale,1\n\
            t,gx,gy,gz\n\
            0,0,0,0\n\
            1,0.5,0,0\n";
        let p = tmp("gyroonly.gcsv", body);
        let s = GcsvAdapter.parse(&p).unwrap();
        assert!(!s.has_accel());
        assert_eq!(s.samples.len(), 2);
    }

    #[test]
    fn gcsv_rejects_a_wrong_magic() {
        let p = tmp("bogus.gcsv", "NOT AN IMU LOG\nt,gx,gy,gz\n0,0,0,0\n");
        assert!(matches!(
            GcsvAdapter.parse(&p),
            Err(MotionError::UnrecognizedDialect)
        ));
    }

    #[test]
    fn gcsv_reports_the_offending_line_on_malformed_data() {
        let body = "GYROFLOW IMU LOG\n\
            orientation,XYZ\n\
            tscale,1\ngscale,1\n\
            t,gx,gy,gz\n\
            0,0,0,0\n\
            1,notanumber,0,0\n";
        let p = tmp("badrow.gcsv", body);
        match GcsvAdapter.parse(&p) {
            Err(MotionError::Malformed { line, .. }) => assert_eq!(line, 7),
            other => panic!("expected a located Malformed error, got {other:?}"),
        }
    }

    // ── dispatch + sniffing ─────────────────────────────────────────────

    #[test]
    fn dispatch_picks_the_strongest_claim() {
        let j = tmp("dispatch.json", JSON_OK);
        assert_eq!(parse_motion(&j).unwrap().format, MotionFormat::PhotonicJson);
        let g = tmp("dispatch.gcsv", GCSV_OK);
        assert_eq!(parse_motion(&g).unwrap().format, MotionFormat::Gcsv);
    }

    #[test]
    fn unclaimed_file_is_not_guessed_at() {
        let p = tmp("mystery.bin", "\u{0}\u{1}\u{2}");
        assert!(matches!(
            parse_motion(&p),
            Err(MotionError::UnrecognizedDialect)
        ));
    }

    #[test]
    fn media_file_without_telemetry_reports_no_motion_track() {
        // The common real case: a re-encoded clip whose private metadata box
        // was stripped. The user needs "no gyro data here", not "corrupt file".
        let p = tmp("reencoded.mp4", "\u{0}fake");
        assert!(matches!(parse_motion(&p), Err(MotionError::NoMotionTrack)));
    }

    #[test]
    fn sample_rate_is_measured_from_the_span() {
        let p = tmp("rate.gcsv", GCSV_OK);
        let s = GcsvAdapter.parse(&p).unwrap();
        // 3 samples 1 ms apart -> 2 intervals over 2 ms -> 1000 Hz.
        assert!((s.sample_rate_hz().unwrap() - 1000.0).abs() < 1e-6);
    }
}
