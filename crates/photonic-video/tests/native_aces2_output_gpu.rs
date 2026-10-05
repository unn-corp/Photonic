//! Isolated ACES 2-style SDR GPU/scalar parity; managed rendering stays gated.

use photonic_render::{
    color::Range,
    native_transfer::NativeExposurePass,
    video::{NativeChromaLocation, NativeYuvInput, YuvConverter, YuvPlanes},
};
use photonic_video::{
    color::native_output::{gpu::NativeAces2SdrPass, Aces2SdrOutput},
    graph::eval::{read_texture_rgba16f, GpuContext},
};

#[test]
fn isolated_gpu_output_matches_scalar_on_extended_chromatic_and_alpha_samples() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping isolated ACES 2 SDR parity");
        return;
    };
    let y = [0_u8, 16, 64, 128, 235, 255, 32, 96, 160, 210, 245, 128];
    let cb = [128_u8, 128, 16, 240, 0, 255, 64, 192, 128, 48, 208, 128];
    let cr = [128_u8, 128, 240, 16, 255, 0, 192, 64, 128, 208, 48, 128];
    let alpha = [255_u8, 255, 128, 64, 255, 255, 192, 128, 255, 255, 128, 0];
    let source = YuvConverter::new(gpu.device()).convert_native(
        gpu.device(),
        gpu.queue(),
        &YuvPlanes::Yuva444 {
            width: y.len() as u32,
            height: 1,
            y: &y,
            cb: &cb,
            cr: &cr,
            a: &alpha,
        },
        NativeYuvInput::Bt709Scene,
        Range::Limited,
        NativeChromaLocation::Center,
    );
    let before = read_texture_rgba16f(&gpu, &source, y.len() as u32, 1);
    assert!(before.iter().any(|pixel| pixel[0] < 0.0));
    assert!(before.iter().any(|pixel| pixel[0] > 1.0));
    let pass = NativeAces2SdrPass::new(gpu.device()).unwrap();
    let output = pass.apply(gpu.device(), gpu.queue(), &source);
    let got = read_texture_rgba16f(&gpu, &output, y.len() as u32, 1);
    let reference = Aces2SdrOutput::shared().unwrap();
    for (index, (input, actual)) in before.iter().zip(&got).enumerate() {
        let expected = reference.map_premultiplied(input.map(f64::from)).unwrap();
        for channel in 0..4 {
            assert!(
                (f64::from(actual[channel]) - expected[channel]).abs() < 0.01,
                "pixel {index} channel {channel}: GPU {:?}, scalar {:?}, input {:?}",
                actual,
                expected,
                input
            );
        }
    }
}

#[test]
fn isolated_gpu_output_matches_scalar_on_float_ap1_reference_vectors() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping float AP1 ACES 2 SDR parity");
        return;
    };
    let samples: [([f32; 3], f32); 16] = [
        ([0.0, 0.0, 0.0], 1.0),
        ([0.18, 0.18, 0.18], 1.0),
        ([1.0, 1.0, 1.0], 1.0),
        ([1.0, 0.0, 0.0], 1.0),
        ([0.0, 1.0, 0.0], 1.0),
        ([0.0, 0.0, 1.0], 1.0),
        ([0.0, 1.0, 1.0], 1.0),
        ([1.0, 0.0, 1.0], 1.0),
        ([1.0, 1.0, 0.0], 1.0),
        ([0.5, 0.25, 0.15], 0.5),
        ([4.0, 0.3, 0.1], 1.0),
        ([-0.1, 0.2, 1.5], 1.0),
        ([0.18, 4.0, 0.1], 1.0),
        ([8.0, 0.1, 0.01], 1.0),
        ([0.02, 0.005, 0.001], 0.25),
        ([0.0, 0.0, 0.0], 0.0),
    ];
    let pixels: Vec<[f32; 4]> = samples
        .into_iter()
        .map(|(rgb, alpha)| [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha])
        .collect();
    let input = gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("native_aces2_float_reference_input"),
        size: wgpu::Extent3d {
            width: pixels.len() as u32,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue().write_texture(
        input.as_image_copy(),
        bytemuck::cast_slice(&pixels),
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(pixels.len() as u32 * 16),
            rows_per_image: Some(1),
        },
        input.size(),
    );
    let output =
        NativeAces2SdrPass::new(gpu.device())
            .unwrap()
            .apply(gpu.device(), gpu.queue(), &input);
    let got = read_texture_rgba16f(&gpu, &output, pixels.len() as u32, 1);
    let reference = Aces2SdrOutput::shared().unwrap();
    for (index, (input, actual)) in pixels.iter().zip(&got).enumerate() {
        let expected = reference.map_premultiplied(input.map(f64::from)).unwrap();
        for channel in 0..4 {
            assert!(
                (f64::from(actual[channel]) - expected[channel]).abs() < 0.01,
                "pixel {index} channel {channel}: GPU {:?}, scalar {:?}, input {:?}",
                actual,
                expected,
                input
            );
        }
    }
}

