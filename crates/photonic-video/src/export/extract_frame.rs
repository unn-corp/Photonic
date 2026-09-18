//! K-E4 — extract a fully-composited program frame to a still PNG.
//!
//! Uses the same colour path as export (`working_frame_to_rgba8`) so the still
//! matches what the user sees on the program monitor at Full quality, not a
//! Draft-resolution preview surprise (26 K-E4).

use std::path::{Path, PathBuf};

use image::RgbaImage;

use super::convert::{working_frame_to_rgba8, EncodePlanes};

/// Errors from extracting a frame still.
#[derive(Debug, thiserror::Error)]
pub enum ExtractFrameError {
    #[error("empty pixel buffer")]
    Empty,
    #[error("pixel buffer size mismatch ({w}×{h}, {len} floats)")]
    SizeMismatch { w: u32, h: u32, len: usize },
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("PNG encode: {0}")]
    Encode(String),
}

/// Encode a linear premultiplied f32 RGBA frame to an sRGB PNG on disk.
///
/// `rgba_premult` is interleaved `[r,g,b,a,…]` or can be built from
/// `Vec<[f32;4]>` via [`flatten_pixels`].
pub fn write_frame_png(
    rgba_premult: &[f32],
    width: u32,
    height: u32,
    path: &Path,
) -> Result<PathBuf, ExtractFrameError> {
    if width == 0 || height == 0 {
        return Err(ExtractFrameError::Empty);
    }
    let expected = (width as usize) * (height as usize) * 4;
    if rgba_premult.len() != expected {
        return Err(ExtractFrameError::SizeMismatch {
            w: width,
            h: height,
            len: rgba_premult.len(),
        });
    }
    let EncodePlanes::Rgba8 { rgba, .. } = working_frame_to_rgba8(rgba_premult, width, height)
    else {
        return Err(ExtractFrameError::Encode(
            "unexpected plane kind from convert".into(),
        ));
    };
    let img = RgbaImage::from_raw(width, height, rgba).ok_or(ExtractFrameError::Empty)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    img.save(path)
        .map_err(|e| ExtractFrameError::Encode(e.to_string()))?;
    Ok(path.to_path_buf())
}

/// Flatten `[[r,g,b,a], …]` into the interleaved buffer [`write_frame_png`] wants.
pub fn flatten_pixels(pixels: &[[f32; 4]]) -> Vec<f32> {
    let mut out = Vec::with_capacity(pixels.len() * 4);
    for p in pixels {
        out.extend_from_slice(p);
    }
    out
}

/// Default output path for an extracted frame (project-adjacent or temp).
pub fn default_extract_path(
    project_path: Option<&Path>,
    sequence_name: &str,
    tick: i64,
) -> PathBuf {
    let safe: String = sequence_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let name = format!("{safe}_t{tick}.png");
    match project_path.and_then(|p| p.parent()) {
        Some(dir) => dir.join("extracts").join(name),
        None => std::env::temp_dir().join("photonic-extracts").join(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_solid_png_roundtrips_size() {
        let w = 8u32;
        let h = 4u32;
        // Linear red, full alpha, premultiplied.
        let mut flat = Vec::new();
        for _ in 0..(w * h) {
            flat.extend_from_slice(&[0.5, 0.0, 0.0, 1.0]);
        }
        let dir = std::env::temp_dir().join(format!("photonic-extract-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("frame.png");
        write_frame_png(&flat, w, h, &path).expect("write");
        assert!(path.is_file());
        let img = image::open(&path).expect("open").to_rgba8();
        assert_eq!(img.dimensions(), (w, h));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_path_uses_extracts_subdir() {
        let p = default_extract_path(Some(Path::new("/proj/movie.photon")), "Seq A", 123);
        assert!(p.to_string_lossy().contains("extracts"));
        assert!(p.to_string_lossy().contains("123"));
    }
}
