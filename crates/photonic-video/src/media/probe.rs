//! `ffprobe` → [`MediaProbe`] (01 §3, 02 §3).
//!
//! Runs `ffprobe -print_format json -show_format -show_streams` and folds the
//! result into the persisted [`MediaProbe`] subset plus a few decode-only
//! extras ([`ProbeDetails`]) the persisted struct deliberately omits (pixel
//! format / alpha presence / VFR suspicion). Probing shells out and blocks; the
//! engine runs it on a worker thread — nothing here spawns threads itself.
//!
//! The content-hash helper ([`content_hash`]) is the relink identity of 01 §3:
//! xxh3 of the file's head + tail + length, so a moved/renamed file still
//! matches without hashing gigabytes.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::Command;

use photonic_core::timeline::{
    AudioStreamInfo, FrameRate, MediaProbe, ProbedColor, ScanType, Tick, VideoStreamInfo,
    TICKS_PER_SECOND,
};
use serde::Deserialize;

use super::ffmpeg_locate::FfmpegTools;

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("failed to spawn ffprobe: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("ffprobe exited with {status}: {stderr}")]
    Exit { status: String, stderr: String },
    #[error("ffprobe output was not valid JSON: {0}")]
    Json(#[source] serde_json::Error),
    #[error("ffprobe reported no usable streams for this file")]
    NoStreams,
}

/// Full probe result: the persisted [`MediaProbe`] plus decode-only extras the
/// persisted struct omits by design (they are cheap to re-derive and would only
/// bloat the document JSON).
#[derive(Clone, Debug, PartialEq)]
pub struct ProbeDetails {
    pub probe: MediaProbe,
    /// Source pixel format string as ffprobe reports it (e.g. `yuv420p`,
    /// `argb`) — decode uses it to pick the output plane layout.
    pub pixel_format: Option<String>,
    /// Whether the source carries an alpha channel (decode picks `yuva444p`
    /// output only when this is true — 02 §3).
    pub has_alpha: bool,
    /// `avg_frame_rate` as reported by the container; differs from the probe's
    /// `frame_rate` (`r_frame_rate`) when the source is variable-frame-rate.
    pub avg_frame_rate: Option<FrameRate>,
    /// Heuristic: the container's average and base rates disagree beyond
    /// rounding ⇒ treat as VFR (decode then uses the pts-table path — 02 §4).
    pub is_vfr: bool,
    /// Progressive / interlaced / unknown (K-G6 / 32 §6). Mirrors
    /// `probe.video.scan` when video is present.
    pub scan: ScanType,
}

/// Probe `path`, returning the persisted [`MediaProbe`] only.
pub fn probe_asset(tools: &FfmpegTools, path: &Path) -> Result<MediaProbe, ProbeError> {
    Ok(probe_details(tools, path)?.probe)
}

/// Probe `path`, returning the persisted probe plus decode-only extras.
pub fn probe_details(tools: &FfmpegTools, path: &Path) -> Result<ProbeDetails, ProbeError> {
    let output = Command::new(&tools.ffprobe)
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
        .map_err(ProbeError::Spawn)?;

    if !output.status.success() {
        return Err(ProbeError::Exit {
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    let parsed: FfProbe = serde_json::from_slice(&output.stdout).map_err(ProbeError::Json)?;
    fold(parsed)
}

/// True if `pix_fmt` names a format with an alpha channel.
pub fn pixel_format_has_alpha(pix_fmt: &str) -> bool {
    // ffmpeg alpha-bearing formats: yuva*, rgba/argb/abgr/bgra, ya8/ya16,
    // gbrap, pal8 (paletted, may carry alpha). A trailing/embedded `a` in the
    // component list is the reliable signal for the pixel formats we decode.
    let f = pix_fmt.to_ascii_lowercase();
    f.starts_with("yuva")
        || f.starts_with("ya")
        || f.contains("rgba")
        || f.contains("argb")
        || f.contains("abgr")
        || f.contains("bgra")
        || f.starts_with("gbrap")
        || f.starts_with("rgba")
        || f.starts_with("argb")
}

// ── ffprobe JSON shape (only the fields we consume) ─────────────────────────

#[derive(Deserialize)]
struct FfProbe {
    #[serde(default)]
    streams: Vec<FfStream>,
    format: Option<FfFormat>,
}

#[derive(Deserialize)]
struct FfStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    // video
    width: Option<u32>,
    height: Option<u32>,
    pix_fmt: Option<String>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    sample_aspect_ratio: Option<String>,
    color_primaries: Option<String>,
    color_transfer: Option<String>,
    color_space: Option<String>,
    color_range: Option<String>,
    /// ffprobe field_order: progressive / tt / bb / tb / bt / …
    field_order: Option<String>,
    // audio
    sample_rate: Option<String>,
    channels: Option<u16>,
    // duration fallback (stream-level)
    duration: Option<String>,
}

#[derive(Deserialize)]
struct FfFormat {
    format_name: Option<String>,
    duration: Option<String>,
}

fn fold(p: FfProbe) -> Result<ProbeDetails, ProbeError> {
    let format = p.format;
    let container = format
        .as_ref()
        .and_then(|f| f.format_name.clone())
        .unwrap_or_default();

    let video = p
        .streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("video"));
    let audio = p
        .streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("audio"));

    if video.is_none() && audio.is_none() {
        return Err(ProbeError::NoStreams);
    }

    // Duration: prefer the format-level value, fall back to a stream's.
    let duration_secs = format
        .as_ref()
        .and_then(|f| f.duration.as_deref())
        .and_then(parse_secs)
        .or_else(|| {
            video
                .and_then(|s| s.duration.as_deref())
                .and_then(parse_secs)
        })
        .or_else(|| {
            audio
                .and_then(|s| s.duration.as_deref())
                .and_then(parse_secs)
        })
        .unwrap_or(0.0);
    let duration = secs_to_tick(duration_secs);

    let mut pixel_format = None;
    let mut has_alpha = false;
    let mut avg_frame_rate = None;
    let mut is_vfr = false;
    let mut scan = ScanType::Unknown;

    let video_info = video.map(|s| {
        let frame_rate = s
            .r_frame_rate
            .as_deref()
            .and_then(parse_rational_rate)
            .unwrap_or(FrameRate::FPS_30);
        avg_frame_rate = s.avg_frame_rate.as_deref().and_then(parse_rational_rate);
        is_vfr = match avg_frame_rate {
            Some(avg) => rates_differ(frame_rate, avg),
            None => false,
        };
        pixel_format = s.pix_fmt.clone();
        has_alpha = s
            .pix_fmt
            .as_deref()
            .map(pixel_format_has_alpha)
            .unwrap_or(false);
        scan = parse_field_order(s.field_order.as_deref());

        VideoStreamInfo {
            width: s.width.unwrap_or(0),
            height: s.height.unwrap_or(0),
            frame_rate,
            pixel_aspect: s
                .sample_aspect_ratio
                .as_deref()
                .and_then(parse_pixel_aspect)
                .unwrap_or(1.0),
            color: ProbedColor {
                primaries: non_unknown(&s.color_primaries),
                transfer: non_unknown(&s.color_transfer),
                matrix: non_unknown(&s.color_space),
                full_range: s.color_range.as_deref().and_then(parse_color_range),
            },
            keyframe_index_cached: false,
            scan,
        }
    });

    let audio_info = audio.map(|s| AudioStreamInfo {
        sample_rate: s
            .sample_rate
            .as_deref()
            .and_then(|r| r.parse::<u32>().ok())
            .unwrap_or(0),
        channels: s.channels.unwrap_or(0),
        codec: s.codec_name.clone().unwrap_or_default(),
    });

    // Container-level codec: the video codec if present, else audio.
    let codec = video
        .and_then(|s| s.codec_name.clone())
        .or_else(|| audio.and_then(|s| s.codec_name.clone()))
        .unwrap_or_default();

    // Audio-only media: scan is N/A → Progressive (no field path to mishandle).
    if video_info.is_none() {
        scan = ScanType::Progressive;
    }

    Ok(ProbeDetails {
        probe: MediaProbe {
            duration,
            video: video_info,
            audio: audio_info,
            container,
            codec,
            // K-C7: persist triage fields so the pool can report without re-probe.
            is_vfr,
            pixel_format: pixel_format.clone(),
            has_alpha,
        },
        pixel_format,
        has_alpha,
        avg_frame_rate,
        is_vfr,
        scan,
    })
}

/// Map ffprobe `field_order` into [`ScanType`] (K-G6).
///
/// | ffprobe | ScanType |
/// |---|---|
/// | progressive / unknown / missing | Progressive / Unknown |
/// | tt / tb | InterlacedTopFirst |
/// | bb / bt | InterlacedBottomFirst |
pub fn parse_field_order(raw: Option<&str>) -> ScanType {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => ScanType::Unknown,
        Some(s) if s.eq_ignore_ascii_case("progressive") => ScanType::Progressive,
        Some(s) if s.eq_ignore_ascii_case("tt") || s.eq_ignore_ascii_case("tb") => {
            ScanType::InterlacedTopFirst
        }
        Some(s) if s.eq_ignore_ascii_case("bb") || s.eq_ignore_ascii_case("bt") => {
            ScanType::InterlacedBottomFirst
        }
        Some(_) => ScanType::Unknown,
    }
}

