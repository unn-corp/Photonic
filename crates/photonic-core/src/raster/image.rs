//! `RasterImage` — the core 8-bit RGBA pixel buffer.
//!
//! Storage is straight (non-premultiplied) alpha, row-major, sRGB — matching
//! Photoshop's default 8-bit mode and the rest of Photonic's color model.
//! Adjustments and filters convert to `f32` internally per-op, so the public
//! representation stays compact while math stays precise.

use base64::Engine;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::io::Cursor;

/// Maximum number of pixels in one persisted raster image or mask.
///
/// This keeps decoded project data bounded while still allowing the largest
/// built-in 24×36 inch, 300-DPI print preset (77,760,000 pixels).
pub(crate) const MAX_PERSISTED_RASTER_PIXELS: usize = 100_000_000;

/// An 8-bit RGBA raster image with straight alpha.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterImage {
    pub width: u32,
    pub height: u32,
    /// RGBA8 pixels, row-major. `len == width * height * 4`.
    pub pixels: Vec<u8>,
}

/// Maximum width or height accepted by MCP raster operations.
pub const MAX_RASTER_DIMENSION: u32 = 16_384;
/// Maximum number of RGBA pixels accepted by MCP raster operations.
pub const MAX_RASTER_PIXELS: usize = 64 * 1024 * 1024;
/// Maximum decoder allocation budget for one encoded raster import.
pub const MAX_RASTER_DECODED_BYTES: u64 = 256 * 1024 * 1024;

impl RasterImage {
    /// A fully transparent image of the given size.
    pub fn new(width: u32, height: u32) -> Self {
        let w = width.max(1);
        let h = height.max(1);
        Self::try_new(w, h).unwrap_or_else(|error| panic!("invalid raster dimensions: {error}"))
    }

    /// A fully transparent image of the given size, returning errors instead of
    /// panicking when the RGBA buffer length cannot be represented or allocated.
    pub fn try_new(width: u32, height: u32) -> Result<Self, String> {
        let pixel_len = checked_pixel_len(width, height)?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(pixel_len)
            .map_err(|error| format!("could not allocate raster pixel buffer: {error}"))?;
        pixels.resize(pixel_len, 0);
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// An image filled with a single RGBA color.
    pub fn filled(width: u32, height: u32, rgba: [u8; 4]) -> Self {
        let w = width.max(1);
        let h = height.max(1);
        Self::try_filled(w, h, rgba)
            .unwrap_or_else(|error| panic!("invalid raster dimensions: {error}"))
    }

    /// An image filled with a single RGBA color, returning allocation errors.
    pub fn try_filled(width: u32, height: u32, rgba: [u8; 4]) -> Result<Self, String> {
        let mut img = Self::try_new(width, height)?;
        for px in img.pixels.chunks_exact_mut(4) {
            px.copy_from_slice(&rgba);
        }
        Ok(img)
    }

    /// Build from raw RGBA bytes. Errors if the length doesn't match `w*h*4`.
    pub fn from_rgba(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, String> {
        // A RasterImage is always at least 1×1: reject zero dimensions rather
        // than build a degenerate buffer whose stored size disagrees with its
        // length (which would panic on the next pixel access).
        let expected = checked_pixel_len(width, height)?;
        if pixels.len() != expected {
            return Err(format!(
                "pixel buffer length {} does not match {}x{}x4 = {}",
                pixels.len(),
                width,
                height,
                expected
            ));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Decode a PNG/JPEG/WebP/etc. encoded image into an RGBA8 buffer.
    pub fn from_encoded(bytes: &[u8]) -> Result<Self, String> {
        let img = decode_encoded(bytes)?;
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        Self::from_rgba(width, height, rgba.into_raw())
    }

    /// Encode the image as PNG bytes.
    pub fn to_png(&self) -> Vec<u8> {
        let buf: image::RgbaImage =
            image::ImageBuffer::from_raw(self.width, self.height, self.pixels.clone())
                .unwrap_or_else(|| image::ImageBuffer::new(self.width, self.height));
        let mut out = Vec::new();
        let _ = image::DynamicImage::ImageRgba8(buf)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png);
        out
    }

    #[inline]
    pub fn in_bounds(&self, x: i64, y: i64) -> bool {
        x >= 0 && y >= 0 && (x as u32) < self.width && (y as u32) < self.height
    }

    #[inline]
    pub fn index(&self, x: u32, y: u32) -> usize {
        ((y as usize) * (self.width as usize) + (x as usize)) * 4
    }

    /// Read a pixel. Returns transparent black for out-of-bounds coordinates.
    #[inline]
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0, 0, 0, 0];
        }
        let i = self.index(x, y);
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }

