//! Native grading-domain transfer parity; the managed graph remains gated.
use photonic_core::timeline::color::NativeOutputTransform;
use photonic_render::{
    color::Range,
    native_display::NativeSrgbEncodePass,
    native_transfer::{AcescctDirection, NativeAcescctPass, NativeExposurePass},
    video::{NativeChromaLocation, NativeYuvInput, YuvConverter, YuvPlanes},
};
use photonic_video::{
    color::{
        acescct,
        native_output::{encode_display, Aces2SdrOutput, PhotonicSdrOutput},
    },
    graph::eval::{read_texture_rgba16f, GpuContext},
};

#[test]
fn acescct_gpu_matches_scalar_reference_on_extended_premultiplied_samples() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping native ACEScct parity");
        return;
    };
    let y = [0_u8, 16, 128, 235, 255, 128];
    let cb = [128_u8; 6];
    let cr = [128_u8; 6];
    let a = [255_u8, 255, 128, 255, 255, 0];
    let planes = YuvPlanes::Yuva444 {
        width: 6,
        height: 1,
        y: &y,
        cb: &cb,
        cr: &cr,
        a: &a,
    };
    let linear = YuvConverter::new(gpu.device()).convert_native(
        gpu.device(),
        gpu.queue(),
        &planes,
        NativeYuvInput::Bt709Scene,
        Range::Limited,
        NativeChromaLocation::Center,
    );
    let before = read_texture_rgba16f(&gpu, &linear, 6, 1);
    assert!(before[0][0] < 0.0);
    assert!(before[4][0] > 1.0);
    let transfer = NativeAcescctPass::new(gpu.device());
    let encoded = transfer.apply(gpu.device(), gpu.queue(), &linear, AcescctDirection::Encode);
    let encoded_pixels = read_texture_rgba16f(&gpu, &encoded, 6, 1);
    for (index, (source, got)) in before.iter().zip(&encoded_pixels).enumerate() {
        let alpha = f64::from(source[3]);
        if alpha == 0.0 {
            assert_eq!(*got, [0.0; 4]);
            continue;
        }
        for channel in 0..3 {
            let expected = acescct::encode(f64::from(source[channel]) / alpha) * alpha;
            assert!(
                (f64::from(got[channel]) - expected).abs() < 0.001,
                "encoded sample {index} channel {channel}: got {}, expected {expected}",
                got[channel]
            );
        }
        assert_eq!(got[3], source[3]);
    }
    let decoded = transfer.apply(
        gpu.device(),
        gpu.queue(),
        &encoded,
        AcescctDirection::Decode,
    );
    let decoded_pixels = read_texture_rgba16f(&gpu, &decoded, 6, 1);
    for (index, (source, got)) in encoded_pixels.iter().zip(&decoded_pixels).enumerate() {
        let alpha = f64::from(source[3]);
        if alpha == 0.0 {
            assert_eq!(*got, [0.0; 4]);
            continue;
        }
        for channel in 0..3 {
            let expected = acescct::decode(f64::from(source[channel]) / alpha) * alpha;
            assert!(
                (f64::from(got[channel]) - expected).abs() < 0.003,
                "decoded sample {index} channel {channel}: got {}, expected {expected}",
                got[channel]
            );
        }
        assert_eq!(got[3], source[3]);
    }
}

