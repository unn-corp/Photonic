//! Isolated native source transform parity; managed sequences remain gated.
use photonic_core::timeline::color::{
    InputMatrix, InputSignalRange, NativeChromaLocation as IntentChromaLocation,
    NativeInputColorInterpretation, NativeInputStandard,
};
use photonic_core::timeline::Tick;
use photonic_render::{
    color::{Colorimetry, Matrix, Range},
    video::{NativeChromaLocation, NativeYuvInput, YuvConverter, YuvPlanes},
};
use photonic_video::{
    color::native::{decode_video_sample_to_ap1, NativeVideoInput},
    decode::{DecodedFrame, DecodedPlanes},
    graph::eval::{read_texture_rgba16f, GpuContext},
};

#[test]
fn native_gpu_source_rejects_inherited_or_mismatched_metadata() {
    let mut input = NativeInputColorInterpretation {
        hlg_peak_nits: None,
        reference_white_nits: None,
        version: 1,
        standard: NativeInputStandard::Bt2020Scene,
        range: InputSignalRange::FromMetadata,
        matrix: InputMatrix::Bt2020NonConstant,
        chroma_location: None,
    };
    assert!(photonic_video::color::native_gpu_source(&input, true).is_err());
    input.range = InputSignalRange::Limited;
    input.matrix = InputMatrix::Bt709;
    assert!(photonic_video::color::native_gpu_source(&input, true).is_err());
    input.matrix = InputMatrix::Bt2020NonConstant;
    input.version = 2;
    assert!(photonic_video::color::native_gpu_source(&input, true).is_err());
    input.version = 1;
    assert!(photonic_video::color::native_gpu_source(&input, true).is_err());
}

#[test]
fn native_420_chroma_siting_changes_spatial_reconstruction() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping native 4:2:0 siting parity");
        return;
    };
    let converter = YuvConverter::new(gpu.device());
    let y = [128_u8; 16];
    let cr = [128_u8; 4];
    let horizontal_cb = [16_u8, 240, 16, 240];
    let horizontal = YuvPlanes::Yuv420 {
        width: 4,
        height: 4,
        y: &y,
        cb: &horizontal_cb,
        cr: &cr,
    };
    let sample = |planes: &YuvPlanes<'_>, location| {
        let texture = converter.convert_native(
            gpu.device(),
            gpu.queue(),
            planes,
            NativeYuvInput::Bt709Scene,
            Range::Limited,
            location,
        );
        read_texture_rgba16f(&gpu, &texture, 4, 4)
    };
    let center = sample(&horizontal, NativeChromaLocation::Center);
    let left = sample(&horizontal, NativeChromaLocation::Left);
    assert!(left[1][2] > center[1][2]);
    assert!(left[2][2] > center[2][2]);

    let vertical_cb = [16_u8, 16, 240, 240];
    let vertical = YuvPlanes::Yuv420 {
        width: 4,
        height: 4,
        y: &y,
        cb: &vertical_cb,
        cr: &cr,
    };
    let center = sample(&vertical, NativeChromaLocation::Center);
    let top = sample(&vertical, NativeChromaLocation::Top);
    assert!(top[4][2] > center[4][2]);

    // An odd luma width has ceil(width/2) chroma columns; sampling by luma
    // normalized UV would place their centers incorrectly.
    let odd_y = [128_u8; 15];
    let odd_cb = [16_u8, 128, 240, 16, 128, 240];
    let odd_cr = [128_u8; 6];
    let odd = YuvPlanes::Yuv420 {
        width: 5,
        height: 3,
        y: &odd_y,
        cb: &odd_cb,
        cr: &odd_cr,
    };
    for (location, expected_cb) in [
        (NativeChromaLocation::Center, 44_u16),
        (NativeChromaLocation::Left, 72_u16),
    ] {
        let texture = converter.convert_native(
            gpu.device(),
            gpu.queue(),
            &odd,
            NativeYuvInput::Bt709Scene,
            Range::Limited,
            location,
        );
        let got = read_texture_rgba16f(&gpu, &texture, 5, 3)[6];
        let expected =
            decode_video_sample_to_ap1([128, expected_cb, 128], 8, true, NativeVideoInput::Bt709)
                .unwrap();
        for channel in 0..3 {
            assert!(
                (f64::from(got[channel]) - expected[channel]).abs() < 0.004,
                "odd 4:2:0 {location:?} channel {channel}: got {}, expected {}",
                got[channel],
                expected[channel]
            );
        }
    }
}

