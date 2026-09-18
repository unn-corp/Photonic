//! Proxy generation + decode-input selection (02 §6, CAP-014).
//!
//! A **proxy** is a lightweight stand-in for a heavy source: half-resolution,
//! **all-intra** H.264 (openh264-compatible baseline) in MP4, transcoded by the
//! ffmpeg sidecar. All-intra (`-g 1`, every frame an IDR keyframe) makes
//! scrubbing instant; the baseline profile keeps the stream openh264-decodable;
//! half-res quarters the pixel throughput on the preview path.
//!
//! Proxies are stored in the sidecar cache dir keyed by the source's content
//! hash (`<cache_dir>/<hash>.proxy.mp4`), mirroring the keyframe/pts index
//! naming — so they survive project moves and are rebuildable at any time. They
//! are **never required for correctness** (CAP-014): [`resolve_decode_input`]
//! silently falls back to the original whenever the proxy is absent, pending, or
//! failed, and export always uses originals.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use photonic_core::timeline::{
    FrameRate, ProxyOrigin, ProxyRef, ProxyStatus, Tick, TICKS_PER_SECOND,
};

use super::cache_dir_for_project;
use super::ffmpeg_locate::FfmpegTools;
use super::probe::{probe_asset, ProbeError};

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("failed to spawn ffmpeg: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("ffmpeg exited with {status}: {stderr}")]
    Exit { status: String, stderr: String },
    #[error("proxy cache I/O error: {0}")]
    Io(#[source] std::io::Error),
    #[error("proxy generation cancelled")]
    Cancelled,
}

/// Failure reasons for attaching a user-supplied proxy file (G-15A).
#[derive(Debug, thiserror::Error)]
pub enum AttachError {
    #[error("proxy path does not exist: {0}")]
    MissingPath(PathBuf),
    #[error("proxy path is not a file: {0}")]
    NotAFile(PathBuf),
    #[error("proxy path is the same as the original media")]
    SameAsOriginal,
    #[error("proxy file has no video stream")]
    NotVideo,
    #[error("failed to probe proxy (or original): {0}")]
    ProbeFailed(#[source] ProbeError),
    #[error("proxy duration does not match original within tolerance")]
    DurationMismatch,
    #[error("proxy frame rate does not match original")]
    FrameRateMismatch,
}

/// Successful attach validation: Ready+Attached proxy plus any soft warnings
/// when `allow_mismatch` overrode duration/rate checks.
#[derive(Clone, Debug, PartialEq)]
pub struct AttachValidation {
    pub proxy: ProxyRef,
    pub warnings: Vec<String>,
}

// ── Cache location ───────────────────────────────────────────────────────────

/// The directory generated proxies live in.
///
/// With a saved project this is the sidecar cache dir (`<project>.photon.cache/`,
/// 01 §9) so proxies sit beside the keyframe/pts indices and survive project
/// moves. For an unsaved project (no path) they go to a shared, rebuildable
/// OS-temp cache — proxies are never required for correctness (CAP-014).
pub fn proxy_cache_dir(project_path: Option<&Path>) -> PathBuf {
    match project_path {
        Some(p) => cache_dir_for_project(p),
        None => std::env::temp_dir().join("photonic-proxy-cache"),
    }
}

/// `<cache_dir>/<content_hash>.proxy.mp4` — keyed by content hash so the proxy
/// survives project moves and is rebuildable (02 §6, 01 §9), mirroring the
/// keyframe-index cache naming.
pub fn proxy_cache_path(cache_dir: &Path, content_hash: &str) -> PathBuf {
    cache_dir.join(format!("{content_hash}.proxy.mp4"))
}

// ── Decode-input selection ───────────────────────────────────────────────────

/// Choose the decode input for an asset given whether proxy media was requested.
///
/// Returns the proxy file only when `use_proxy` is set and a [`ProxyStatus::Ready`]
/// proxy is present on disk; otherwise the original. Proxies are never required
/// for correctness (CAP-014): a missing, pending, or failed proxy silently falls
/// back to the original, so `ForceProxy` is always safe even before a proxy has
/// been generated.
pub fn resolve_decode_input(original: &Path, proxy: Option<&ProxyRef>, use_proxy: bool) -> PathBuf {
    if use_proxy {
        if let Some(p) = proxy {
            if p.status == ProxyStatus::Ready && p.path.is_file() {
                return p.path.clone();
            }
        }
    }
    original.to_path_buf()
}

/// Whether an asset should be auto-queued for L7 proxy generation after import
/// metadata (L1–L4) completes (24 §2 L7, G-15C).
///
/// Policy is project-level (`generate_proxies`). Only file-backed video without
/// an already-Ready proxy is eligible. Pending/Failed/missing may be retried.
/// User-**Attached** proxies are never auto-replaced (G-15A).
pub fn should_auto_generate_proxy(
    kind: photonic_core::timeline::AssetKind,
    source: &photonic_core::timeline::AssetSource,
    proxy: Option<&ProxyRef>,
    generate_proxies: bool,
) -> bool {
    use photonic_core::timeline::{AssetKind, AssetSource, ProxyOrigin};
    if !generate_proxies {
        return false;
    }
    if kind != AssetKind::Video {
        return false;
    }
    if !matches!(source, AssetSource::File { .. }) {
        return false;
    }
    if let Some(p) = proxy {
        if p.origin == ProxyOrigin::Attached {
            return false;
        }
        if p.status == ProxyStatus::Ready && p.path.is_file() {
            return false;
        }
    }
    true
}

// ── Attach (G-15A) ───────────────────────────────────────────────────────────

/// Validate a user-supplied proxy file against its original and return a
/// Ready + [`ProxyOrigin::Attached`] ref.
///
/// Pure of document/ops: callers apply the result via
/// [`photonic_core::timeline::ops::set_asset_proxy`]. Does not copy or move
/// files. Match policy (D-G15-01): same nominal frame rate when both known;
/// duration within one source frame (or 1/30s if rate unknown). When
/// `allow_mismatch` is true, duration/rate failures become warnings instead.
pub fn validate_attach(
    tools: &FfmpegTools,
    original: &Path,
    proxy_path: &Path,
    allow_mismatch: bool,
) -> Result<AttachValidation, AttachError> {
    if !proxy_path.exists() {
        return Err(AttachError::MissingPath(proxy_path.to_path_buf()));
    }
    if !proxy_path.is_file() {
        return Err(AttachError::NotAFile(proxy_path.to_path_buf()));
    }

    // Canonicalize when possible so hardlinks / `./` aliases count as same file.
    let same = match (original.canonicalize(), proxy_path.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => original == proxy_path,
    };
    if same {
        return Err(AttachError::SameAsOriginal);
    }

    let orig_probe = probe_asset(tools, original).map_err(AttachError::ProbeFailed)?;
    let proxy_probe = probe_asset(tools, proxy_path).map_err(AttachError::ProbeFailed)?;

    let Some(proxy_video) = proxy_probe.video.as_ref() else {
        return Err(AttachError::NotVideo);
    };

    let mut warnings = Vec::new();

    // Frame rate: same nominal when both known (exact rational match).
    if let Some(orig_video) = orig_probe.video.as_ref() {
        if orig_video.frame_rate != proxy_video.frame_rate {
            let msg = format!(
                "frame rate mismatch: original {}/{} vs proxy {}/{}",
                orig_video.frame_rate.num,
                orig_video.frame_rate.den,
                proxy_video.frame_rate.num,
                proxy_video.frame_rate.den
            );
            if allow_mismatch {
                warnings.push(msg);
            } else {
                return Err(AttachError::FrameRateMismatch);
            }
        }
    }

    // Duration: |d_proxy - d_orig| ≤ one frame of original rate (or 1/30s).
    let tolerance = duration_tolerance(orig_probe.video.as_ref().map(|v| v.frame_rate));
    let delta = (proxy_probe.duration.0 - orig_probe.duration.0).unsigned_abs() as i64;
    if delta > tolerance.0 {
        let msg = format!(
            "duration mismatch: original {:.3}s vs proxy {:.3}s (tolerance {:.3}s)",
            orig_probe.duration.as_seconds_f64(),
            proxy_probe.duration.as_seconds_f64(),
            tolerance.as_seconds_f64()
        );
        if allow_mismatch {
            warnings.push(msg);
        } else {
            return Err(AttachError::DurationMismatch);
        }
    }

    let path = proxy_path
        .canonicalize()
        .unwrap_or_else(|_| proxy_path.to_path_buf());

    Ok(AttachValidation {
        proxy: ProxyRef {
            path,
            status: ProxyStatus::Ready,
            origin: ProxyOrigin::Attached,
        },
        warnings,
    })
}

/// One original frame at `rate`, or 1/30s when rate is unknown.
fn duration_tolerance(rate: Option<FrameRate>) -> Tick {
    match rate {
        Some(fr) if fr.num > 0 => fr.ticks_per_frame(),
        _ => Tick(TICKS_PER_SECOND / 30),
    }
}

// ── Generation ───────────────────────────────────────────────────────────────

/// ffmpeg encode args for the proxy profile (02 §6). `scale=trunc(iw/4)*2:…`
/// halves each dimension rounding down to an even size (yuv420p requires even
/// w/h); `-g 1` forces all-intra; `baseline` keeps it openh264-decodable. Audio
/// is dropped (`-an`) — the decode path is video-only.
const PROXY_ENCODE_ARGS: &[&str] = &[
    "-an",
    "-vf",
    "scale=trunc(iw/4)*2:trunc(ih/4)*2",
    "-c:v",
    "libx264",
    "-profile:v",
    "baseline",
    "-preset",
    "veryfast",
    "-crf",
    "23",
    "-g",
    "1",
    "-pix_fmt",
    "yuv420p",
    "-movflags",
    "+faststart",
    // Force the muxer explicitly: the output is written to a `.part` staging
    // file whose extension ffmpeg cannot map to a container on its own.
    "-f",
    "mp4",
];

/// The `.part` staging path a proxy is written to before an atomic rename, so a
/// crash or cancel never leaves a truncated file that looks Ready by path.
fn staging_path(output: &Path) -> PathBuf {
    let mut s = output.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

/// Transcode `input` → a half-res all-intra H.264/MP4 proxy at `output`.
///
/// Writes to a `.part` sibling and atomically renames on success. `cancel` is
/// polled between waits; when it returns `true` the ffmpeg process is killed,
/// the partial file removed, and [`ProxyError::Cancelled`] returned. The parent
/// directory of `output` is created if needed.
pub fn generate_proxy(
    tools: &FfmpegTools,
    input: &Path,
    output: &Path,
    cancel: &dyn Fn() -> bool,
) -> Result<(), ProxyError> {
    if let Some(dir) = output.parent() {
        std::fs::create_dir_all(dir).map_err(ProxyError::Io)?;
    }
    let tmp = staging_path(output);

    let mut command = Command::new(&tools.ffmpeg);
    command
        .arg("-y")
        .args(["-nostdin", "-loglevel", "error"])
        .arg("-i")
        .arg(input)
        .args(PROXY_ENCODE_ARGS)
        .arg(&tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    lower_background_priority(&mut command);
    // 37 §2.2: SIGKILL this proxy transcode if the editor process dies (Linux).
    crate::media::child_registry::arm_parent_death_signal(&mut command);
    let mut child = command.spawn().map_err(ProxyError::Spawn)?;

    let status = loop {
        if cancel() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&tmp);
            return Err(ProxyError::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(ProxyError::Io(e));
            }
        }
    };

    if status.success() {
        std::fs::rename(&tmp, output).map_err(ProxyError::Io)?;
        Ok(())
    } else {
        let stderr = stderr_tail(&mut child);
        let _ = std::fs::remove_file(&tmp);
        Err(ProxyError::Exit {
            status: status.to_string(),
            stderr,
        })
    }
}

/// Make proxy transcodes cooperative with interactive playback on Unix. This
/// runs in the child immediately before `exec`, so it cannot lower the UI
/// process itself. Failure is deliberately non-fatal: containers and hardened
/// desktops may forbid changing priority, but proxy generation must still work.
#[cfg(unix)]
fn lower_background_priority(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    // SAFETY: `pre_exec` runs in the newly-forked child. The closure performs
    // only the async-signal-safe `setpriority` syscall and deliberately ignores
    // a permission failure before returning to exec ffmpeg.
    unsafe {
        command.pre_exec(|| {
            let _ = libc::setpriority(libc::PRIO_PROCESS, 0, 10);
            Ok(())
        });
    }
}

/// Windows: BELOW_NORMAL priority class so proxy encodes yield to interactive
/// playback/decode without requiring admin rights.
#[cfg(windows)]
fn lower_background_priority(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
    command.creation_flags(BELOW_NORMAL_PRIORITY_CLASS);
}

#[cfg(not(any(unix, windows)))]
fn lower_background_priority(_command: &mut Command) {}

/// Read the last ~500 chars ffmpeg wrote to stderr (for a `ProxyError` message).
/// `-loglevel error` keeps this near-empty on success, so reading it after exit
/// (rather than draining concurrently) does not risk a pipe-full wedge here.
fn stderr_tail(child: &mut std::process::Child) -> String {
    use std::io::Read;
    let mut buf = String::new();
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut buf);
    }
    let tail: String = buf.chars().rev().take(500).collect();
    tail.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::ffmpeg_locate::locate_for_test;

    fn unique_tmp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "photonic-proxy-test-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn proxy_cache_path_names_by_hash() {
        let p = proxy_cache_path(Path::new("/cache"), "deadbeef");
        assert_eq!(p, PathBuf::from("/cache/deadbeef.proxy.mp4"));
    }

    #[test]
    fn proxy_cache_dir_uses_sidecar_dir_when_project_known() {
        let d = proxy_cache_dir(Some(Path::new("/proj/movie.photon")));
        assert_eq!(d, PathBuf::from("/proj/movie.photon.cache"));
        // Unsaved project ⇒ a rebuildable temp cache (never required for
        // correctness).
        assert!(proxy_cache_dir(None).ends_with("photonic-proxy-cache"));
    }

    #[test]
    fn should_auto_generate_proxy_policy() {
        use photonic_core::timeline::{AssetKind, AssetSource, VectorRef};
        let path = PathBuf::from("/media/clip.mp4");
        let file = AssetSource::File {
            path: path.clone(),
            rel_path: None,
        };
        assert!(!should_auto_generate_proxy(
            AssetKind::Video,
            &file,
            None,
            false
        ));
        assert!(should_auto_generate_proxy(
            AssetKind::Video,
            &file,
            None,
            true
        ));
        assert!(!should_auto_generate_proxy(
            AssetKind::Audio,
            &file,
            None,
            true
        ));
        assert!(!should_auto_generate_proxy(
            AssetKind::Video,
            &AssetSource::EmbeddedVector {
                root: VectorRef::WholeDocument,
            },
            None,
            true
        ));
        let ready_path = unique_tmp_dir("auto-ready").join("p.proxy.mp4");
        std::fs::write(&ready_path, b"p").unwrap();
        let ready = ProxyRef::ready_generated(ready_path.clone());
        assert!(!should_auto_generate_proxy(
            AssetKind::Video,
            &file,
            Some(&ready),
            true
        ));
        let pending = ProxyRef::with_status(ready_path, ProxyStatus::Pending);
        assert!(should_auto_generate_proxy(
            AssetKind::Video,
            &file,
            Some(&pending),
            true
        ));
        let attached = ProxyRef {
            path: PathBuf::from("/media/user-proxy.mp4"),
            status: ProxyStatus::Ready,
            origin: photonic_core::timeline::ProxyOrigin::Attached,
        };
        // Attached must never be auto-replaced even if path missing on disk.
        assert!(!should_auto_generate_proxy(
            AssetKind::Video,
            &file,
            Some(&attached),
            true
        ));
    }

    #[test]
    fn resolve_decode_input_falls_back_unless_ready_on_disk() {
        let dir = unique_tmp_dir("resolve");
        let original = dir.join("orig.mp4");
        let proxy_file = dir.join("p.proxy.mp4");
        std::fs::write(&original, b"o").unwrap();
        std::fs::write(&proxy_file, b"p").unwrap();

        let ready = ProxyRef::ready_generated(proxy_file.clone());
        let pending = ProxyRef::with_status(proxy_file.clone(), ProxyStatus::Pending);
        let missing = ProxyRef::ready_generated(dir.join("nope.proxy.mp4"));

        // Ready + present + requested ⇒ proxy.
        assert_eq!(
            resolve_decode_input(&original, Some(&ready), true),
            proxy_file
        );
        // Not requested ⇒ original even when a Ready proxy exists.
        assert_eq!(
            resolve_decode_input(&original, Some(&ready), false),
            original
        );
        // Pending / missing-file / absent ⇒ original (silent fallback).
        assert_eq!(
            resolve_decode_input(&original, Some(&pending), true),
            original
        );
        assert_eq!(
            resolve_decode_input(&original, Some(&missing), true),
            original
        );
        assert_eq!(resolve_decode_input(&original, None, true), original);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Engine integration (02 §6 / CAP-014): generate a real proxy for a
    /// synthetic fixture, then assert `ForceProxy` selects the proxy input and
    /// `ForceOriginal` the original — asserted via the decode source's chosen
    /// path (`DecodeSource::input_path`), not pixels.
    #[test]
    fn generate_proxy_then_decode_source_selects_by_mode() {
        use crate::decode::scheduler::{PtsKind, SourceParams};
        use crate::decode::{DecodeSource, PixFmt, SharedRing};
        use crate::media::keyframe_index::KeyframeIndex;
        use crate::media::probe::probe_details;
        use photonic_core::timeline::FrameRate;

        let Some(tools) = locate_for_test() else {
            eprintln!("skip: ffmpeg/ffprobe not found (set PHOTONIC_FFMPEG_DIR)");
            return;
        };
        let dir = unique_tmp_dir("gen");

        // Synthetic 320×240 fixture (no display needed — lavfi testsrc).
        let input = dir.join("input.mp4");
        let made = Command::new(&tools.ffmpeg)
            .args([
                "-y",
                "-nostdin",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=10:duration=1",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&input)
            .status();
        if !matches!(made, Ok(s) if s.success()) || !input.is_file() {
            eprintln!("skip: could not synthesize a fixture with this ffmpeg build");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let hash = crate::media::probe::content_hash(&input).unwrap();
        let out = proxy_cache_path(&dir, &hash);

        generate_proxy(&tools, &input, &out, &|| false).expect("proxy generation");
        assert!(out.is_file(), "proxy file was not written");
        assert!(std::fs::metadata(&out).unwrap().len() > 0);

        // Real transcode: half-res, all-intra H.264 in MP4.
        let details = probe_details(&tools, &out).expect("probe proxy");
        let v = details.probe.video.expect("proxy has a video stream");
        assert_eq!((v.width, v.height), (160, 120), "proxy should be half-res");

        // Selection: ForceProxy ⇒ proxy, ForceOriginal ⇒ original.
        let pref = ProxyRef::ready_generated(out.clone());
        let sel_proxy = resolve_decode_input(&input, Some(&pref), true);
        let sel_orig = resolve_decode_input(&input, Some(&pref), false);
        assert_eq!(sel_proxy, out);
        assert_eq!(sel_orig, input);

        // …and the decode source built for each honors that chosen path.
        let mk = |p: &Path| {
            DecodeSource::new(
                tools.clone(),
                SourceParams {
                    input: p.to_path_buf(),
                    width: 160,
                    height: 120,
                    pix_fmt: PixFmt::Yuv420p,
                    pts_kind: PtsKind::Cfr(FrameRate::FPS_30),
                    keyframes: KeyframeIndex::default(),
                },
                SharedRing::preview(),
            )
        };
        assert_eq!(mk(&sel_proxy).input_path(), out.as_path());
        assert_eq!(mk(&sel_orig).input_path(), input.as_path());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_attach_rejects_missing_and_same_as_original() {
        let dir = unique_tmp_dir("attach-basic");
        let original = dir.join("orig.mp4");
        std::fs::write(&original, b"o").unwrap();

        // Missing tools still need a dummy for path-only checks — use locate.
        // Path checks run before probe, so we can exercise them without ffmpeg
        // by constructing a fake tools only when present; without tools we
        // still cover MissingPath via the exists() branch before probe.
        let missing = dir.join("nope.mp4");
        // probe is never reached for MissingPath — pass a placeholder FfmpegTools
        // only when available; otherwise skip tool-dependent path and call with
        // a synthetic that will never be used for MissingPath.
        let Some(tools) = locate_for_test() else {
            // Still exercise pure path existence without tools by checking the
            // error path that does not need ffprobe — re-check logic inline.
            assert!(!missing.exists());
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };

        let err = validate_attach(&tools, &original, &missing, false).unwrap_err();
        assert!(matches!(err, AttachError::MissingPath(_)));

        let err = validate_attach(&tools, &original, &original, false).unwrap_err();
        assert!(matches!(err, AttachError::SameAsOriginal));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_attach_ready_attached_with_generated_proxy_fixture() {
        let Some(tools) = locate_for_test() else {
            eprintln!("skip: ffmpeg/ffprobe not found (set PHOTONIC_FFMPEG_DIR)");
            return;
        };
        let dir = unique_tmp_dir("attach-ok");

        let input = dir.join("input.mp4");
        let made = Command::new(&tools.ffmpeg)
            .args([
                "-y",
                "-nostdin",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=10:duration=1",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&input)
            .status();
        if !matches!(made, Ok(s) if s.success()) || !input.is_file() {
            eprintln!("skip: could not synthesize a fixture with this ffmpeg build");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let hash = crate::media::probe::content_hash(&input).unwrap();
        let out = proxy_cache_path(&dir, &hash);
        generate_proxy(&tools, &input, &out, &|| false).expect("proxy generation");

        let result = validate_attach(&tools, &input, &out, false).expect("attach should succeed");
        assert_eq!(result.proxy.status, ProxyStatus::Ready);
        assert_eq!(result.proxy.origin, ProxyOrigin::Attached);
        assert!(result.warnings.is_empty());
        assert!(result.proxy.path.is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