    /// Sample a pixel with clamp-to-edge addressing (never out of bounds).
    #[inline]
    pub fn sample_clamped(&self, x: i64, y: i64) -> [u8; 4] {
        let cx = x.clamp(0, self.width as i64 - 1) as u32;
        let cy = y.clamp(0, self.height as i64 - 1) as u32;
        self.pixel(cx, cy)
    }

    /// Bilinear sample at a floating-point coordinate (clamp-to-edge).
    pub fn sample_bilinear(&self, fx: f32, fy: f32) -> [u8; 4] {
        let x0 = fx.floor();
        let y0 = fy.floor();
        let tx = fx - x0;
        let ty = fy - y0;
        let x0 = x0 as i64;
        let y0 = y0 as i64;
        let c00 = self.sample_clamped(x0, y0);
        let c10 = self.sample_clamped(x0 + 1, y0);
        let c01 = self.sample_clamped(x0, y0 + 1);
        let c11 = self.sample_clamped(x0 + 1, y0 + 1);
        let mut out = [0u8; 4];
        for c in 0..4 {
            let top = c00[c] as f32 * (1.0 - tx) + c10[c] as f32 * tx;
            let bot = c01[c] as f32 * (1.0 - tx) + c11[c] as f32 * tx;
            out[c] = (top * (1.0 - ty) + bot * ty).round().clamp(0.0, 255.0) as u8;
        }
        out
    }

    #[inline]
    pub fn set_pixel(&mut self, x: u32, y: u32, rgba: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let i = self.index(x, y);
        self.pixels[i..i + 4].copy_from_slice(&rgba);
    }

    /// Apply a per-pixel transform across the whole image.
    pub fn map_pixels(&mut self, mut f: impl FnMut([u8; 4]) -> [u8; 4]) {
        for px in self.pixels.chunks_exact_mut(4) {
            let out = f([px[0], px[1], px[2], px[3]]);
            px.copy_from_slice(&out);
        }
    }