#[test]
fn native_sixteen_bit_420_preserves_odd_width_chroma_siting() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping native 16-bit 4:2:0 siting parity");
        return;
    };
    let packed =
        |codes: &[u16]| -> Vec<u8> { codes.iter().flat_map(|code| code.to_le_bytes()).collect() };
    let y = packed(&[128_u16 * 257; 15]);
    let cb = packed(&[
        16_u16 * 257,
        128 * 257,
        240 * 257,
        240 * 257,
        128 * 257,
        16 * 257,
    ]);
    let cr = packed(&[128_u16 * 257; 6]);
    let planes = YuvPlanes::Yuv420P16 {
        width: 5,
        height: 3,
        y: &y,
        cb: &cb,
        cr: &cr,
    };
    let converter = YuvConverter::new(gpu.device());
    let frame = DecodedFrame {
        pts: Tick::ZERO,
        planes: DecodedPlanes::yuv420p16(
            5,
            3,
            [y.as_slice(), cb.as_slice(), cr.as_slice()].concat(),
        ),
    };
    let mut intent = NativeInputColorInterpretation {
        hlg_peak_nits: None,
        reference_white_nits: None,
        version: 1,
        standard: NativeInputStandard::Bt709Scene,
        range: InputSignalRange::Limited,
        matrix: InputMatrix::Bt709,
        chroma_location: None,
    };
    assert!(photonic_video::color::convert_native_decoded_frame(
        gpu.device(),
        gpu.queue(),
        &converter,
        &frame,
        &intent,
    )
    .is_err());
    intent.chroma_location = Some(IntentChromaLocation::Left);
    let bridged = photonic_video::color::convert_native_decoded_frame(
        gpu.device(),
        gpu.queue(),
        &converter,
        &frame,
        &intent,
    )
    .unwrap();
    let padded = photonic_video::color::convert_native_decoded_frame_to_size(
        gpu.device(),
        gpu.queue(),
        &converter,
        &frame,
        &intent,
        (8, 4),
    )
    .unwrap();
    let padded_pixels = read_texture_rgba16f(&gpu, &padded, 8, 4);
    assert_eq!(
        padded_pixels[9],
        read_texture_rgba16f(&gpu, &bridged, 5, 3)[6]
    );
    assert_eq!(padded_pixels[31], [0.0; 4]);
    for (location, expected_cb) in [
        (NativeChromaLocation::Center, 86_u16),
        (NativeChromaLocation::Left, 100_u16),
        (NativeChromaLocation::TopLeft, 128_u16),
        (NativeChromaLocation::Top, 128_u16),
        (NativeChromaLocation::BottomLeft, 72_u16),
        (NativeChromaLocation::Bottom, 44_u16),
    ] {
        let texture = converter.convert_native(
            gpu.device(),
            gpu.queue(),
            &planes,
            NativeYuvInput::Bt709Scene,
            Range::Limited,
            location,
        );
        let got = read_texture_rgba16f(&gpu, &texture, 5, 3)[6];
        if location == NativeChromaLocation::Left {
            assert_eq!(got, read_texture_rgba16f(&gpu, &bridged, 5, 3)[6]);
        }
        let expected = decode_video_sample_to_ap1(
            [128 * 257, expected_cb * 257, 128 * 257],
            16,
            true,
            NativeVideoInput::Bt709,
        )
        .unwrap();
        for channel in 0..3 {
            assert!(
                (f64::from(got[channel]) - expected[channel]).abs() < 0.004,
                "16-bit odd 4:2:0 {location:?} channel {channel}: got {}, expected {}",
                got[channel],
                expected[channel]
            );
        }
    }
}

