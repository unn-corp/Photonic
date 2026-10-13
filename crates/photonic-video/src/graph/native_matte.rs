//! Explicit bounded model-input bridge; this is an inference encoding, not an
//! image-output transform. The upstream AP1 image is never modified. HDR/out-of-
//! gamut model inputs clip to encoded sRGB; model quality there is unqualified.
use photonic_core::raster::image::RasterImage;
use photonic_matte::worker::MAX_INPUT_PIXELS;

/// Every demanded matte for an exact frame must fit simultaneously. Otherwise
/// LRU eviction could make an exact multi-node frame permanently unfinishable.
pub(crate) fn validate_capacity(
    graph: &super::ir::FrameGraph,
    canvas: (u32, u32),
    tap: Option<super::ir::IrNodeId>,
) -> Result<(), &'static str> {
    let demanded = graph.demanded_nodes(tap);
    let keys: std::collections::HashSet<_> = graph
        .nodes
        .iter()
        .zip(demanded)
        .filter(|(node, needed)| *needed && matches!(node.op, super::ir::IrOp::NativeMatteExtract))
        .map(|(node, _)| node.content_hash)
        .collect();
    let bytes = ((canvas.0 as u64) * (canvas.1 as u64))
        .checked_mul(keys.len() as u64)
        .unwrap_or(u64::MAX);
    if keys.len() > photonic_matte::worker::MAX_CACHE_ENTRIES
        || bytes > photonic_matte::worker::MAX_CACHE_BYTES as u64
    {
        return Err(
            "native subject matte exceeds the eight-result / 16 MiB simultaneous-mask budget",
        );
    }
    Ok(())
}