    /// Apply a per-pixel RGB transform that leaves alpha untouched. `f` receives
    /// and returns linearless 0..1 RGB; alpha is preserved.
    pub fn map_rgb(&mut self, mut f: impl FnMut([f32; 3]) -> [f32; 3]) {
        for px in self.pixels.chunks_exact_mut(4) {
            let rgb = [
                px[0] as f32 / 255.0,
                px[1] as f32 / 255.0,
                px[2] as f32 / 255.0,
            ];
            let out = f(rgb);
            px[0] = (out[0] * 255.0).round().clamp(0.0, 255.0) as u8;
            px[1] = (out[1] * 255.0).round().clamp(0.0, 255.0) as u8;
            px[2] = (out[2] * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }

    /// Number of pixels (not bytes).
    pub fn len(&self) -> usize {
        (self.width as usize) * (self.height as usize)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn decode_encoded(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    // Read only the encoded header first so a malicious image cannot make the
    // decoder allocate its full pixel buffer before the project limit is known.
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let (width, height) = reader.into_dimensions().map_err(|e| e.to_string())?;
    checked_raster_pixel_count(width, height)?;
    if width > MAX_RASTER_DIMENSION || height > MAX_RASTER_DIMENSION {
        return Err(format!(
            "raster dimensions {width}x{height} exceed the maximum of {MAX_RASTER_DIMENSION}x{MAX_RASTER_DIMENSION}"
        ));
    }

    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_RASTER_DIMENSION);
    limits.max_image_height = Some(MAX_RASTER_DIMENSION);
    limits.max_alloc = Some(MAX_RASTER_DECODED_BYTES);
    reader.limits(limits);
    reader.decode().map_err(|e| e.to_string())
}

pub(crate) fn checked_raster_pixel_count(width: u32, height: u32) -> Result<usize, String> {
    if width == 0 || height == 0 {
        return Err("raster dimensions must be non-zero".to_string());
    }
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| "raster dimensions overflow pixel count".to_string())?;
    if pixels > MAX_PERSISTED_RASTER_PIXELS {
        return Err(format!(
            "raster dimensions {}x{} exceed maximum of {} pixels",
            width, height, MAX_PERSISTED_RASTER_PIXELS
        ));
    }
    Ok(pixels)
}

fn checked_pixel_len(width: u32, height: u32) -> Result<usize, String> {
    if width == 0 || height == 0 {
        return Err("image dimensions must be non-zero".to_string());
    }
    checked_raster_pixel_count(width, height)?
        .checked_mul(4)
        .ok_or_else(|| "image dimensions overflow pixel buffer length".to_string())
}

/// Validate raster dimensions before a caller-controlled operation allocates or
/// resizes an RGBA image. The returned error is suitable for surfacing through an
/// MCP tool result.
pub fn validate_raster_dimensions(width: u32, height: u32) -> Result<(), String> {
    let pixel_len = checked_pixel_len(width, height)?;
    if width > MAX_RASTER_DIMENSION || height > MAX_RASTER_DIMENSION {
        return Err(format!(
            "raster dimensions {}x{} exceed the maximum of {}x{}",
            width, height, MAX_RASTER_DIMENSION, MAX_RASTER_DIMENSION
        ));
    }

    let pixels = pixel_len / 4;
    if pixels > MAX_RASTER_PIXELS {
        return Err(format!(
            "raster dimensions {}x{} contain {} pixels, exceeding the {}-pixel budget",
            width, height, pixels, MAX_RASTER_PIXELS
        ));
    }
    Ok(())
}

// ── sRGB luminance ─────────────────────────────────────────────────────────────

/// Perceptual luma (Rec. 601) of an sRGB pixel, 0..1.
#[inline]
pub fn luma(rgb: [f32; 3]) -> f32 {
    0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2]
}

// ── Serialization (base64 PNG) ──────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct RasterImageRepr {
    width: u32,
    height: u32,
    /// base64-encoded PNG of the pixel data.
    png: String,
}

impl Serialize for RasterImage {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let png = self.to_png();
        let repr = RasterImageRepr {
            width: self.width,
            height: self.height,
            png: base64::engine::general_purpose::STANDARD.encode(png),
        };
        repr.serialize(s)
    }
}

impl<'de> Deserialize<'de> for RasterImage {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let repr = RasterImageRepr::deserialize(d)?;
        let expected =
            checked_pixel_len(repr.width, repr.height).map_err(serde::de::Error::custom)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(repr.png.as_bytes())
            .map_err(serde::de::Error::custom)?;
        let img = RasterImage::from_encoded(&bytes).map_err(serde::de::Error::custom)?;
        if img.width != repr.width || img.height != repr.height {
            return Err(serde::de::Error::custom(format!(
                "stored image dimensions {}x{} do not match decoded dimensions {}x{}",
                repr.width, repr.height, img.width, img.height
            )));
        }
        if img.pixels.len() != expected {
            return Err(serde::de::Error::custom(format!(
                "pixel buffer length {} does not match {}x{}x4 = {}",
                img.pixels.len(),
                repr.width,
                repr.height,
                expected
            )));
        }
        Ok(img)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_transparent() {
        let img = RasterImage::new(4, 3);
        assert_eq!(img.width, 4);
        assert_eq!(img.height, 3);
        assert_eq!(img.pixels.len(), 4 * 3 * 4);
        assert!(img.pixels.iter().all(|&b| b == 0));
    }

    #[test]
    fn filled_and_pixel() {
        let img = RasterImage::filled(2, 2, [10, 20, 30, 255]);
        assert_eq!(img.pixel(0, 0), [10, 20, 30, 255]);
        assert_eq!(img.pixel(1, 1), [10, 20, 30, 255]);
        assert_eq!(img.pixel(5, 5), [0, 0, 0, 0]); // oob
    }

    #[test]
    fn set_pixel_roundtrip() {
        let mut img = RasterImage::new(3, 3);
        img.set_pixel(1, 2, [1, 2, 3, 4]);
        assert_eq!(img.pixel(1, 2), [1, 2, 3, 4]);
    }