#[test]
fn scene_linear_exposure_matches_scalar_and_preserves_alpha() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping native exposure parity");
        return;
    };
    let y = [0_u8, 128, 235, 255];
    let c = [128_u8; 4];
    let alpha = [255_u8, 128, 255, 0];
    let source = YuvConverter::new(gpu.device()).convert_native(
        gpu.device(),
        gpu.queue(),
        &YuvPlanes::Yuva444 {
            width: 4,
            height: 1,
            y: &y,
            cb: &c,
            cr: &c,
            a: &alpha,
        },
        NativeYuvInput::Bt709Scene,
        Range::Limited,
        NativeChromaLocation::Center,
    );
    let before = read_texture_rgba16f(&gpu, &source, 4, 1);
    let exposure = NativeExposurePass::new(gpu.device());
    for stops in [-2.0_f32, 0.0, 1.5] {
        let output = exposure
            .apply(gpu.device(), gpu.queue(), &source, stops)
            .unwrap();
        for (index, (input, got)) in before
            .iter()
            .zip(read_texture_rgba16f(&gpu, &output, 4, 1))
            .enumerate()
        {
            let expected = photonic_video::color::native::expose_acescg(
                [input[0], input[1], input[2]].map(f64::from),
                f64::from(stops),
            );
            for channel in 0..3 {
                assert!(
                    (f64::from(got[channel]) - expected[channel]).abs() < 0.003,
                    "stops {stops}, sample {index}, channel {channel}"
                );
            }
            assert_eq!(got[3], input[3]);
        }
    }
    let exposed = exposure
        .apply(gpu.device(), gpu.queue(), &source, 1.5)
        .unwrap();
    let encoded = NativeAcescctPass::new(gpu.device()).apply(
        gpu.device(),
        gpu.queue(),
        &exposed,
        AcescctDirection::Encode,
    );
    for (input, got) in before
        .iter()
        .zip(read_texture_rgba16f(&gpu, &encoded, 4, 1))
    {
        let alpha = f64::from(input[3]);
        if alpha == 0.0 {
            assert_eq!(got, [0.0; 4]);
            continue;
        }
        for channel in 0..3 {
            let linear = f64::from(input[channel]) / alpha;
            let exposed = photonic_video::color::native::expose_acescg([linear; 3], 1.5)[0];
            let expected = acescct::encode(exposed) * alpha;
            assert!((f64::from(got[channel]) - expected).abs() < 0.002);
        }
    }
    assert!(exposure
        .apply(gpu.device(), gpu.queue(), &source, f32::NAN)
        .is_err());
    assert!(exposure
        .apply(gpu.device(), gpu.queue(), &source, 33.0)
        .is_err());
}

#[test]
fn native_source_exposure_and_scalar_sdr_output_keep_neutral_alpha() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping native source-to-output reference");
        return;
    };
    let y = [16_u8, 128, 235, 128];
    let chroma = [128_u8; 4];
    let alpha = [255_u8, 128, 255, 0];
    let source = YuvConverter::new(gpu.device()).convert_native(
        gpu.device(),
        gpu.queue(),
        &YuvPlanes::Yuva444 {
            width: 4,
            height: 1,
            y: &y,
            cb: &chroma,
            cr: &chroma,
            a: &alpha,
        },
        NativeYuvInput::Bt709Scene,
        Range::Limited,
        NativeChromaLocation::Center,
    );
    let exposed = NativeExposurePass::new(gpu.device())
        .apply(gpu.device(), gpu.queue(), &source, 1.0)
        .unwrap();
    let pixels = read_texture_rgba16f(&gpu, &exposed, 4, 1);
    let output = PhotonicSdrOutput::shared().unwrap();
    let aces2_output = Aces2SdrOutput::shared().unwrap();
    for (index, pixel) in pixels.into_iter().enumerate() {
        let coverage = f64::from(pixel[3]);
        if coverage == 0.0 {
            assert_eq!(pixel, [0.0; 4]);
            assert_eq!(
                output.map_premultiplied(pixel.map(f64::from)).unwrap(),
                [0.0; 4]
            );
            assert_eq!(
                aces2_output
                    .map_premultiplied(pixel.map(f64::from))
                    .unwrap(),
                [0.0; 4]
            );
            continue;
        }
        let encoded_premultiplied = output.map_premultiplied(pixel.map(f64::from)).unwrap();
        assert_eq!(encoded_premultiplied[3], coverage);
        let encoded = [
            encoded_premultiplied[0] / coverage,
            encoded_premultiplied[1] / coverage,
            encoded_premultiplied[2] / coverage,
        ];
        assert!(encoded
            .iter()
            .all(|value| value.is_finite() && (0.0..=1.0).contains(value)));
        assert!(
            (encoded[0] - encoded[1]).abs() < 0.005 && (encoded[1] - encoded[2]).abs() < 0.005,
            "neutral sample {index} shifted in output: {encoded:?}"
        );
        if index == 1 {
            assert!((coverage - 128.0 / 255.0).abs() < 0.001);
        }
        let aces2 = aces2_output
            .map_premultiplied(pixel.map(f64::from))
            .unwrap();
        assert_eq!(aces2[3], coverage);
        assert!(aces2[..3]
            .iter()
            .all(|value| value.is_finite() && (0.0..=coverage).contains(value)));
    }
}