/// Human-readable consequence string for triage / diagnostics (K-G6 / K-C7).
pub fn interlaced_consequence(scan: ScanType) -> Option<&'static str> {
    match scan {
        ScanType::InterlacedTopFirst | ScanType::InterlacedBottomFirst => Some(
            "Interlaced fields will be treated as progressive frames until a \
             deinterlace source-range node is applied — expect combing on motion. \
             (K-G6: detection only; deinterlace is a separate engine node.)",
        ),
        _ => None,
    }
}

/// ffprobe uses `"N/A"` / `"unknown"` for missing color tags; drop those.
fn non_unknown(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("unknown") && *s != "N/A")
        .map(str::to_string)
}

fn parse_secs(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() || s == "N/A" {
        return None;
    }
    s.parse::<f64>().ok()
}

/// Round seconds to the nearest tick (05 §7's determinism note: fixed rounding).
fn secs_to_tick(secs: f64) -> Tick {
    Tick((secs * TICKS_PER_SECOND as f64).round() as i64)
}

/// Parse an ffprobe rational rate like `"30/1"` or `"30000/1001"`.
pub fn parse_rational_rate(s: &str) -> Option<FrameRate> {
    let (num, den) = s.split_once('/')?;
    let num: u32 = num.trim().parse().ok()?;
    let den: u32 = den.trim().parse().ok()?;
    if num == 0 || den == 0 {
        return None;
    }
    Some(FrameRate { num, den })
}