pub(crate) fn model_input(w: u32, h: u32, pixels: &[[f32; 4]]) -> Result<RasterImage, String> {
    let count = (w as usize)
        .checked_mul(h as usize)
        .ok_or("matte dimensions overflow")?;
    if w == 0 || h == 0 || count > MAX_INPUT_PIXELS || pixels.len() != count {
        return Err("native matte requires a complete image within four megapixels".into());
    }
    let matrix = crate::color::native::ap1_to_linear_srgb();
    let mut bytes = Vec::with_capacity(count * 4);
    for &pixel in pixels {
        if pixel.iter().any(|v| !v.is_finite()) || !(0.0..=1.0).contains(&pixel[3]) {
            return Err("native matte requires finite AP1 pixels and alpha in 0..=1".into());
        }
        let alpha = pixel[3] as f64;
        let rgb = if alpha > 0.0 {
            matrix.transform([
                pixel[0] as f64 / alpha,
                pixel[1] as f64 / alpha,
                pixel[2] as f64 / alpha,
            ])
        } else {
            [0.0; 3]
        };
        for value in rgb {
            let value = value.clamp(0.0, 1.0);
            let encoded = if value <= 0.0031308 {
                12.92 * value
            } else {
                1.055 * value.powf(1.0 / 2.4) - 0.055
            };
            bytes.push((encoded * 255.0).round().clamp(0.0, 255.0) as u8);
        }
        bytes.push((alpha * 255.0).round() as u8);
    }
    RasterImage::from_rgba(w, h, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_readback_crops_logical_pixels_and_checks_contract() {
        use std::sync::Arc;
        let gpu = super::super::eval::GpuContext::request_blocking()
            .expect("GPU qualification requires adapter");
        let texture = Arc::new(gpu.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("native-matte-readback-test"),
            size: wgpu::Extent3d {
                width: 32,
                height: 8,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        }));
        let mut bytes = Vec::new();
        for _y in 0..8 {
            for x in 0..32 {
                let rgba = if x % 2 == 0 {
                    [0x3800u16; 4]
                } else {
                    [0x3400u16, 0x3400, 0x3400, 0x3c00]
                };
                for v in rgba {
                    bytes.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
        gpu.queue().write_texture(
            texture.as_image_copy(),
            &bytes,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(8),
            },
            texture.size(),
        );
        assert!(read_model_input(&gpu, &texture, 33, 3).is_err());
        let caller = std::thread::current().id();
        let handle = std::thread::spawn(move || {
            assert_ne!(caller, std::thread::current().id());
            read_model_input(&gpu, &texture, 17, 3)
        });
        let image = handle.join().unwrap().unwrap();
        assert_eq!((image.width, image.height), (17, 3));
        for (i, pixel) in image.pixels.chunks_exact(4).enumerate() {
            let expected = if i % 17 % 2 == 0 {
                [255, 255, 255, 128]
            } else {
                [137, 137, 137, 255]
            };
            for c in 0..4 {
                assert!(
                    (pixel[c] as i16 - expected[c] as i16).abs() <= 1,
                    "{pixel:?} vs {expected:?}"
                );
            }
        }
    }
    #[test]
    fn half_decoder_preserves_finite_values_and_nonfinite_rejection() {
        assert_eq!(half(0x3c00), 1.0);
        assert_eq!(half(0xbc00), -1.0);
        assert_eq!(half(1), 2.0f32.powi(-24));
        assert_eq!(half(0x7bff), 65504.0);
        assert!(half(0x7c00).is_infinite());
        assert!(half(0x7e00).is_nan());
    }
    #[test]
    fn independently_formed_srgb_patches_roundtrip_through_ap1_model_input() {
        // Fixed independent colorimetric matrix, not the inverse under test.
        let matrix = [
            [0.6130974024, 0.3395231462, 0.0473794514],
            [0.0701937225, 0.9163538791, 0.0134523984],
            [0.0206155920, 0.1095697729, 0.8698146351],
        ];
        let mut pixels = Vec::new();
        let mut expected = Vec::new();
        for rgb in [
            [0u8, 0, 0],
            [255, 255, 255],
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [25, 140, 219],
            [1, 10, 127],
        ] {
            for alpha in [1.0f64, 0.5, 0.01, 0.0] {
                let linear = rgb.map(|v| {
                    let v = v as f64 / 255.0;
                    if v <= 0.04045 {
                        v / 12.92
                    } else {
                        ((v + 0.055) / 1.055).powf(2.4)
                    }
                });
                let mut pixel = [0.0; 4];
                for c in 0..3 {
                    pixel[c] =
                        ((0..3).map(|i| matrix[c][i] * linear[i]).sum::<f64>() * alpha) as f32;
                }
                pixel[3] = alpha as f32;
                pixels.push(pixel);
                expected.push(if alpha == 0.0 {
                    [0, 0, 0, 0]
                } else {
                    [rgb[0], rgb[1], rgb[2], (alpha * 255.0).round() as u8]
                });
            }
        }
        let actual = model_input(pixels.len() as u32, 1, &pixels).unwrap();
        for (actual, expected) in actual.pixels.chunks_exact(4).zip(expected) {
            for c in 0..4 {
                assert!(
                    (actual[c] as i16 - expected[c] as i16).abs() <= 1,
                    "{actual:?} {expected:?}"
                );
            }
        }
    }
    #[test]
    fn native_matte_input_guards_and_alpha_independence() {
        for pixel in [
            [f32::NAN, 0., 0., 1.],
            [0., f32::INFINITY, 0., 1.],
            [0., 0., 0., -0.1],
            [0., 0., 0., 1.1],
        ] {
            assert!(model_input(1, 1, &[pixel]).is_err());
        }
        assert!(model_input(2, 2, &[[0.; 4]]).is_err());
        assert!(model_input(0, 1, &[]).is_err());
        assert!(model_input(u32::MAX, u32::MAX, &[]).is_err());
        let transparent = model_input(1, 1, &[[100., -100., 50., 0.]]).unwrap();
        assert_eq!(transparent.pixels, [0, 0, 0, 0]);
        let opaque = model_input(1, 1, &[[0.25, 0.25, 0.25, 1.0]]).unwrap();
        let partial = model_input(1, 1, &[[0.125, 0.125, 0.125, 0.5]]).unwrap();
        assert_eq!(
            &opaque.pixels[..3],
            &partial.pixels[..3],
            "unassociated RGB must not change with coverage"
        );
        assert_eq!(partial.pixels[3], 128);
        let clipped = model_input(1, 1, &[[100., 100., 100., 1.]]).unwrap();
        assert_eq!(clipped.pixels, [255, 255, 255, 255]);
    }
}

/// Called only inside the matte worker's preparation closure. Holding the Arc
/// pins the immutable source against the GPU pool's strong-count recycle guard.
pub(crate) fn read_model_input(
    gpu: &super::eval::GpuContext,
    texture: &wgpu::Texture,
    w: u32,
    h: u32,
) -> Result<RasterImage, String> {
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };
    let count = (w as usize)
        .checked_mul(h as usize)
        .ok_or("matte dimensions overflow")?;
    if w == 0
        || h == 0
        || count > MAX_INPUT_PIXELS
        || w > texture.width()
        || h > texture.height()
        || texture.format() != wgpu::TextureFormat::Rgba16Float
        || !texture.usage().contains(wgpu::TextureUsages::COPY_SRC)
    {
        return Err("native matte readback requires bounded RGBA16F copyable input".into());
    }
    let row = w
        .checked_mul(8)
        .and_then(|v| v.checked_add(255))
        .map(|v| v / 256 * 256)
        .ok_or("matte row overflow")?;
    let size = u64::from(row) * u64::from(h);
    let staging = gpu.device().create_buffer(&wgpu::BufferDescriptor {
        label: Some("native-matte-worker-readback"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device().create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::ImageCopyBuffer {
            buffer: &staging,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue().submit([encoder.finish()]);
    let slice = staging.slice(..);
    let (tx, rx) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        gpu.device().poll(wgpu::Maintain::Poll);
        match rx.try_recv() {
            Ok(result) => {
                result.map_err(|e| format!("native matte map failed: {e}"))?;
                break;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err("native matte map callback disconnected".into())
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if Instant::now() >= deadline {
            return Err("native matte readback deadline exceeded".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let raw = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity(count);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let offset = y * row as usize + x * 8;
            pixels.push(std::array::from_fn(|c| {
                half(u16::from_le_bytes([
                    raw[offset + c * 2],
                    raw[offset + c * 2 + 1],
                ]))
            }));
        }
    }
    drop(raw);
    staging.unmap();
    model_input(w, h, &pixels)
}
fn half(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = (bits >> 10) & 31;
    let fraction = bits & 1023;
    let value = match exponent {
        0 => (fraction as f32) * 2.0f32.powi(-24),
        31 => {
            if fraction == 0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => (1.0 + fraction as f32 / 1024.0) * 2.0f32.powi(exponent as i32 - 15),
    };
    sign * value
}