#[test]
fn gpu_display_encoding_matches_scalar_on_premultiplied_values() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping native display-encoding parity");
        return;
    };
    let luma = [0_u8, 16, 64, 128, 235, 255];
    let cb = [128_u8, 128, 16, 240, 0, 255];
    let cr = [128_u8, 128, 240, 16, 255, 0];
    let alpha = [255_u8, 255, 128, 64, 255, 0];
    // Numeric pass parity: this input supplies extended premultiplied values.
    // A qualified rendering transform must produce Rec.709 values in practice.
    let source = YuvConverter::new(gpu.device()).convert_native(
        gpu.device(),
        gpu.queue(),
        &YuvPlanes::Yuva444 {
            width: 6,
            height: 1,
            y: &luma,
            cb: &cb,
            cr: &cr,
            a: &alpha,
        },
        NativeYuvInput::Bt709Scene,
        Range::Limited,
        NativeChromaLocation::Center,
    );
    let before = read_texture_rgba16f(&gpu, &source, 6, 1);
    assert!(before[2][0] != before[2][1] || before[2][1] != before[2][2]);
    let encoded = NativeSrgbEncodePass::new(gpu.device()).apply(gpu.device(), gpu.queue(), &source);
    for (index, (input, got)) in before
        .iter()
        .zip(read_texture_rgba16f(&gpu, &encoded, 6, 1))
        .enumerate()
    {
        let coverage = f64::from(input[3]);
        if coverage == 0.0 {
            assert_eq!(got, [0.0; 4]);
            continue;
        }
        let straight = [input[0], input[1], input[2]].map(|value| f64::from(value) / coverage);
        let expected = encode_display(NativeOutputTransform::SrgbSdr, straight).unwrap();
        for channel in 0..3 {
            assert!(
                (f64::from(got[channel]) - expected[channel] * coverage).abs() < 0.001,
                "sample {index} channel {channel}: {:?} vs {expected:?}",
                got
            );
        }
        assert_eq!(got[3], input[3]);
    }
}

#[test]
fn native_normalized_white_balance_matches_independent_scene_gains() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping native normalized white-balance parity");
        return;
    };
    let source = YuvConverter::new(gpu.device()).convert_native(
        gpu.device(),
        gpu.queue(),
        &YuvPlanes::Yuva444 {
            width: 6,
            height: 1,
            y: &[0, 128, 235, 255, 128, 128],
            cb: &[128, 16, 240, 128, 128, 240],
            cr: &[128, 240, 16, 128, 128, 16],
            a: &[255, 255, 128, 255, 64, 0],
        },
        NativeYuvInput::Bt709Scene,
        Range::Limited,
        NativeChromaLocation::Center,
    );
    let before = read_texture_rgba16f(&gpu, &source, 6, 1);
    assert!(before[0][0] < 0.0 && before[3][0] > 1.0);
    let pass = NativeExposurePass::new(gpu.device());
    for (temp, tint) in [(-1.0, -1.0), (1.0, 1.0), (0.2, -0.3), (0.0, 0.0)] {
        let points =
            photonic_render::native_transfer::white_balance_printer_points(temp, tint).unwrap();
        let target = pass.apply(gpu.device(), gpu.queue(), &source, 0.0).unwrap();
        pass.apply_printer_into(gpu.device(), gpu.queue(), &source, &target, points)
            .unwrap();
        let gains = [
            1.0 + 0.4 * f64::from(temp),
            1.0 - 0.2 * f64::from(tint),
            1.0 - 0.4 * f64::from(temp),
        ];
        for (input, got) in before.iter().zip(read_texture_rgba16f(&gpu, &target, 6, 1)) {
            assert_eq!(got[3], input[3]);
            for channel in 0..3 {
                let expected = f64::from(input[channel]) * gains[channel];
                assert!(
                    (f64::from(got[channel]) - expected).abs() < 0.002 * expected.abs().max(1.0),
                    "{temp}/{tint}: {got:?} versus {input:?} with {gains:?}"
                );
            }
        }
    }
    for (temp, tint) in [
        (1.1, 0.0),
        (0.0, -1.1),
        (f32::NAN, 0.0),
        (0.0, f32::INFINITY),
    ] {
        assert!(
            photonic_render::native_transfer::white_balance_printer_points(temp, tint).is_err()
        );
    }
}

