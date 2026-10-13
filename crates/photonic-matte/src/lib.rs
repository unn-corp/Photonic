//! Local, offline **background removal** (image matting).
//!
//! Runs the small, permissively-licensed **U²-Net-p** salient-object model
//! (~4.7 MB, Apache-2.0) fully on-device via `ort` (ONNX Runtime) — the same
//! runtime already used by [`photonic-embed`]. The model is downloaded once to a
//! per-user cache on first use, then works offline. No external AI service is
//! required.
//!
//! The public entry point [`remove_background`] takes a [`RasterImage`] and
//! returns a [`Mask`] — an 8-bit **foreground matte** (255 = subject, 0 =
//! background) sized to the input image. Callers apply it as a non-destructive
//! layer mask (see `RasterNode::set_foreground_matte`), so the operation is
//! fully reversible.

pub mod worker;

use anyhow::{anyhow, Context, Result};
use ndarray::Array4;
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use photonic_core::raster::{image::RasterImage, mask::Mask};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// U²-Net-p ONNX model, hosted on the `rembg` GitHub release. Overridable via the
/// `PHOTONIC_RMBG_MODEL_URL` environment variable.
const DEFAULT_MODEL_URL: &str =
    "https://github.com/danielgatis/rembg/releases/download/v0.0.0/u2netp.onnx";

/// The square side the U²-Net family expects at its input.
const INPUT_SIZE: u32 = 320;

/// ImageNet normalization constants used by the U²-Net preprocessing.
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// Stable per-user cache directory for the model, mirroring `photonic-embed`:
/// `$XDG_CACHE_HOME/Photonic/models`, `%APPDATA%\Photonic\models`, or
/// `~/.cache/Photonic/models`.
fn model_cache_dir() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::var("APPDATA").ok().map(PathBuf::from))
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".cache"))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Photonic").join("models")
}

/// Resolve the local model path, downloading it once if it is not already
/// cached. Honors `PHOTONIC_RMBG_MODEL_PATH` (use an existing local file) and
/// `PHOTONIC_RMBG_MODEL_URL` (override the download source).
fn ensure_model() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("PHOTONIC_RMBG_MODEL_PATH") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Ok(path);
        }
        return Err(anyhow!(
            "PHOTONIC_RMBG_MODEL_PATH points at a missing file: {}",
            path.display()
        ));
    }

    let dir = model_cache_dir();
    let path = dir.join("u2netp.onnx");
    // A previous partial download can leave a tiny truncated file that ort will
    // fail to parse; treat anything implausibly small as absent.
    let good = std::fs::metadata(&path)
        .map(|m| m.len() > 1_000_000)
        .unwrap_or(false);
    if good {
        return Ok(path);
    }

    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating model cache dir {}", dir.display()))?;
    let url =
        std::env::var("PHOTONIC_RMBG_MODEL_URL").unwrap_or_else(|_| DEFAULT_MODEL_URL.to_string());
    tracing::info!(url = %url, "downloading background-removal model (once)");

    let resp = ureq::get(&url)
        .call()
        .with_context(|| format!("downloading matting model from {url}"))?;
    let mut bytes: Vec<u8> = Vec::new();
    std::io::copy(&mut resp.into_reader(), &mut bytes).context("reading model response body")?;
    if bytes.len() < 1_000_000 {
        return Err(anyhow!(
            "downloaded model is implausibly small ({} bytes) — the source URL may be stale; \
             set PHOTONIC_RMBG_MODEL_URL or PHOTONIC_RMBG_MODEL_PATH",
            bytes.len()
        ));
    }

    // Write atomically via a temp file so a crash mid-write can't leave a
    // half-written model that later reads treat as valid.
    let tmp = path.with_extension("onnx.part");
    std::fs::write(&tmp, &bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("finalizing {}", path.display()))?;
    tracing::info!(path = %path.display(), bytes = bytes.len(), "matting model ready");
    Ok(path)
}

/// Process-wide cached session. Loading the model + spinning up an ort session is
/// slow (hundreds of ms); reuse it across calls. Held behind a `OnceLock` of a
/// `Result` string so a load failure is reported on every call without retrying a
/// broken model each time within a run.
fn session() -> Result<&'static Session> {
    static SESSION: OnceLock<std::result::Result<Session, String>> = OnceLock::new();
    SESSION
        .get_or_init(|| build_session().map_err(|e| format!("{e:#}")))
        .as_ref()
        .map_err(|e| anyhow!(e.clone()))
}