/// Parse `sample_aspect_ratio` (`"1:1"`, `"40:33"`) into a float pixel aspect.
fn parse_pixel_aspect(s: &str) -> Option<f32> {
    let (num, den) = s.split_once(':')?;
    let num: f32 = num.trim().parse().ok()?;
    let den: f32 = den.trim().parse().ok()?;
    if den == 0.0 {
        return None;
    }
    Some(num / den)
}

/// `color_range`: `"pc"`/`"full"` = full range, `"tv"`/`"limited"` = limited.
fn parse_color_range(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "pc" | "full" | "jpeg" => Some(true),
        "tv" | "limited" | "mpeg" => Some(false),
        _ => None,
    }
}

/// Two rates disagree beyond a small rounding tolerance (used only to flag VFR;
/// exact CFR sources have `r_frame_rate == avg_frame_rate`).
fn rates_differ(a: FrameRate, b: FrameRate) -> bool {
    let fa = a.num as f64 / a.den as f64;
    let fb = b.num as f64 / b.den as f64;
    (fa - fb).abs() > 0.01 * fa.max(fb).max(1.0)
}

// ── Content hash (relink identity, 01 §3) ───────────────────────────────────

/// Bytes sampled from each of the file's head and tail for the content hash.
const HASH_CHUNK: usize = 64 * 1024;