#[test]
fn native_sixteen_bit_420_keeps_code_excursions() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping native 16-bit 4:2:0 excursion parity");
        return;
    };
    let y_codes = [0_u16, 4096, 60160, 65535];
    let y: Vec<u8> = y_codes.iter().flat_map(|code| code.to_le_bytes()).collect();
    let chroma = 32768_u16.to_le_bytes();
    let planes = YuvPlanes::Yuv420P16 {
        width: 2,
        height: 2,
        y: &y,
        cb: &chroma,
        cr: &chroma,
    };
    let texture = YuvConverter::new(gpu.device()).convert_native(
        gpu.device(),
        gpu.queue(),
        &planes,
        NativeYuvInput::Bt709Scene,
        Range::Limited,
        NativeChromaLocation::Center,
    );
    let pixels = read_texture_rgba16f(&gpu, &texture, 2, 2);
    assert!(pixels[0][0] < 0.0);
    assert!(pixels[3][0] > 1.0);
    for (index, y_code) in y_codes.into_iter().enumerate() {
        let expected =
            decode_video_sample_to_ap1([y_code, 32768, 32768], 16, true, NativeVideoInput::Bt709)
                .unwrap();
        for channel in 0..3 {
            assert!(
                (f64::from(pixels[index][channel]) - expected[channel]).abs() < 0.003,
                "16-bit excursion {index} channel {channel}"
            );
        }
    }
}

#[test]
fn native_sixteen_bit_422_preserves_horizontal_siting_and_full_height_chroma() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping native 16-bit 4:2:2 siting parity");
        return;
    };
    let packed =
        |codes: &[u16]| -> Vec<u8> { codes.iter().flat_map(|code| code.to_le_bytes()).collect() };
    let y = packed(&[32768_u16; 15]);
    let cb = packed(&[4096, 4096, 4096, 4100, 32001, 60000, 60000, 60000, 60000]);
    let cr = packed(&[32768_u16; 9]);
    let frame = DecodedFrame {
        pts: Tick::ZERO,
        planes: DecodedPlanes::yuv422p16(
            5,
            3,
            [y.as_slice(), cb.as_slice(), cr.as_slice()].concat(),
        ),
    };
    assert_eq!(frame.planes.cb(), cb);
    let converter = YuvConverter::new(gpu.device());
    let mut intent = NativeInputColorInterpretation {
        hlg_peak_nits: None,
        reference_white_nits: None,
        version: 1,
        standard: NativeInputStandard::Bt709Scene,
        range: InputSignalRange::Limited,
        matrix: InputMatrix::Bt709,
        chroma_location: None,
    };
    assert!(photonic_video::color::convert_native_decoded_frame(
        gpu.device(),
        gpu.queue(),
        &converter,
        &frame,
        &intent
    )
    .is_err());
    for (location, expected_cb) in [
        (IntentChromaLocation::Left, 18050_u16),
        (IntentChromaLocation::Center, 11075_u16),
    ] {
        intent.chroma_location = Some(location);
        let texture = photonic_video::color::convert_native_decoded_frame(
            gpu.device(),
            gpu.queue(),
            &converter,
            &frame,
            &intent,
        )
        .unwrap();
        let got = read_texture_rgba16f(&gpu, &texture, 5, 3)[6];
        let expected = decode_video_sample_to_ap1(
            [32768, expected_cb, 32768],
            16,
            true,
            NativeVideoInput::Bt709,
        )
        .unwrap();
        for channel in 0..3 {
            assert!(
                (f64::from(got[channel]) - expected[channel]).abs() < 0.004,
                "4:2:2 {location:?} channel {channel}: got {}, expected {}",
                got[channel],
                expected[channel]
            );
        }
    }
}

#[test]
fn native_eight_bit_422_preserves_full_height_chroma() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping native 8-bit 4:2:2 siting parity");
        return;
    };
    let y = [128_u8; 15];
    let cb = [16_u8, 16, 16, 16, 128, 240, 240, 240, 240];
    let cr = [128_u8; 9];
    let planes = YuvPlanes::Yuv422 {
        width: 5,
        height: 3,
        y: &y,
        cb: &cb,
        cr: &cr,
    };
    let converter = YuvConverter::new(gpu.device());
    for (location, expected_cb) in [
        (NativeChromaLocation::Left, 72_u16),
        (NativeChromaLocation::Center, 44_u16),
    ] {
        let texture = converter.convert_native(
            gpu.device(),
            gpu.queue(),
            &planes,
            NativeYuvInput::Bt709Scene,
            Range::Limited,
            location,
        );
        let got = read_texture_rgba16f(&gpu, &texture, 5, 3)[6];
        let expected =
            decode_video_sample_to_ap1([128, expected_cb, 128], 8, true, NativeVideoInput::Bt709)
                .unwrap();
        for channel in 0..3 {
            assert!(
                (f64::from(got[channel]) - expected[channel]).abs() < 0.004,
                "8-bit 4:2:2 {location:?} channel {channel}: got {}, expected {}",
                got[channel],
                expected[channel]
            );
        }
    }
}