#[test]
fn native_qualifier_matte_uses_scene_input_window_soft_key_and_coverage() {
    use photonic_core::timeline::WindowShape;
    use photonic_render::grade::{ResolvedHslQualifier, ResolvedMask};
    let gpu = GpuContext::request_blocking().expect("native key matte qualification requires GPU");
    // Eight physical pixels with a four-pixel logical canvas exercise padding.
    let source = YuvConverter::new(gpu.device()).convert_native(
        gpu.device(),
        gpu.queue(),
        &YuvPlanes::Yuva444 {
            width: 8,
            height: 1,
            y: &[128; 8],
            cb: &[128; 8],
            cr: &[128; 8],
            a: &[255, 128, 255, 0, 255, 255, 255, 255],
        },
        NativeYuvInput::Bt709Scene,
        Range::Limited,
        NativeChromaLocation::Center,
    );
    let corrected_input = NativeExposurePass::new(gpu.device())
        .apply(gpu.device(), gpu.queue(), &source, 1.0)
        .unwrap();
    let q = ResolvedHslQualifier {
        lum: [0.55, 0.8],
        softness: 0.1,
        matte_levels: [0.2, 0.1],
        ..Default::default()
    };
    let mask = ResolvedMask {
        shape: WindowShape::Rectangle,
        center: [0.5; 2],
        size: [0.2, 1.0],
        rotation: 0.0,
        softness: 0.0,
        invert: false,
    };
    let render = |q: &ResolvedHslQualifier, input| {
        photonic_render::grade_gpu::native_qualifier_matte_gpu(
            gpu.device(),
            gpu.queue(),
            input,
            q,
            Some(&mask),
            (4, 1),
        )
        .unwrap()
    };
    let matte = render(&q, &corrected_input);
    let pixels = read_texture_rgba16f(&gpu, &matte, 4, 1);
    let inputs = read_texture_rgba16f(&gpu, &corrected_input, 4, 1);
    let encode = |v: f64| {
        if v <= 0.0078125 {
            v * 10.5402377416545 + 0.0729055341958355
        } else {
            (v.log2() + 9.72) / 17.52
        }
    };
    let smooth = |lo: f64, hi: f64, value: f64| {
        let t = ((value - lo) / (hi - lo)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    for (i, (input, pixel)) in inputs.iter().zip(&pixels).enumerate() {
        let a = f64::from(input[3]);
        let expected = if a == 0.0 || i == 0 || i == 3 {
            0.0
        } else {
            let log =
                [input[0], input[1], input[2]].map(|v| encode(f64::from(v) / a).clamp(0.0, 1.0));
            let lightness = (log.into_iter().fold(f64::NEG_INFINITY, f64::max)
                + log.into_iter().fold(f64::INFINITY, f64::min))
                / 2.0;
            let gate = smooth(0.45, 0.55, lightness) * (1.0 - smooth(0.8, 0.9, lightness));
            ((gate - 0.2) / 0.7).clamp(0.0, 1.0) * a
        };
        for c in 0..3 {
            assert!(
                (f64::from(pixel[c]) - expected).abs() < 0.007,
                "sample {i}: {pixel:?} vs {expected}"
            );
        }
        assert_eq!(pixel[3], 1.0);
    }
    assert!(pixels[1][0] > 0.05 && pixels[2][0] > pixels[1][0]);
    let earlier = read_texture_rgba16f(&gpu, &render(&q, &source), 4, 1);
    assert!(
        pixels[2][0] > earlier[2][0] + 0.1,
        "earlier exposure must influence the selected key"
    );
    let mut different_cdl = q;
    different_cdl.correction.offset = [0.4, -0.2, 0.1];
    assert_eq!(
        pixels,
        read_texture_rgba16f(&gpu, &render(&different_cdl, &corrected_input), 4, 1)
    );
}