    #[test]
    fn from_rgba_validates_length() {
        assert!(RasterImage::from_rgba(2, 2, vec![0; 16]).is_ok());
        assert!(RasterImage::from_rgba(2, 2, vec![0; 15]).is_err());
    }

    #[test]
    fn raster_dimension_validator_enforces_boundaries() {
        assert!(validate_raster_dimensions(0, 1).is_err());
        assert!(validate_raster_dimensions(1, 0).is_err());
        assert!(validate_raster_dimensions(MAX_RASTER_DIMENSION, 1).is_ok());
        assert!(validate_raster_dimensions(MAX_RASTER_DIMENSION + 1, 1).is_err());
        assert!(validate_raster_dimensions(8192, 8192).is_ok());
        assert!(validate_raster_dimensions(8193, 8192).is_err());
    }

    #[test]
    fn try_new_rejects_extreme_dimensions() {
        let error = RasterImage::try_new(u32::MAX, u32::MAX).unwrap_err();
        assert!(
            error.contains("overflow") || error.contains("exceed maximum"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn png_roundtrip() {
        let mut img = RasterImage::filled(8, 6, [200, 100, 50, 255]);
        img.set_pixel(0, 0, [1, 2, 3, 255]);
        let png = img.to_png();
        let back = RasterImage::from_encoded(&png).unwrap();
        assert_eq!(back.width, 8);
        assert_eq!(back.height, 6);
        assert_eq!(back.pixel(0, 0), [1, 2, 3, 255]);
        assert_eq!(back.pixel(7, 5), [200, 100, 50, 255]);
    }

    #[test]
    fn encoded_image_rejects_dimensions_over_limit() {
        let encoded = RasterImage::new(MAX_RASTER_DIMENSION + 1, 1).to_png();
        let err = RasterImage::from_encoded(&encoded).unwrap_err();

        assert!(err.contains("exceed the maximum"), "{err}");
    }

    #[test]
    fn encoded_image_rejects_decoded_bytes_over_limit() {
        // A header-only PPM is enough to exercise ImageReader's allocation
        // check: it rejects the advertised decoded size before reading pixels.
        let width = MAX_RASTER_DIMENSION;
        let height = (MAX_RASTER_DECODED_BYTES / 3 / u64::from(width) + 1) as u32;
        assert!(height <= MAX_RASTER_DIMENSION);
        let header = format!("P6\n{width} {height}\n255\n");

        let err = RasterImage::from_encoded(header.as_bytes()).unwrap_err();
        assert!(err.to_ascii_lowercase().contains("limit"), "{err}");
    }

    #[test]
    fn serde_roundtrip_via_png() {
        let mut img = RasterImage::filled(5, 5, [0, 128, 255, 255]);
        img.set_pixel(2, 2, [255, 0, 0, 128]);
        let json = serde_json::to_string(&img).unwrap();
        assert!(json.contains("\"png\""));
        let back: RasterImage = serde_json::from_str(&json).unwrap();
        assert_eq!(img, back);
    }

    #[test]
    fn serde_rejects_forged_dimensions() {
        let img = RasterImage::filled(1, 1, [1, 2, 3, 255]);
        let mut value = serde_json::to_value(&img).unwrap();
        value["width"] = serde_json::json!(2);
        value["height"] = serde_json::json!(2);

        let err = serde_json::from_value::<RasterImage>(value).unwrap_err();
        assert!(err.to_string().contains("do not match decoded dimensions"));
    }

    #[test]
    fn serde_rejects_oversized_dimensions_before_decoding() {
        let img = RasterImage::filled(1, 1, [1, 2, 3, 255]);
        let mut value = serde_json::to_value(&img).unwrap();
        value["width"] = serde_json::json!((MAX_PERSISTED_RASTER_PIXELS + 1) as u64);

        let err = serde_json::from_value::<RasterImage>(value).unwrap_err();
        assert!(err.to_string().contains("exceed maximum"));
    }

    #[test]
    fn bilinear_midpoint_average() {
        let mut img = RasterImage::new(2, 1);
        img.set_pixel(0, 0, [0, 0, 0, 255]);
        img.set_pixel(1, 0, [100, 100, 100, 255]);
        let mid = img.sample_bilinear(0.5, 0.0);
        assert_eq!(mid[0], 50);
    }
}