#[test]
fn native_eight_bit_444_preserves_each_chroma_sample() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping native 8-bit 4:4:4 parity");
        return;
    };
    let y = [128_u8; 4];
    let cb = [16_u8, 64, 192, 240];
    let cr = [128_u8; 4];
    let texture = YuvConverter::new(gpu.device()).convert_native(
        gpu.device(),
        gpu.queue(),
        &YuvPlanes::Yuv444 {
            width: 2,
            height: 2,
            y: &y,
            cb: &cb,
            cr: &cr,
        },
        NativeYuvInput::Bt709Scene,
        Range::Limited,
        NativeChromaLocation::Center,
    );
    for (index, got) in read_texture_rgba16f(&gpu, &texture, 2, 2)
        .into_iter()
        .enumerate()
    {
        let expected = decode_video_sample_to_ap1(
            [128, cb[index] as u16, 128],
            8,
            true,
            NativeVideoInput::Bt709,
        )
        .unwrap();
        for channel in 0..3 {
            assert!(
                (f64::from(got[channel]) - expected[channel]).abs() < 0.004,
                "8-bit 4:4:4 sample {index} channel {channel}"
            );
        }
    }
}

#[test]
fn native_yuv_gpu_matches_scalar_reference() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping native source parity");
        return;
    };
    let converter = YuvConverter::new(gpu.device());
    let samples = [
        [16_u8, 128, 128],
        [235, 128, 128],
        [63, 102, 240],
        [0, 128, 128],
        [255, 128, 128],
    ];
    let y: Vec<u8> = samples.iter().map(|s| s[0]).collect();
    let cb: Vec<u8> = samples.iter().map(|s| s[1]).collect();
    let cr: Vec<u8> = samples.iter().map(|s| s[2]).collect();
    let a = [255_u8, 128, 255, 255, 255];
    let planes = YuvPlanes::Yuva444 {
        width: samples.len() as u32,
        height: 1,
        y: &y,
        cb: &cb,
        cr: &cr,
        a: &a,
    };
    for (gpu_input, cpu_input) in [
        (NativeYuvInput::Bt709Scene, NativeVideoInput::Bt709),
        (
            NativeYuvInput::Bt2020Scene,
            NativeVideoInput::Bt2020NonConstant,
        ),
    ] {
        for range in [Range::Limited, Range::Full] {
            let input = NativeInputColorInterpretation {
                hlg_peak_nits: None,
                reference_white_nits: None,
                version: 1,
                standard: match gpu_input {
                    NativeYuvInput::Bt709Scene => NativeInputStandard::Bt709Scene,
                    NativeYuvInput::Bt2020Scene => NativeInputStandard::Bt2020Scene,
                    _ => unreachable!(),
                },
                range: match range {
                    Range::Full => InputSignalRange::Full,
                    Range::Limited => InputSignalRange::Limited,
                },
                matrix: match gpu_input {
                    NativeYuvInput::Bt709Scene => InputMatrix::Bt709,
                    NativeYuvInput::Bt2020Scene => InputMatrix::Bt2020NonConstant,
                    _ => unreachable!(),
                },
                chroma_location: None,
            };
            let (selected, selected_range, chroma_location) =
                photonic_video::color::native_gpu_source(&input, false).unwrap();
            assert_eq!((selected, selected_range), (gpu_input, range));
            assert_eq!(chroma_location, NativeChromaLocation::Center);
            let tex = converter.convert_native_to_size(
                gpu.device(),
                gpu.queue(),
                &planes,
                selected,
                selected_range,
                chroma_location,
                (samples.len() as u32 + 2, 1),
            );
            let got = read_texture_rgba16f(&gpu, &tex, samples.len() as u32, 1);
            for (index, codes) in samples.iter().enumerate() {
                let expected = decode_video_sample_to_ap1(
                    codes.map(u16::from),
                    8,
                    matches!(range, Range::Limited),
                    cpu_input,
                )
                .unwrap();
                let alpha = f64::from(a[index]) / 255.0;
                for channel in 0..3 {
                    assert!(
                        (f64::from(got[index][channel]) - expected[channel] * alpha).abs() < 0.005,
                        "{gpu_input:?} {range:?} sample {index} channel {channel}: got {}, expected {}",
                        got[index][channel], expected[channel] * alpha
                    );
                }
                assert!((f64::from(got[index][3]) - alpha).abs() < 0.001);
            }
        }
    }
}