#[test]
fn isolated_gpu_output_matches_independent_aces_two_reference_corpus() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping independent ACES 2 SDR corpus");
        return;
    };
    let mut pixels = Vec::<[f32; 4]>::new();
    let mut expected = Vec::<[f32; 3]>::new();
    for fixture in [
        include_str!("../../../tests/fixtures/aces2_sdr_100nit_4cube.tsv"),
        include_str!("../../../tests/fixtures/aces2_sdr_100nit_edges.tsv"),
    ] {
        for line in fixture.lines().filter(|line| !line.starts_with('#')) {
            let values: Vec<f32> = line
                .split_whitespace()
                .map(|value| value.parse().unwrap())
                .collect();
            assert_eq!(values.len(), 6);
            pixels.push([values[0], values[1], values[2], 1.0]);
            expected.push([values[3], values[4], values[5]]);
        }
    }
    assert_eq!(pixels.len(), 96);
    let input = gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("native_aces2_reference_corpus_input"),
        size: wgpu::Extent3d {
            width: pixels.len() as u32,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue().write_texture(
        input.as_image_copy(),
        bytemuck::cast_slice(&pixels),
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(pixels.len() as u32 * 16),
            rows_per_image: Some(1),
        },
        input.size(),
    );
    let output =
        NativeAces2SdrPass::new(gpu.device())
            .unwrap()
            .apply(gpu.device(), gpu.queue(), &input);
    let actual = read_texture_rgba16f(&gpu, &output, pixels.len() as u32, 1);
    let mut worst = (0.0, 0, 0);
    for (pixel, (got, reference)) in actual.iter().zip(&expected).enumerate() {
        for channel in 0..3 {
            let error = (got[channel] - reference[channel]).abs();
            if error > worst.0 {
                worst = (error, pixel, channel);
            }
        }
    }
    assert!(
        worst.0 < 0.005,
        "worst GPU error {} at pixel {} channel {}: input {:?}, output {:?}, reference {:?}",
        worst.0,
        worst.1,
        worst.2,
        pixels[worst.1],
        actual[worst.1],
        expected[worst.1]
    );
}