/// xxh3-64 of the file's head + tail + length, rendered as a lowercase hex
/// string for `MediaAsset::content_hash` (01 §3). Cheap on huge media (never
/// reads the middle) and stable across platforms/renames — the relink identity.
pub fn content_hash(path: &Path) -> std::io::Result<String> {
    use xxhash_rust::xxh3::Xxh3;

    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();

    let mut hasher = Xxh3::new();
    hasher.update(&len.to_le_bytes());

    if len as usize <= 2 * HASH_CHUNK {
        // Small file: hash the whole thing.
        let mut buf = Vec::with_capacity(len as usize);
        file.read_to_end(&mut buf)?;
        hasher.update(&buf);
    } else {
        let mut head = vec![0u8; HASH_CHUNK];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut head)?;
        hasher.update(&head);

        let mut tail = vec![0u8; HASH_CHUNK];
        file.seek(SeekFrom::End(-(HASH_CHUNK as i64)))?;
        file.read_exact(&mut tail)?;
        hasher.update(&tail);
    }

    Ok(format!("{:016x}", hasher.digest()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rational_rates() {
        assert_eq!(
            parse_rational_rate("30/1"),
            Some(FrameRate { num: 30, den: 1 })
        );
        assert_eq!(
            parse_rational_rate("30000/1001"),
            Some(FrameRate {
                num: 30000,
                den: 1001
            })
        );
        assert_eq!(parse_rational_rate("0/0"), None);
        assert_eq!(parse_rational_rate("garbage"), None);
    }

    #[test]
    fn alpha_detection() {
        assert!(pixel_format_has_alpha("yuva444p"));
        assert!(pixel_format_has_alpha("argb"));
        assert!(pixel_format_has_alpha("rgba"));
        assert!(pixel_format_has_alpha("bgra"));
        assert!(!pixel_format_has_alpha("yuv420p"));
        assert!(!pixel_format_has_alpha("rgb24"));
    }

    #[test]
    fn color_range_parsing() {
        assert_eq!(parse_color_range("pc"), Some(true));
        assert_eq!(parse_color_range("tv"), Some(false));
        assert_eq!(parse_color_range("unknown"), None);
    }

    #[test]
    fn field_order_parsing() {
        assert_eq!(
            parse_field_order(Some("progressive")),
            ScanType::Progressive
        );
        assert_eq!(parse_field_order(Some("tt")), ScanType::InterlacedTopFirst);
        assert_eq!(parse_field_order(Some("tb")), ScanType::InterlacedTopFirst);
        assert_eq!(
            parse_field_order(Some("bb")),
            ScanType::InterlacedBottomFirst
        );
        assert_eq!(
            parse_field_order(Some("bt")),
            ScanType::InterlacedBottomFirst
        );
        assert_eq!(parse_field_order(None), ScanType::Unknown);
        assert_eq!(parse_field_order(Some("weird")), ScanType::Unknown);
        assert!(parse_field_order(Some("tt")).is_interlaced());
        assert!(!parse_field_order(Some("progressive")).is_interlaced());
        assert!(interlaced_consequence(ScanType::InterlacedTopFirst).is_some());
        assert!(interlaced_consequence(ScanType::Progressive).is_none());
    }

    #[test]
    fn rates_differ_flags_vfr() {
        // 24fps base vs 27fps average (the vfr_sample shape) ⇒ VFR.
        assert!(rates_differ(
            FrameRate { num: 24, den: 1 },
            FrameRate { num: 27, den: 1 }
        ));
        // Identical ⇒ not VFR.
        assert!(!rates_differ(FrameRate::FPS_30, FrameRate::FPS_30));
        // NTSC 29.97 vs 30 is within the rounding tolerance ⇒ not VFR.
        assert!(!rates_differ(FrameRate::FPS_29_97, FrameRate::FPS_30));
    }

    #[test]
    fn content_hash_is_stable_and_length_sensitive() {
        let dir = std::env::temp_dir();
        let p1 = dir.join("photonic_probe_hash_a.bin");
        let p2 = dir.join("photonic_probe_hash_b.bin");
        std::fs::write(&p1, b"hello world").unwrap();
        std::fs::write(&p2, b"hello world!").unwrap();

        let h1 = content_hash(&p1).unwrap();
        let h1_again = content_hash(&p1).unwrap();
        let h2 = content_hash(&p2).unwrap();

        assert_eq!(h1, h1_again, "same file hashes identically");
        assert_ne!(h1, h2, "one extra byte changes the hash");
        assert_eq!(h1.len(), 16, "xxh3-64 renders as 16 hex chars");

        let _ = std::fs::remove_file(&p1);
        let _ = std::fs::remove_file(&p2);
    }
}