#[test]
fn native_packed_sixteen_bit_upload_matches_scalar_with_half_float_output() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping native 16-bit parity");
        return;
    };
    let samples = [
        [4096_u16, 32768, 32768],
        [4097, 32768, 32768],
        [60160, 32768, 32768],
        [65535, 12000, 51000],
    ];
    let bytes = |channel: usize| -> Vec<u8> {
        samples
            .iter()
            .flat_map(|sample| sample[channel].to_le_bytes())
            .collect()
    };
    let y = bytes(0);
    let cb = bytes(1);
    let cr = bytes(2);
    let alpha_codes = [65535_u16, 65535, 32768, 65535];
    let alpha: Vec<u8> = alpha_codes.into_iter().flat_map(u16::to_le_bytes).collect();
    let planes = YuvPlanes::Yuv444P16 {
        width: samples.len() as u32,
        height: 1,
        y: &y,
        cb: &cb,
        cr: &cr,
        a: Some(&alpha),
    };
    let converter = YuvConverter::new(gpu.device());
    let legacy_color = Colorimetry {
        matrix: Matrix::Bt709,
        range: Range::Limited,
    };
    let legacy_before = converter.convert(gpu.device(), gpu.queue(), &planes, legacy_color);
    let legacy_before = read_texture_rgba16f(&gpu, &legacy_before, samples.len() as u32, 1);
    let tex = converter.convert_native(
        gpu.device(),
        gpu.queue(),
        &planes,
        NativeYuvInput::Bt2020Scene,
        Range::Limited,
        NativeChromaLocation::Center,
    );
    let got = read_texture_rgba16f(&gpu, &tex, samples.len() as u32, 1);
    assert!(
        got[1][0] > got[0][0],
        "adjacent 16-bit luma codes must survive packed upload"
    );
    let legacy_after = converter.convert(gpu.device(), gpu.queue(), &planes, legacy_color);
    assert_eq!(
        legacy_before,
        read_texture_rgba16f(&gpu, &legacy_after, samples.len() as u32, 1),
        "native packed upload must not change Legacy SDR conversion"
    );
    for (index, codes) in samples.iter().enumerate() {
        let expected =
            decode_video_sample_to_ap1(*codes, 16, true, NativeVideoInput::Bt2020NonConstant)
                .unwrap();
        let alpha = f64::from(alpha_codes[index]) / 65535.0;
        for channel in 0..3 {
            assert!(
                (f64::from(got[index][channel]) - expected[channel] * alpha).abs() < 0.002,
                "16-bit sample {index} channel {channel}: got {}, expected {}",
                got[index][channel],
                expected[channel] * alpha
            );
        }
        assert!((f64::from(got[index][3]) - alpha).abs() < 0.001);
    }
}