#[test]
fn isolated_gpu_output_matches_scalar_across_a_two_dimensional_image() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping ACES 2 SDR image parity");
        return;
    };
    // Odd dimensions exercise the padded readback path and the right/bottom edges.
    let (width, height) = (37_u32, 19_u32);
    let mut pixels = Vec::with_capacity((width * height) as usize);
    for row in 0..height {
        for column in 0..width {
            let x = column as f32 / (width - 1) as f32;
            let y = row as f32 / (height - 1) as f32;
            let alpha = if (row + column) % 17 == 0 {
                0.0
            } else {
                [0.125, 0.5, 1.0][(row as usize + column as usize) % 3]
            };
            let rgb = [
                (x * 10.0 - 0.2) * alpha,
                (y * 4.0 - 0.1) * alpha,
                ((1.0 - x) * (1.0 - y) * 2.0) * alpha,
            ];
            pixels.push([rgb[0], rgb[1], rgb[2], alpha]);
        }
    }
    let input = gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("native_aces2_image_parity_input"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue().write_texture(
        input.as_image_copy(),
        bytemuck::cast_slice(&pixels),
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(width * 16),
            rows_per_image: Some(height),
        },
        input.size(),
    );
    let output = gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("native_aces2_reusable_image_output"),
        size: input.size(),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let mut encoder = gpu.device().create_command_encoder(&Default::default());
    NativeAces2SdrPass::new(gpu.device()).unwrap().encode_into(
        gpu.device(),
        &mut encoder,
        &input,
        &output,
    );
    gpu.queue().submit([encoder.finish()]);
    let actual = read_texture_rgba16f(&gpu, &output, width, height);
    assert_eq!(actual.len(), pixels.len());
    let reference = Aces2SdrOutput::shared().unwrap();
    let mut worst = (0.0_f64, 0_usize, 0_usize);
    for (index, (input, got)) in pixels.iter().zip(&actual).enumerate() {
        let expected = reference.map_premultiplied(input.map(f64::from)).unwrap();
        for channel in 0..4 {
            let error = (f64::from(got[channel]) - expected[channel]).abs();
            if error > worst.0 {
                worst = (error, index, channel);
            }
        }
    }
    assert!(
        worst.0 < 0.01,
        "worst GPU/scalar error {} at ({}, {}) channel {}: input {:?}, output {:?}",
        worst.0,
        worst.1 % width as usize,
        worst.1 / width as usize,
        worst.2,
        pixels[worst.1],
        actual[worst.1]
    );
}

#[test]
fn native_high_depth_yuv_exposure_and_sdr_output_preserve_image_positions() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping native high-depth output chain");
        return;
    };
    let (width, height) = (17_u32, 9_u32);
    let mut y = Vec::new();
    let mut cb = Vec::new();
    let mut cr = Vec::new();
    let mut alpha = Vec::new();
    for row in 0..height {
        for column in 0..width {
            let luma = 4096 + 56064 * column / (width - 1);
            let blue = 32768 + 12000 * row / (height - 1);
            let red = 32768 - 12000 * row / (height - 1);
            for (plane, value) in [
                (&mut y, luma),
                (&mut cb, blue),
                (&mut cr, red),
                (&mut alpha, if column % 5 == 0 { 32768 } else { 65535 }),
            ] {
                plane.extend_from_slice(&(value as u16).to_le_bytes());
            }
        }
    }
    let source = YuvConverter::new(gpu.device()).convert_native(
        gpu.device(),
        gpu.queue(),
        &YuvPlanes::Yuv444P16 {
            width,
            height,
            y: &y,
            cb: &cb,
            cr: &cr,
            a: Some(&alpha),
        },
        NativeYuvInput::Bt709Scene,
        Range::Limited,
        NativeChromaLocation::Center,
    );
    let exposed = NativeExposurePass::new(gpu.device())
        .apply(gpu.device(), gpu.queue(), &source, 1.25)
        .unwrap();
    let before_output = read_texture_rgba16f(&gpu, &exposed, width, height);
    let output =
        NativeAces2SdrPass::new(gpu.device())
            .unwrap()
            .apply(gpu.device(), gpu.queue(), &exposed);
    let actual = read_texture_rgba16f(&gpu, &output, width, height);
    let scalar = Aces2SdrOutput::shared().unwrap();
    let mut worst = (0.0_f64, 0_usize, 0_usize);
    for (index, (input, got)) in before_output.iter().zip(&actual).enumerate() {
        let expected = scalar.map_premultiplied(input.map(f64::from)).unwrap();
        for channel in 0..4 {
            let error = (f64::from(got[channel]) - expected[channel]).abs();
            if error > worst.0 {
                worst = (error, index, channel);
            }
        }
    }
    assert!(
        worst.0 < 0.01,
        "high-depth chain error {} at ({}, {}) channel {}: input {:?}, output {:?}",
        worst.0,
        worst.1 % width as usize,
        worst.1 / width as usize,
        worst.2,
        before_output[worst.1],
        actual[worst.1]
    );
}