fn build_session() -> Result<Session> {
    let path = ensure_model()?;
    let session = Session::builder()
        .context("creating ort session builder")?
        .with_intra_threads(1)
        .context("bounding matting inference threads")?
        .with_inter_threads(1)
        .context("bounding matting graph threads")?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .context("setting optimization level")?
        .commit_from_file(&path)
        .with_context(|| format!("loading ONNX model {}", path.display()))?;
    Ok(session)
}

/// SHA-256 of the pinned upstream U²-Net-p release. Its upstream MD5 also
/// matches rembg's published 8e83ca70e441ab06c318d82300c84806. Native timeline
/// evaluation must never download or silently substitute a different model.
pub const PINNED_MODEL_SHA256: &str =
    "309c8469258dda742793dce0ebea8e6dd393174f89934733ecc8b14c76f4ddd8";
const MAX_PINNED_MODEL_BYTES: u64 = 8 * 1024 * 1024;

/// Owned, serial inference session for a background worker. Loading and inference
/// belong on that worker, never the GUI/engine thread. This API is offline-only.
pub struct CachedMatteModel {
    session: Session,
}
impl CachedMatteModel {
    pub fn load_cached() -> Result<Self> {
        let path = std::env::var("PHOTONIC_RMBG_MODEL_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| model_cache_dir().join("u2netp.onnx"));
        Self::load_pinned(&path)
    }
    pub fn load_pinned(path: &Path) -> Result<Self> {
        use std::io::Read;
        let file = std::fs::File::open(path)
            .with_context(|| format!("cached matting model missing: {}", path.display()))?;
        let mut bytes = Vec::new();
        file.take(MAX_PINNED_MODEL_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("reading cached matting model")?;
        verify_pinned_model(&bytes)?;
        // Load verified bytes, not the path, to avoid a replace-between-check/load race.
        let session = Session::builder()?
            .with_intra_threads(1)?
            .with_inter_threads(1)?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .commit_from_memory(&bytes)
            .context("loading pinned matting model")?;
        Ok(Self { session })
    }
    pub fn matte(&self, img: &RasterImage) -> Result<Mask> {
        infer(&self.session, img)
    }
}
fn verify_pinned_model(bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 > MAX_PINNED_MODEL_BYTES
        || format!("{:x}", Sha256::digest(bytes)) != PINNED_MODEL_SHA256
    {
        return Err(anyhow!(
            "cached matting model does not match pinned U2NetP SHA-256"
        ));
    }
    Ok(())
}

/// Whether the background-removal model is already downloaded (so the UI can hint
/// that the first click will pull ~4.7 MB).
pub fn model_is_cached() -> bool {
    if let Ok(p) = std::env::var("PHOTONIC_RMBG_MODEL_PATH") {
        return PathBuf::from(p).is_file();
    }
    std::fs::metadata(model_cache_dir().join("u2netp.onnx"))
        .map(|m| m.len() > 1_000_000)
        .unwrap_or(false)
}

/// Compute a **foreground matte** for `img`: a [`Mask`] the same size as `img`
/// with coverage `255` over the salient subject and `0` over the background.
///
/// Slow (model load on first call, then inference); run off the UI thread.
pub fn remove_background(img: &RasterImage) -> Result<Mask> {
    validate_image(img)?;
    let session = session()?;
    infer(session, img)
}

fn infer(session: &Session, img: &RasterImage) -> Result<Mask> {
    let input = preprocess(img)?;

    // ── Run inference ────────────────────────────────────────────────────────
    let input_name = session
        .inputs
        .first()
        .ok_or_else(|| anyhow!("matting model has no input"))?
        .name
        .clone();
    let output_name = session
        .outputs
        .first()
        .ok_or_else(|| anyhow!("matting model has no output"))?
        .name
        .clone();
    let tensor = Tensor::from_array(input).context("building input tensor")?;
    let outputs = session
        .run(ort::inputs![input_name.as_str() => tensor]?)
        .context("running matting inference")?;
    let output = outputs[output_name.as_str()]
        .try_extract_tensor::<f32>()
        .context("extracting matting output")?;
    // `output` is an ndarray view of shape [1, 1, 320, 320]; flatten in logical
    // (row-major) order.
    let data: Vec<f32> = output.iter().copied().collect();

    if output.shape() != [1, 1, INPUT_SIZE as usize, INPUT_SIZE as usize] {
        return Err(anyhow!(
            "unexpected matting output shape: {:?}",
            output.shape()
        ));
    }
    let matte_small = normalize_prediction(&data)?;
    let matte = resize_gray(&matte_small, INPUT_SIZE, INPUT_SIZE, img.width, img.height);
    Ok(Mask {
        width: img.width,
        height: img.height,
        data: matte,
    })
}

/// Validate before loading a model or allocating its input. RasterImage fields
/// are public, so callers can bypass its checked constructors.
fn validate_image(img: &RasterImage) -> Result<()> {
    use photonic_core::raster::image::{MAX_RASTER_DIMENSION, MAX_RASTER_PIXELS};
    let pixels = (img.width as usize)
        .checked_mul(img.height as usize)
        .ok_or_else(|| anyhow!("matting image size overflow"))?;
    if img.width == 0
        || img.height == 0
        || img.width > MAX_RASTER_DIMENSION
        || img.height > MAX_RASTER_DIMENSION
        || pixels > MAX_RASTER_PIXELS
        || pixels.checked_mul(4) != Some(img.pixels.len())
    {
        return Err(anyhow!("matting requires a bounded, complete RGBA image"));
    }
    Ok(())
}

fn preprocess(img: &RasterImage) -> Result<Array4<f32>> {
    validate_image(img)?;
    let small = resize_rgba(img, INPUT_SIZE, INPUT_SIZE);
    // The model consumes RGB. Alpha must not change the ImageNet divisor.
    let max = small
        .chunks_exact(4)
        .flat_map(|p| p[..3].iter().copied())
        .max()
        .unwrap_or(0)
        .max(1) as f32;
    let mut input = Array4::<f32>::zeros((1, 3, INPUT_SIZE as usize, INPUT_SIZE as usize));
    for y in 0..INPUT_SIZE as usize {
        for x in 0..INPUT_SIZE as usize {
            let i = (y * INPUT_SIZE as usize + x) * 4;
            for c in 0..3 {
                input[[0, c, y, x]] = (small[i + c] as f32 / max - MEAN[c]) / STD[c];
            }
        }
    }
    Ok(input)
}

fn normalize_prediction(data: &[f32]) -> Result<Vec<u8>> {
    let n = (INPUT_SIZE * INPUT_SIZE) as usize;
    if data.len() != n || data.iter().any(|v| !v.is_finite()) {
        return Err(anyhow!(
            "matting output must have exactly {n} finite samples"
        ));
    }
    let lo = data.iter().copied().fold(f32::INFINITY, f32::min) as f64;
    let hi = data.iter().copied().fold(f32::NEG_INFINITY, f32::max) as f64;
    // Evaluate in f64 so finite extreme predictions cannot overflow the range.
    let range = (hi - lo).max(1e-6);
    Ok(data
        .iter()
        .map(|&v| (((v as f64 - lo) / range).clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect())
}

/// High-quality (Lanczos3) resize of an RGBA image to `w×h`, returning raw RGBA
/// bytes.
fn resize_rgba(img: &RasterImage, w: u32, h: u32) -> Vec<u8> {
    let buf: image::RgbaImage =
        match image::ImageBuffer::from_raw(img.width, img.height, img.pixels.clone()) {
            Some(b) => b,
            None => return vec![0u8; (w as usize) * (h as usize) * 4],
        };
    let resized = image::imageops::resize(&buf, w, h, image::imageops::FilterType::Lanczos3);
    resized.into_raw()
}

/// High-quality (Lanczos3) resize of a single-channel gray buffer.
fn resize_gray(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let buf: image::GrayImage = match image::ImageBuffer::from_raw(sw, sh, src.to_vec()) {
        Some(b) => b,
        None => return vec![0u8; (dw as usize) * (dh as usize)],
    };
    let resized = image::imageops::resize(&buf, dw, dh, image::imageops::FilterType::Lanczos3);
    resized.into_raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_images_fail_before_model_access() {
        for (w, h, len) in [
            (0, 1, 0),
            (1, 0, 0),
            (2, 2, 15),
            (1, 1, 5),
            (u32::MAX, u32::MAX, 0),
            (16385, 1, 0),
        ] {
            let img = RasterImage {
                width: w,
                height: h,
                pixels: vec![0; len],
            };
            assert!(remove_background(&img)
                .unwrap_err()
                .to_string()
                .contains("bounded"));
        }
    }

    #[test]
    fn model_preprocessing_uses_rgb_not_alpha_for_normalization() {
        let a = RasterImage::filled(320, 320, [10, 20, 40, 255]);
        let b = RasterImage::filled(320, 320, [10, 20, 40, 64]);
        let x = preprocess(&a).unwrap();
        let y = preprocess(&b).unwrap();
        assert_eq!(x, y);
        for c in 0..3 {
            let expected =
                ([0.25f64, 0.5, 1.0][c] - [0.485, 0.456, 0.406][c]) / [0.229, 0.224, 0.225][c];
            assert!((x[[0, c, 0, 0]] as f64 - expected).abs() < 0.000001);
        }
        assert!(preprocess(&RasterImage::filled(1, 1, [0, 0, 0, 0]))
            .unwrap()
            .iter()
            .all(|v| v.is_finite()));
    }

    #[test]
    fn prediction_shape_nonfinite_and_extremes_are_checked() {
        let n = (INPUT_SIZE * INPUT_SIZE) as usize;
        assert!(normalize_prediction(&vec![0.0; n - 1]).is_err());
        assert!(normalize_prediction(&vec![0.0; n + 1]).is_err());
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut a = vec![0.0; n];
            a[7] = bad;
            assert!(normalize_prediction(&a).is_err());
        }
        assert!(normalize_prediction(&vec![0.75; n])
            .unwrap()
            .iter()
            .all(|v| *v == 0));
        let mut a = vec![0.0; n];
        a[0] = -f32::MAX;
        a[1] = f32::MAX;
        let out = normalize_prediction(&a).unwrap();
        assert_eq!(&out[..3], &[0, 255, 128]);
        let mut a = vec![0.0; n];
        a[0] = -2.0;
        a[1] = 2.0;
        a[2] = 1.0;
        let out = normalize_prediction(&a).unwrap();
        assert_eq!(&out[..4], &[0, 255, 191, 128]);
    }

    #[test]
    fn pinned_model_rejects_unverified_or_oversized_bytes() {
        assert!(verify_pinned_model(&[]).is_err());
        assert!(verify_pinned_model(&[1, 2, 3]).is_err());
        assert!(verify_pinned_model(&vec![0; MAX_PINNED_MODEL_BYTES as usize + 1]).is_err());
    }

    #[test]
    #[ignore = "requires the explicitly provisioned pinned local model"]
    fn pinned_cached_subject_and_repeat_are_stable() {
        let model = CachedMatteModel::load_cached().expect("pinned offline model");
        let mut img = RasterImage::filled(96, 96, [12, 12, 20, 255]);
        for y in 30..66 {
            for x in 30..66 {
                img.set_pixel(x, y, [230, 220, 60, 255]);
            }
        }
        let a = model.matte(&img).unwrap();
        let b = model.matte(&img).unwrap();
        assert_eq!(a.data, b.data);
        assert_eq!((a.width, a.height), (96, 96));
        assert!(a.get(48, 48) as u32 > a.get(3, 3) as u32 + 40);
        eprintln!(
            "pinned matte center={} corner={} repeated_exact=true",
            a.get(48, 48),
            a.get(3, 3)
        );
    }

    /// End-to-end: downloads the model on first run, then mattes a synthetic
    /// image (bright square on a dark field). Network + ~4.7 MB download, so it
    /// is `#[ignore]`d by default. Run with:
    ///   cargo test -p photonic-matte -- --ignored --nocapture
    #[test]
    #[ignore]
    fn matte_synthetic_subject() {
        let mut img = RasterImage::filled(96, 96, [12, 12, 20, 255]);
        for y in 30..66 {
            for x in 30..66 {
                img.set_pixel(x, y, [230, 220, 60, 255]);
            }
        }
        let mask = remove_background(&img).expect("matte");
        assert_eq!((mask.width, mask.height), (96, 96));
        let center = mask.get(48, 48) as u32;
        let corner = mask.get(3, 3) as u32;
        eprintln!("center={center} corner={corner}");
        assert!(
            center > corner + 40,
            "subject center ({center}) should score well above background corner ({corner})"
        );
    }
}