#[test]
fn native_pq_absolute_luminance_and_packed_gpu_parity() {
    use photonic_video::color::native::decode_pq_nits;
    // Independent published PQ anchors: 100 nits at 0.5080784215; 1000 at 0.7518270962.
    assert_eq!(decode_pq_nits(0.0), 0.0);
    assert!((decode_pq_nits(0.5080784215) - 100.0).abs() < 0.00001);
    assert!((decode_pq_nits(0.7518270962) - 1000.0).abs() < 0.0001);
    assert!((decode_pq_nits(1.0) - 10000.0).abs() < 0.00001);
    assert_eq!(decode_pq_nits(-0.2), 0.0);
    assert_eq!(decode_pq_nits(1.2), decode_pq_nits(1.0));
    let mut intent = NativeInputColorInterpretation {
        hlg_peak_nits: None,
        version: 1,
        standard: NativeInputStandard::Bt2100PqDisplay,
        range: InputSignalRange::Full,
        matrix: InputMatrix::Bt2020NonConstant,
        chroma_location: None,
        reference_white_nits: None,
    };
    assert!(intent.validate().is_err());
    for white in [0, 10001] {
        intent.reference_white_nits = Some(white);
        assert!(intent.validate().is_err());
    }
    intent.reference_white_nits = Some(203);
    assert!(intent.validate().is_ok());
    let encoded = serde_json::to_string(&intent).unwrap();
    assert_eq!(
        serde_json::from_str::<NativeInputColorInterpretation>(&encoded).unwrap(),
        intent
    );
    intent.standard = NativeInputStandard::Bt2020Scene;
    assert!(intent.validate().is_err());
    intent.reference_white_nits = None;
    assert!(!serde_json::to_string(&intent)
        .unwrap()
        .contains("reference_white_nits"));
    let Some(gpu) = GpuContext::request_blocking() else {
        panic!("PQ qualification requires GPU");
    };
    let converter = YuvConverter::new(gpu.device());
    for limited in [false, true] {
        let samples = if limited {
            [
                [4096u16, 32768, 32768],
                [32581, 32768, 32768],
                [46244, 32768, 32768],
                [60160, 32768, 32768],
                [50000, 25000, 45000],
            ]
        } else {
            [
                [0u16, 32768, 32768],
                [33297, 32768, 32768],
                [49271, 32768, 32768],
                [65535, 32768, 32768],
                [50000, 25000, 45000],
            ]
        };
        let bytes = |c: usize| {
            samples
                .iter()
                .flat_map(|s| s[c].to_le_bytes())
                .collect::<Vec<_>>()
        };
        let y = bytes(0);
        let cb = bytes(1);
        let cr = bytes(2);
        let alpha_codes = [65535u16, 32768, 16384, 65535, 45000];
        let alpha = alpha_codes
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let planes = YuvPlanes::Yuv444P16 {
            width: 5,
            height: 1,
            y: &y,
            cb: &cb,
            cr: &cr,
            a: Some(&alpha),
        };
        for white in [100, 203, 1000] {
            let tex = converter.convert_native(
                gpu.device(),
                gpu.queue(),
                &planes,
                NativeYuvInput::Bt2100PqDisplay {
                    reference_white_nits: white,
                },
                if limited { Range::Limited } else { Range::Full },
                NativeChromaLocation::Center,
            );
            let got = read_texture_rgba16f(&gpu, &tex, 5, 1);
            for (i, codes) in samples.into_iter().enumerate() {
                let expected = decode_video_sample_to_ap1(
                    codes,
                    16,
                    limited,
                    NativeVideoInput::Bt2100PqDisplay {
                        reference_white_nits: white,
                    },
                )
                .unwrap();
                let a = f64::from(alpha_codes[i]) / 65535.0;
                for c in 0..3 {
                    let e = expected[c] * a;
                    assert!(
                        (f64::from(got[i][c]) - e).abs() < 0.002 + e.abs() * 0.002,
                        "range {limited} white {white} sample {i} channel {c}: {} vs {e}",
                        got[i][c]
                    );
                }
                assert!((f64::from(got[i][3]) - a).abs() < 0.001);
            }
        }
    }
}

#[test]
fn native_hlg_scene_white_anchor_signed_headroom_and_gpu_parity() {
    use photonic_video::color::native::{decode_hlg_scene, hlg_scene_normalization};
    assert_eq!(decode_hlg_scene(0.0), 0.0);
    assert!((decode_hlg_scene(0.5) - 1.0 / 12.0).abs() < 1e-12);
    assert!((decode_hlg_scene(0.75) - 0.2649625598).abs() < 1e-9);
    assert!((decode_hlg_scene(1.0) - 1.0).abs() < 1e-7);
    assert_eq!(decode_hlg_scene(-0.5), -decode_hlg_scene(0.5));
    assert!(decode_hlg_scene(1.2) > 1.0);
    let scale = hlg_scene_normalization(203, 1000).unwrap();
    assert!((decode_hlg_scene(0.75) * scale - 1.0).abs() < 0.002);
    let mut intent = NativeInputColorInterpretation {
        version: 1,
        standard: NativeInputStandard::Bt2100HlgScene,
        range: InputSignalRange::Limited,
        matrix: InputMatrix::Bt2020NonConstant,
        chroma_location: None,
        reference_white_nits: None,
        hlg_peak_nits: None,
    };
    for (white, peak) in [
        (None, None),
        (Some(203), None),
        (Some(0), Some(1000)),
        (Some(203), Some(399)),
        (Some(203), Some(2001)),
        (Some(1500), Some(1000)),
    ] {
        intent.reference_white_nits = white;
        intent.hlg_peak_nits = peak;
        assert!(intent.validate().is_err());
    }
    intent.reference_white_nits = Some(203);
    intent.hlg_peak_nits = Some(1000);
    assert!(intent.validate().is_ok());
    assert_eq!(
        serde_json::from_str::<NativeInputColorInterpretation>(
            &serde_json::to_string(&intent).unwrap()
        )
        .unwrap(),
        intent
    );
    intent.standard = NativeInputStandard::Bt2100PqDisplay;
    assert!(intent.validate().is_err());
    intent.standard = NativeInputStandard::Bt2020Scene;
    intent.reference_white_nits = None;
    intent.hlg_peak_nits = None;
    let legacy = serde_json::to_string(&intent).unwrap();
    assert!(!legacy.contains("reference_white_nits") && !legacy.contains("hlg_peak_nits"));
    let gpu = GpuContext::request_blocking().expect("HLG qualification requires GPU");
    let converter = YuvConverter::new(gpu.device());
    for limited in [false, true] {
        let samples = if limited {
            [
                [0u16, 32768, 32768],
                [32128, 32768, 32768],
                [46144, 32768, 32768],
                [65535, 32768, 32768],
                [60000, 20000, 50000],
            ]
        } else {
            [
                [0u16, 32768, 32768],
                [32768, 32768, 32768],
                [49151, 32768, 32768],
                [65535, 32768, 32768],
                [60000, 20000, 50000],
            ]
        };
        let bytes = |c: usize| {
            samples
                .iter()
                .flat_map(|s| s[c].to_le_bytes())
                .collect::<Vec<_>>()
        };
        let y = bytes(0);
        let cb = bytes(1);
        let cr = bytes(2);
        let alpha_codes = [65535u16, 32768, 16384, 65535, 45000];
        let alpha = alpha_codes
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let planes = YuvPlanes::Yuv444P16 {
            width: 5,
            height: 1,
            y: &y,
            cb: &cb,
            cr: &cr,
            a: Some(&alpha),
        };
        for (white, peak) in [(100, 400), (203, 1000), (1000, 2000)] {
            let tex = converter.convert_native(
                gpu.device(),
                gpu.queue(),
                &planes,
                NativeYuvInput::Bt2100HlgScene {
                    reference_white_nits: white,
                    display_peak_nits: peak,
                },
                if limited { Range::Limited } else { Range::Full },
                NativeChromaLocation::Center,
            );
            let got = read_texture_rgba16f(&gpu, &tex, 5, 1);
            for (i, codes) in samples.into_iter().enumerate() {
                let expected = decode_video_sample_to_ap1(
                    codes,
                    16,
                    limited,
                    NativeVideoInput::Bt2100HlgScene {
                        reference_white_nits: white,
                        display_peak_nits: peak,
                    },
                )
                .unwrap();
                let a = f64::from(alpha_codes[i]) / 65535.0;
                for c in 0..3 {
                    let e = expected[c] * a;
                    assert!(
                        (f64::from(got[i][c]) - e).abs() < 0.003 + e.abs() * 0.003,
                        "range {limited} white {white} peak {peak} sample {i}: {} vs {e}",
                        got[i][c]
                    );
                }
                assert!((f64::from(got[i][3]) - a).abs() < 0.001);
            }
            if limited {
                assert!(got[0][0] < 0.0);
                assert!(got[3][0] > 1.0);
            }
        }
    }
}
