//! Isolated managed graph operations; unrestricted timeline rendering and export remain gated.

use photonic_core::layer::BlendMode;
use photonic_render::color::Colorimetry;
use photonic_render::native_transfer::AcescctDirection;
use photonic_video::color::native::{decode_bt709_source, decode_ycbcr_codes, YcbcrMatrix};
use photonic_video::color::native_output::Aces2SdrOutput;
use photonic_video::export::convert::{video_signal_frame_to_yuv422p10, EncodePlanes};
use photonic_video::graph::{
    eval::{read_texture_rgba16f, Evaluator, GpuContext, NullFrameSource},
    eval_cpu::{self, EmptyProvider},
    ir::{
        ContentHash, FrameColorEncoding, FrameGraph, IrNode, IrNodeId, IrOp, LinearColor, OutPort,
        Sampling, WorkingColorDomain,
    },
};

#[test]
fn translated_native_shot_reveals_lower_track_at_transparent_edge() {
    let graph = FrameGraph {
        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
        nodes: vec![
            IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: 1.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(101),
            },
            IrNode {
                op: IrOp::Transform2DTransparent {
                    mat: glam::Mat3::from_translation(glam::Vec2::new(1.0, 0.0)),
                    sampling: Sampling::Nearest,
                },
                inputs: vec![(IrNodeId(0), OutPort::default())],
                content_hash: ContentHash(102),
            },
            IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: 0.0,
                        g: 0.0,
                        b: 1.0,
                        a: 1.0,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(103),
            },
            IrNode {
                op: IrOp::Merge {
                    mode: BlendMode::Normal,
                    opacity: 1.0,
                },
                inputs: vec![
                    (IrNodeId(1), OutPort::default()),
                    (IrNodeId(2), OutPort::default()),
                ],
                content_hash: ContentHash(104),
            },
        ],
        output: Some(IrNodeId(3)),
    };
    assert!(graph.validate_working_color_domain().is_ok());
    let cpu = eval_cpu::evaluate(&graph, (2, 1), &mut EmptyProvider);
    assert_eq!(cpu.pixels, vec![[0.0, 0.0, 1.0, 1.0], [1.0, 0.0, 0.0, 1.0]]);
    if let Some(gpu) = GpuContext::request_blocking() {
        let texture = Evaluator::new(gpu.clone())
            .evaluate(&graph, (2, 1), &mut NullFrameSource)
            .expect("native composite should render");
        for (actual, expected) in read_texture_rgba16f(&gpu, &texture, 2, 1)
            .iter()
            .zip(&cpu.pixels)
        {
            for (channel, target) in actual.iter().zip(expected) {
                assert!(
                    (channel - target).abs() < 0.002,
                    "{actual:?} vs {expected:?}"
                );
            }
        }
    }
}

fn graph(stops: f32, domain: WorkingColorDomain) -> FrameGraph {
    FrameGraph {
        working_color_domain: domain,
        nodes: vec![
            IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: 0.25,
                        g: 0.1,
                        b: 0.05,
                        a: 0.5,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(1),
            },
            IrNode {
                op: IrOp::NativeExposure { stops },
                inputs: vec![(IrNodeId(0), OutPort::default())],
                content_hash: ContentHash(2),
            },
        ],
        output: Some(IrNodeId(1)),
    }
}

#[test]
fn managed_normal_composite_preserves_above_white_values() {
    let mut graph = FrameGraph {
        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
        nodes: vec![
            IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: 2.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.5,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(1),
            },
            IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: 0.5,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(2),
            },
            IrNode {
                op: IrOp::Merge {
                    mode: BlendMode::Normal,
                    opacity: 0.5,
                },
                inputs: vec![
                    (IrNodeId(0), OutPort::default()),
                    (IrNodeId(1), OutPort::default()),
                ],
                content_hash: ContentHash(3),
            },
        ],
        output: Some(IrNodeId(2)),
    };
    assert!(graph.validate_working_color_domain().is_ok());
    let cpu = eval_cpu::evaluate(&graph, (2, 2), &mut EmptyProvider);
    assert!(cpu
        .pixels
        .iter()
        .all(|pixel| (pixel[0] - 1.375).abs() < 1e-6 && (pixel[3] - 1.0).abs() < 1e-6));
    if let Some(gpu) = GpuContext::request_blocking() {
        let texture = Evaluator::new(gpu.clone())
            .evaluate(&graph, (2, 2), &mut NullFrameSource)
            .unwrap();
        for pixel in read_texture_rgba16f(&gpu, &texture, 2, 2) {
            assert!(
                (pixel[0] - 1.375).abs() < 0.004 && (pixel[3] - 1.0).abs() < 0.004,
                "{pixel:?}"
            );
        }
    }
    graph.nodes[2].op = IrOp::Merge {
        mode: BlendMode::Color,
        opacity: 0.5,
    };
    assert!(graph.validate_working_color_domain().is_err());
}

#[test]
fn acescg_exposure_is_an_explicit_cpu_gpu_graph_operation() {
    let graph = graph(2.0, WorkingColorDomain::SceneLinearAcescg);
    assert!(graph.validate_working_color_domain().is_ok());
    assert_eq!(
        graph.output_color_encoding(),
        Ok(FrameColorEncoding::SceneLinearAcescg)
    );
    let cpu = eval_cpu::evaluate(&graph, (3, 2), &mut EmptyProvider);
    assert_eq!(cpu.pixels.len(), 6);
    for pixel in &cpu.pixels {
        for (actual, expected) in pixel.iter().zip([1.0, 0.4, 0.2, 0.5]) {
            assert!((actual - expected).abs() < 1e-6);
        }
    }
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("GPU unavailable; skipping managed exposure graph parity");
        return;
    };
    let mut evaluator = Evaluator::new(gpu.clone());
    let texture = evaluator
        .evaluate(&graph, (3, 2), &mut NullFrameSource)
        .expect("qualified managed exposure graph should render");
    let pixels = read_texture_rgba16f(&gpu, &texture, 3, 2);
    for pixel in &pixels {
        for (actual, expected) in pixel.iter().zip([1.0, 0.4, 0.2, 0.5]) {
            assert!((actual - expected).abs() < 0.002, "{pixel:?}");
        }
    }
}

#[test]
fn native_linear_offset_preserves_signed_highlights_and_alpha() {
    let mut graph = graph(0.0, WorkingColorDomain::SceneLinearAcescg);
    graph.nodes.push(IrNode {
        op: IrOp::NativeLinearOffset {
            rgb: [-1.0, 0.5, 2.0],
        },
        inputs: vec![(IrNodeId(1), OutPort::default())],
        content_hash: ContentHash(3),
    });
    graph.output = Some(IrNodeId(2));
    assert!(graph.validate_working_color_domain().is_ok());
    let expected = [-0.25, 0.35, 1.05, 0.5];
    let cpu = eval_cpu::evaluate(&graph, (2, 2), &mut EmptyProvider);
    for pixel in cpu.pixels {
        for (actual, target) in pixel.iter().zip(expected) {
            assert!((actual - target).abs() < 1e-6, "{pixel:?}");
        }
    }
    if let Some(gpu) = GpuContext::request_blocking() {
        let texture = Evaluator::new(gpu.clone())
            .evaluate(&graph, (2, 2), &mut NullFrameSource)
            .expect("native linear offset graph should render");
        for pixel in read_texture_rgba16f(&gpu, &texture, 2, 2) {
            for (actual, target) in pixel.iter().zip(expected) {
                assert!((actual - target).abs() < 0.004, "{pixel:?}");
            }
        }
    }
    graph.working_color_domain = WorkingColorDomain::LegacyLinearRec709;
    assert_eq!(
        graph.validate_working_color_domain(),
        Err("native exposure requires scene-linear ACEScg")
    );
    graph.working_color_domain = WorkingColorDomain::SceneLinearAcescg;
    graph.nodes[2].op = IrOp::NativeLinearOffset {
        rgb: [f32::NAN, 0.0, 0.0],
    };
    assert_eq!(
        graph.validate_working_color_domain(),
        Err("native linear offset must be finite and within -16..=16")
    );
}

#[test]
fn native_printer_lights_apply_per_channel_stops_without_clamping() {
    let mut graph = graph(0.0, WorkingColorDomain::SceneLinearAcescg);
    graph.nodes.push(IrNode {
        op: IrOp::NativePrinterLights {
            points: [12.0, 0.0, -12.0],
        },
        inputs: vec![(IrNodeId(1), OutPort::default())],
        content_hash: ContentHash(3),
    });
    graph.output = Some(IrNodeId(2));
    assert!(graph.validate_working_color_domain().is_ok());
    let expected = [0.5, 0.1, 0.025, 0.5];
    for pixel in eval_cpu::evaluate(&graph, (2, 2), &mut EmptyProvider).pixels {
        for (actual, target) in pixel.iter().zip(expected) {
            assert!((actual - target).abs() < 1e-6, "{pixel:?}");
        }
    }
    if let Some(gpu) = GpuContext::request_blocking() {
        let texture = Evaluator::new(gpu.clone())
            .evaluate(&graph, (2, 2), &mut NullFrameSource)
            .expect("native printer lights graph should render");
        for pixel in read_texture_rgba16f(&gpu, &texture, 2, 2) {
            for (actual, target) in pixel.iter().zip(expected) {
                assert!((actual - target).abs() < 0.004, "{pixel:?}");
            }
        }
    }
    graph.nodes[2].op = IrOp::NativePrinterLights {
        points: [f32::INFINITY, 0.0, 0.0],
    };
    assert_eq!(
        graph.validate_working_color_domain(),
        Err("native printer lights must be finite and within -120..=120 points")
    );
}

#[test]
fn native_highlight_rolloff_preserves_hue_ratios_negative_detail_and_alpha() {
    let mut graph = graph(0.0, WorkingColorDomain::SceneLinearAcescg);
    graph.nodes[0].op = IrOp::SolidColor {
        color: LinearColor {
            r: 2.0,
            g: -0.25,
            b: 0.5,
            a: 0.5,
        },
    };
    graph.nodes.push(IrNode {
        op: IrOp::NativeHighlightRolloff {
            knee: 1.0,
            strength: 1.0,
        },
        inputs: vec![(IrNodeId(1), OutPort::default())],
        content_hash: ContentHash(3),
    });
    graph.output = Some(IrNodeId(2));
    assert!(graph.validate_working_color_domain().is_ok());
    let expected = [0.875, -0.109375, 0.21875, 0.5];
    for pixel in eval_cpu::evaluate(&graph, (2, 2), &mut EmptyProvider).pixels {
        for (actual, target) in pixel.iter().zip(expected) {
            assert!((actual - target).abs() < 1e-6, "{pixel:?}");
        }
    }
    if let Some(gpu) = GpuContext::request_blocking() {
        let texture = Evaluator::new(gpu.clone())
            .evaluate(&graph, (2, 2), &mut NullFrameSource)
            .expect("native highlight roll-off graph should render");
        for pixel in read_texture_rgba16f(&gpu, &texture, 2, 2) {
            for (actual, target) in pixel.iter().zip(expected) {
                assert!((actual - target).abs() < 0.004, "{pixel:?}");
            }
        }
    }
    graph.nodes[2].op = IrOp::NativeHighlightRolloff {
        knee: -1.0,
        strength: 1.0,
    };
    assert_eq!(
        graph.validate_working_color_domain(),
        Err("native highlight roll-off parameters must be finite and within 0..=64")
    );
}

#[test]
fn native_exposure_rejects_legacy_domain_and_invalid_stops() {
    let legacy = graph(1.0, WorkingColorDomain::LegacyLinearRec709);
    assert_eq!(
        legacy.validate_working_color_domain(),
        Err("native exposure requires scene-linear ACEScg")
    );
    assert!(eval_cpu::evaluate(&legacy, (3, 2), &mut EmptyProvider)
        .pixels
        .iter()
        .all(|pixel| *pixel == [0.0; 4]));
    for stops in [f32::NAN, f32::INFINITY, -33.0, 33.0] {
        let invalid = graph(stops, WorkingColorDomain::SceneLinearAcescg);
        assert_eq!(
            invalid.validate_working_color_domain(),
            Err("native exposure must be finite and within -32..=32 stops")
        );
    }
}

#[test]
fn acescct_nodes_round_trip_after_scene_exposure_and_enforce_order() {
    let mut graph = graph(1.0, WorkingColorDomain::SceneLinearAcescg);
    graph.nodes.push(IrNode {
        op: IrOp::NativeAcescct {
            direction: AcescctDirection::Encode,
        },
        inputs: vec![(IrNodeId(1), OutPort::default())],
        content_hash: ContentHash(3),
    });
    graph.nodes.push(IrNode {
        op: IrOp::NativeAcescct {
            direction: AcescctDirection::Decode,
        },
        inputs: vec![(IrNodeId(2), OutPort::default())],
        content_hash: ContentHash(4),
    });
    graph.output = Some(IrNodeId(3));
    assert!(graph.validate_working_color_domain().is_ok());
    let cpu = eval_cpu::evaluate(&graph, (3, 2), &mut EmptyProvider);
    let expected = [0.5, 0.2, 0.1, 0.5];
    for pixel in &cpu.pixels {
        for (actual, target) in pixel.iter().zip(expected) {
            assert!((actual - target).abs() < 0.0002, "{pixel:?}");
        }
    }
    if let Some(gpu) = GpuContext::request_blocking() {
        let mut evaluator = Evaluator::new(gpu.clone());
        let texture = evaluator
            .evaluate(&graph, (3, 2), &mut NullFrameSource)
            .unwrap();
        for pixel in read_texture_rgba16f(&gpu, &texture, 3, 2) {
            for (actual, target) in pixel.iter().zip(expected) {
                assert!((actual - target).abs() < 0.005, "{pixel:?}");
            }
        }
    }
    graph.output = Some(IrNodeId(2));
    assert_eq!(
        graph.validate_working_color_domain(),
        Err("ACEScct grading coordinates cannot be a displayed output")
    );
    graph.output = Some(IrNodeId(3));
    graph.nodes[2].op = IrOp::NativeAcescct {
        direction: AcescctDirection::Decode,
    };
    assert_eq!(
        graph.validate_working_color_domain(),
        Err("ACEScct transfer has an invalid input color domain")
    );
    graph.nodes[2].op = IrOp::NativeAcescct {
        direction: AcescctDirection::Encode,
    };
    graph.nodes[3].op = IrOp::NativeExposure { stops: 1.0 };
    assert_eq!(
        graph.validate_working_color_domain(),
        Err("native exposure requires scene-linear ACEScg")
    );
    graph.nodes[3].op = IrOp::Transform2D {
        mat: glam::Mat3::IDENTITY,
        sampling: photonic_video::graph::ir::Sampling::Nearest,
    };
    assert_eq!(
        graph.validate_working_color_domain(),
        Err("operation is not qualified in the ACEScct grading domain")
    );
}

#[test]
fn managed_graph_sdr_output_matches_scalar_after_exposure_and_log_round_trip() {
    let mut graph = graph(1.0, WorkingColorDomain::SceneLinearAcescg);
    graph.nodes.push(IrNode {
        op: IrOp::NativeAcescct {
            direction: AcescctDirection::Encode,
        },
        inputs: vec![(IrNodeId(1), OutPort::default())],
        content_hash: ContentHash(3),
    });
    graph.nodes.push(IrNode {
        op: IrOp::NativeAcescct {
            direction: AcescctDirection::Decode,
        },
        inputs: vec![(IrNodeId(2), OutPort::default())],
        content_hash: ContentHash(4),
    });
    graph.nodes.push(IrNode {
        op: IrOp::NativeSdrOutput,
        inputs: vec![(IrNodeId(3), OutPort::default())],
        content_hash: ContentHash(5),
    });
    graph.output = Some(IrNodeId(4));
    assert!(graph.validate_working_color_domain().is_ok());
    assert_eq!(
        graph.output_color_encoding(),
        Ok(FrameColorEncoding::SrgbDisplay)
    );
    let expected = Aces2SdrOutput::shared()
        .unwrap()
        .map_premultiplied([0.5, 0.2, 0.1, 0.5])
        .unwrap();
    let cpu = eval_cpu::evaluate(&graph, (3, 2), &mut EmptyProvider);
    for pixel in &cpu.pixels {
        for (actual, target) in pixel.iter().zip(expected) {
            assert!((f64::from(*actual) - target).abs() < 0.0005, "{pixel:?}");
        }
    }
    if let Some(gpu) = GpuContext::request_blocking() {
        let mut evaluator = Evaluator::new(gpu.clone());
        let output = evaluator
            .evaluate(&graph, (3, 2), &mut NullFrameSource)
            .unwrap();
        for pixel in read_texture_rgba16f(&gpu, &output, 3, 2) {
            for (actual, target) in pixel.iter().zip(expected) {
                assert!((f64::from(*actual) - target).abs() < 0.01, "{pixel:?}");
            }
        }
    }
    graph.output = Some(IrNodeId(3));
    assert_eq!(
        graph.validate_working_color_domain(),
        Err("managed SDR output transform must be the graph output")
    );
    graph.output = Some(IrNodeId(4));
    graph.nodes[4].inputs[0].0 = IrNodeId(2);
    assert_eq!(
        graph.validate_working_color_domain(),
        Err("SDR output transform requires scene-linear ACEScg")
    );
    graph.nodes[4].inputs[0].0 = IrNodeId(3);
    graph.nodes.push(IrNode {
        op: IrOp::NativeExposure { stops: 1.0 },
        inputs: vec![(IrNodeId(4), OutPort::default())],
        content_hash: ContentHash(6),
    });
    assert_eq!(
        graph.validate_working_color_domain(),
        Err("display-encoded pixels cannot enter another graph operation")
    );
}

#[test]
fn managed_graph_video_output_matches_scalar_and_stays_out_of_display_path() {
    let mut graph = graph(1.0, WorkingColorDomain::SceneLinearAcescg);
    graph.nodes.push(IrNode {
        op: IrOp::NativeSdrVideoOutput,
        inputs: vec![(IrNodeId(1), OutPort::default())],
        content_hash: ContentHash(6),
    });
    graph.output = Some(IrNodeId(2));
    assert_eq!(
        graph.output_color_encoding(),
        Ok(FrameColorEncoding::Bt709Video)
    );
    let expected = Aces2SdrOutput::shared()
        .unwrap()
        .map_premultiplied_video([0.5, 0.2, 0.1, 0.5])
        .unwrap();
    let cpu = eval_cpu::evaluate(&graph, (3, 2), &mut EmptyProvider);
    for pixel in &cpu.pixels {
        for (actual, target) in pixel.iter().zip(expected) {
            assert!((f64::from(*actual) - target).abs() < 0.0005, "{pixel:?}");
        }
    }
    if let Some(gpu) = GpuContext::request_blocking() {
        let mut evaluator = Evaluator::new(gpu.clone());
        let output = evaluator
            .evaluate(&graph, (3, 2), &mut NullFrameSource)
            .unwrap();
        for pixel in read_texture_rgba16f(&gpu, &output, 3, 2) {
            for (actual, target) in pixel.iter().zip(expected) {
                assert!((f64::from(*actual) - target).abs() < 0.01, "{pixel:?}");
            }
        }
    }
    graph.nodes.push(IrNode {
        op: IrOp::NativeExposure { stops: 1.0 },
        inputs: vec![(IrNodeId(2), OutPort::default())],
        content_hash: ContentHash(7),
    });
    graph.output = Some(IrNodeId(3));
    assert_eq!(
        graph.validate_working_color_domain(),
        Err("display-encoded pixels cannot enter another graph operation")
    );
}

#[test]
fn neutral_managed_video_signal_survives_ten_bit_pack_and_decode() {
    let output = Aces2SdrOutput::shared().unwrap();
    let scene = [0.18; 3];
    let rendered_linear = output.map_straight_linear(scene).unwrap();
    let signal = output.map_straight_video(scene).unwrap();
    let pixels: Vec<f32> = [signal[0], signal[1], signal[2], 1.0]
        .into_iter()
        .cycle()
        .take(8)
        .map(|value| value as f32)
        .collect();
    let planes = video_signal_frame_to_yuv422p10(&pixels, 2, 1, Colorimetry::BT709_LIMITED);
    let EncodePlanes::Yuv422P10 { y, cb, cr, .. } = planes else {
        panic!("expected 10-bit 4:2:2 planes");
    };
    let code = [
        u16::from_le_bytes([y[0], y[1]]),
        u16::from_le_bytes([cb[0], cb[1]]),
        u16::from_le_bytes([cr[0], cr[1]]),
    ];
    let reconstructed = decode_ycbcr_codes(code, 10, true, YcbcrMatrix::Bt709)
        .unwrap()
        .map(decode_bt709_source);
    for channel in 0..3 {
        assert!((reconstructed[channel] - rendered_linear[channel]).abs() < 0.005);
    }
}

#[test]
fn native_saturation_uses_ap1_luminance_and_preserves_extended_partial_alpha() {
    let gpu = GpuContext::request_blocking();
    for (rgb, alpha, saturation, vibrance) in [
        ([1.0, 0.0, 0.0], 1.0, 0.0, 0.0),
        ([-0.2, 2.0, 0.4], 0.5, 1.5, 0.3),
        ([0.6, 0.5, 0.4], 0.25, 1.0, 0.8),
        ([2.0, -0.1, 0.2], 0.5, 1.0, 0.0),
        ([0.5, 0.5, 0.5], 1.0, 3.0, -1.0),
        ([2.0, -0.1, 0.4], 0.0, 1.5, 0.5),
    ] {
        // Independent f64 reference: AP1 Y row, followed by Photonic's
        // scene-linear luminance/chroma scaling (no RGB output clamp).
        let input = rgb.map(f64::from);
        let y = input[0] * 0.2722287168 + input[1] * 0.6740817658 + input[2] * 0.0536895174;
        let peak = input[0].max(input[1]).max(input[2]);
        let trough = input[0].min(input[1]).min(input[2]);
        let reference = input[0]
            .abs()
            .max(input[1].abs())
            .max(input[2].abs())
            .max(0.001);
        let colorfulness = ((peak - trough) / reference).clamp(0.0, 1.0);
        let factor =
            f64::from(saturation) * (1.0 + f64::from(vibrance) * (1.0 - colorfulness)).max(0.0);
        let expected = input.map(|value| ((y + (value - y) * factor) * f64::from(alpha)) as f32);
        let mut graph = graph(0.0, WorkingColorDomain::SceneLinearAcescg);
        graph.nodes[0].op = IrOp::SolidColor {
            color: LinearColor {
                r: rgb[0] * alpha,
                g: rgb[1] * alpha,
                b: rgb[2] * alpha,
                a: alpha,
            },
        };
        graph.nodes.push(IrNode {
            op: IrOp::NativeSaturationVibrance {
                saturation,
                vibrance,
            },
            inputs: vec![(IrNodeId(1), OutPort::default())],
            content_hash: ContentHash(401),
        });
        graph.output = Some(IrNodeId(2));
        assert!(graph.validate_working_color_domain().is_ok());
        for pixel in eval_cpu::evaluate(&graph, (2, 2), &mut EmptyProvider).pixels {
            for (actual, target) in pixel[..3].iter().zip(expected) {
                assert!((actual - target).abs() < 1e-6, "{pixel:?} vs {expected:?}");
            }
            assert_eq!(pixel[3], alpha);
        }
        if let Some(gpu) = &gpu {
            let texture = Evaluator::new(gpu.clone())
                .evaluate(&graph, (2, 2), &mut NullFrameSource)
                .expect("native saturation renders");
            for pixel in read_texture_rgba16f(gpu, &texture, 2, 2) {
                for (actual, target) in pixel[..3].iter().zip(expected) {
                    assert!((actual - target).abs() < 0.004, "{pixel:?} vs {expected:?}");
                }
                assert_eq!(pixel[3], alpha);
            }
        }
    }
}

#[test]
fn native_saturation_rejects_invalid_values_and_legacy_domain() {
    for (saturation, vibrance) in [
        (f32::NAN, 0.0),
        (1.0, f32::INFINITY),
        (-0.1, 0.0),
        (4.1, 0.0),
        (1.0, -1.1),
        (1.0, 1.1),
    ] {
        let mut graph = graph(0.0, WorkingColorDomain::SceneLinearAcescg);
        graph.nodes[1].op = IrOp::NativeSaturationVibrance {
            saturation,
            vibrance,
        };
        assert!(graph.validate_working_color_domain().is_err());
    }
    let mut graph = graph(0.0, WorkingColorDomain::LegacyLinearRec709);
    graph.nodes[1].op = IrOp::NativeSaturationVibrance {
        saturation: 1.0,
        vibrance: 0.0,
    };
    assert_eq!(
        graph.validate_working_color_domain(),
        Err("native saturation/vibrance requires scene-linear ACEScg")
    );
}

#[test]
fn native_creative_lut_samples_declared_coordinates_without_legacy_transfer() {
    use photonic_render::grade::ResolvedLut3d;
    let gpu = GpuContext::request_blocking();
    let cube = "LUT_1D_SIZE 2\nDOMAIN_MIN -1 -1 -1\nDOMAIN_MAX 2 2 2\n0 0 0\n1 1 1\nLUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
    let table = std::sync::Arc::new(photonic_render::parse_cube(cube).unwrap());
    for tetrahedral in [false, true] {
        for intensity in [0.0, 0.5, 1.0] {
            let rgb = [-0.25_f32, 0.25, 1.5];
            let alpha = 0.5;
            // The linear shaper maps the explicitly declared -1..2 domain to 0..1.
            let mapped = rgb.map(|value| (value + 1.0) / 3.0);
            let expected = std::array::from_fn::<_, 3, _>(|c| {
                (rgb[c] + (mapped[c] - rgb[c]) * intensity) * alpha
            });
            let mut graph = graph(0.0, WorkingColorDomain::SceneLinearAcescg);
            graph.nodes[0].op = IrOp::SolidColor {
                color: LinearColor {
                    r: rgb[0] * alpha,
                    g: rgb[1] * alpha,
                    b: rgb[2] * alpha,
                    a: alpha,
                },
            };
            graph.nodes[1].op = IrOp::NativeLut3d {
                lut: ResolvedLut3d {
                    table: table.clone(),
                    intensity,
                    tetrahedral,
                },
            };
            for pixel in eval_cpu::evaluate(&graph, (2, 2), &mut EmptyProvider).pixels {
                for (actual, target) in pixel[..3].iter().zip(expected) {
                    assert!((actual - target).abs() < 1e-6, "{pixel:?} vs {expected:?}");
                }
                assert_eq!(pixel[3], alpha);
            }
            if let Some(gpu) = &gpu {
                let texture = Evaluator::new(gpu.clone())
                    .evaluate(&graph, (2, 2), &mut NullFrameSource)
                    .expect("native LUT renders");
                for pixel in read_texture_rgba16f(gpu, &texture, 2, 2) {
                    for (actual, target) in pixel[..3].iter().zip(expected) {
                        assert!((actual - target).abs() < 0.004, "{pixel:?} vs {expected:?}");
                    }
                    assert_eq!(pixel[3], alpha);
                }
            }
        }
    }
}

#[test]
fn native_lut_rejects_invalid_table_data_before_evaluation() {
    use photonic_render::grade::ResolvedLut3d;
    for invalid in 0..6 {
        let mut table = photonic_render::Lut3d::identity(2);
        let mut intensity = 1.0;
        match invalid {
            0 => {
                table.data.pop();
            }
            1 => table.data[0][0] = f32::NAN,
            2 => table.domain_max[0] = table.domain_min[0],
            3 => intensity = f32::INFINITY,
            4 => table.data[0][0] = 70000.0,
            _ => table.shaper = Some(vec![[0.0; 3], [70000.0; 3]]),
        }
        let mut graph = graph(0.0, WorkingColorDomain::SceneLinearAcescg);
        graph.nodes[1].op = IrOp::NativeLut3d {
            lut: ResolvedLut3d {
                table: std::sync::Arc::new(table),
                intensity,
                tetrahedral: false,
            },
        };
        assert!(graph.validate_working_color_domain().is_err());
        assert!(eval_cpu::evaluate(&graph, (2, 2), &mut EmptyProvider)
            .pixels
            .iter()
            .all(|p| *p == [0.0; 4]));
    }
}

#[test]
fn native_acescct_lut_roundtrip_matches_piecewise_reference() {
    use photonic_render::grade::ResolvedLut3d;
    let encode = |x: f64| {
        if x <= 0.0078125 {
            10.5402377416545 * x + 0.0729055341958355
        } else {
            (x.log2() + 9.72) / 17.52
        }
    };
    let decode = |x: f64| {
        if x <= 0.155251141552511 {
            (x - 0.0729055341958355) / 10.5402377416545
        } else {
            (x * 17.52 - 9.72).exp2().min(65504.0)
        }
    };
    let rgb = [-0.01_f32, 0.18, 2.0];
    let alpha = 0.5;
    let expected = rgb.map(|x| {
        (decode(0.05 + 0.95 * encode(f64::from(x)).clamp(0.0, 1.0)) * f64::from(alpha)) as f32
    });
    let mut table = photonic_render::Lut3d::identity(2);
    for sample in &mut table.data {
        *sample = sample.map(|x| 0.05 + 0.95 * x);
    }
    let graph = FrameGraph {
        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
        nodes: vec![
            IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: rgb[0] * alpha,
                        g: rgb[1] * alpha,
                        b: rgb[2] * alpha,
                        a: alpha,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(701),
            },
            IrNode {
                op: IrOp::NativeAcescct {
                    direction: AcescctDirection::Encode,
                },
                inputs: vec![(IrNodeId(0), OutPort::default())],
                content_hash: ContentHash(702),
            },
            IrNode {
                op: IrOp::NativeLut3d {
                    lut: ResolvedLut3d {
                        table: std::sync::Arc::new(table),
                        intensity: 1.0,
                        tetrahedral: true,
                    },
                },
                inputs: vec![(IrNodeId(1), OutPort::default())],
                content_hash: ContentHash(703),
            },
            IrNode {
                op: IrOp::NativeAcescct {
                    direction: AcescctDirection::Decode,
                },
                inputs: vec![(IrNodeId(2), OutPort::default())],
                content_hash: ContentHash(704),
            },
        ],
        output: Some(IrNodeId(3)),
    };
    assert!(graph.validate_working_color_domain().is_ok());
    for pixel in eval_cpu::evaluate(&graph, (2, 2), &mut EmptyProvider).pixels {
        for (actual, target) in pixel[..3].iter().zip(expected) {
            assert!((actual - target).abs() < 1e-5, "{pixel:?} vs {expected:?}");
        }
        assert_eq!(pixel[3], alpha);
    }
    if let Some(gpu) = GpuContext::request_blocking() {
        let texture = Evaluator::new(gpu.clone())
            .evaluate(&graph, (2, 2), &mut NullFrameSource)
            .expect("log LUT graph renders");
        for pixel in read_texture_rgba16f(&gpu, &texture, 2, 2) {
            for (actual, target) in pixel[..3].iter().zip(expected) {
                assert!((actual - target).abs() < 0.015, "{pixel:?} vs {expected:?}");
            }
            assert_eq!(pixel[3], alpha);
        }
    }
}

#[test]
fn native_power_windows_mix_extended_pixels_at_sequence_coordinates() {
    check_native_power_window_mixing(false);
}

#[test]
fn tonal_exposure_power_windows_preserve_alpha_and_logical_coordinates() {
    check_native_power_window_mixing(true);
}

fn check_native_power_window_mixing(tonal: bool) {
    use photonic_core::timeline::WindowShape;
    use photonic_render::grade::ResolvedMask;
    let gpu = GpuContext::request_blocking();
    for shape in [
        WindowShape::Ellipse,
        WindowShape::Rectangle,
        WindowShape::Gradient,
    ] {
        for invert in [false, true] {
            let mask = ResolvedMask {
                shape,
                center: [0.5, 0.5],
                size: [0.3, 0.4],
                rotation: 0.3,
                softness: 0.4,
                invert,
            };
            let mut graph = graph(1.0, WorkingColorDomain::SceneLinearAcescg);
            if tonal {
                graph.nodes[1].op = IrOp::NativeTonalExposure {
                    tonal: photonic_render::tonal_exposure::ResolvedTonalExposure {
                        stops: [1.0; 3],
                        boundaries: [-2.0, 2.0],
                        softness: 1.0,
                        native_ap1: true,
                    },
                };
            }

            graph.nodes[0].op = IrOp::SolidColor {
                color: LinearColor {
                    r: -0.125,
                    g: 1.0,
                    b: 0.25,
                    a: 0.5,
                },
            };
            graph.nodes.push(IrNode {
                op: IrOp::NativeMaskMix { mask },
                inputs: vec![
                    (IrNodeId(1), OutPort::default()),
                    (IrNodeId(0), OutPort::default()),
                ],
                content_hash: ContentHash(801),
            });
            graph.output = Some(IrNodeId(2));
            assert!(graph.validate_working_color_domain().is_ok());
            let cpu = eval_cpu::evaluate(&graph, (5, 3), &mut EmptyProvider);
            for (index, pixel) in cpu.pixels.iter().enumerate() {
                // Independent f64 window coordinates and smoothstep equation.
                let dx = (index % 5) as f64 / 5.0 + 0.1 - 0.5;
                let dy = (index / 5) as f64 / 3.0 + 1.0 / 6.0 - 0.5;
                let (s, c) = 0.3_f64.sin_cos();
                let x = dx * c + dy * s;
                let y = -dx * s + dy * c;
                let smooth = |lo: f64, hi: f64, value: f64| {
                    let t = ((value - lo) / (hi - lo)).clamp(0.0, 1.0);
                    t * t * (3.0 - 2.0 * t)
                };
                let weight = match shape {
                    WindowShape::Gradient => 1.0 - smooth(-0.4, 0.4, y),
                    WindowShape::Rectangle => {
                        1.0 - smooth(0.6, 1.4, (x / 0.3).abs().max((y / 0.4).abs()))
                    }
                    WindowShape::Ellipse => {
                        1.0 - smooth(0.6, 1.4, ((x / 0.3).powi(2) + (y / 0.4).powi(2)).sqrt())
                    }
                };
                let weight = if invert { 1.0 - weight } else { weight } as f32;
                let expected = [
                    -0.125 * (1.0 + weight),
                    1.0 + weight,
                    0.25 * (1.0 + weight),
                    0.5,
                ];
                for (actual, target) in pixel.iter().zip(expected) {
                    assert!(
                        (actual - target).abs() < 1e-6,
                        "{shape:?} {invert} {index}: {pixel:?} vs {expected:?}"
                    );
                }
            }
            if let Some(gpu) = &gpu {
                // A 5x3 logical frame uses a pooled 64x64 target; the mask must
                // follow the logical picture rather than the physical bucket.
                let texture = Evaluator::new(gpu.clone())
                    .evaluate(&graph, (5, 3), &mut NullFrameSource)
                    .expect("native mask graph renders");
                for (actual, expected) in read_texture_rgba16f(gpu, &texture, 5, 3)
                    .iter()
                    .zip(cpu.pixels)
                {
                    for (value, target) in actual.iter().zip(expected) {
                        assert!(
                            (value - target).abs() < 0.004,
                            "{shape:?} {invert}: {actual:?} vs {expected:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn native_power_windows_refuse_invalid_geometry() {
    use photonic_core::timeline::WindowShape;
    use photonic_render::grade::ResolvedMask;
    for invalid in 0..4 {
        let mut mask = ResolvedMask {
            shape: WindowShape::Ellipse,
            center: [0.5; 2],
            size: [0.25; 2],
            rotation: 0.0,
            softness: 0.1,
            invert: false,
        };
        match invalid {
            0 => mask.center[0] = f32::NAN,
            1 => mask.size[0] = 0.0,
            2 => mask.rotation = f32::INFINITY,
            _ => mask.softness = -0.1,
        }
        let mut graph = graph(1.0, WorkingColorDomain::SceneLinearAcescg);
        graph.nodes.push(IrNode {
            op: IrOp::NativeMaskMix { mask },
            inputs: vec![
                (IrNodeId(1), OutPort::default()),
                (IrNodeId(0), OutPort::default()),
            ],
            content_hash: ContentHash(802),
        });
        graph.output = Some(IrNodeId(2));
        assert!(graph.validate_working_color_domain().is_err());
    }
}

#[test]
fn native_log_contrast_preserves_pivot_alpha_and_extended_coordinates() {
    let gpu = GpuContext::request_blocking();
    for (rgb, alpha, pivot, amount) in [
        ([-0.2, 0.4, 1.2], 0.5, 0.4, 1.0),
        ([0.4, 0.4, 0.4], 0.25, 0.4, -2.0),
        ([-0.1, 0.6, 1.4], 1.0, 0.5, 0.0),
        ([0.1, 0.5, 0.9], 0.0, 0.5, 2.0),
    ] {
        let mut graph = graph(0.0, WorkingColorDomain::SceneLinearAcescg);
        graph.nodes[0].op = IrOp::SolidColor {
            color: LinearColor {
                r: rgb[0] * alpha,
                g: rgb[1] * alpha,
                b: rgb[2] * alpha,
                a: alpha,
            },
        };
        // The transfer is tested independently elsewhere. Here values enter
        // the log operator directly to isolate its mathematical contract.
        graph.nodes[1].op = IrOp::NativeAcescct {
            direction: AcescctDirection::Encode,
        };
        graph.nodes.push(IrNode {
            op: IrOp::NativeLogContrast { pivot, amount },
            inputs: vec![(IrNodeId(1), OutPort::default())],
            content_hash: ContentHash(901),
        });
        graph.nodes.push(IrNode {
            op: IrOp::NativeAcescct {
                direction: AcescctDirection::Decode,
            },
            inputs: vec![(IrNodeId(2), OutPort::default())],
            content_hash: ContentHash(903),
        });
        graph.output = Some(IrNodeId(3));
        assert!(graph.validate_working_color_domain().is_ok());
        let encode = |v: f64| {
            if v <= 0.0078125 {
                v * 10.5402377416545 + 0.0729055341958355
            } else {
                (v.log2() + 9.72) / 17.52
            }
        };
        let decode = |v: f64| {
            if v <= 0.155251141552511 {
                (v - 0.0729055341958355) / 10.5402377416545
            } else {
                2f64.powf(v * 17.52 - 9.72).min(65504.0)
            }
        };
        let expected = rgb.map(|v| {
            (decode(
                f64::from(pivot)
                    + (encode(f64::from(v)) - f64::from(pivot)) * 2f64.powf(f64::from(amount)),
            ) * f64::from(alpha)) as f32
        });
        for pixel in eval_cpu::evaluate(&graph, (3, 2), &mut EmptyProvider).pixels {
            for (actual, target) in pixel[..3].iter().zip(expected) {
                assert!(
                    (actual - target).abs() < 1e-5 * target.abs().max(1.0),
                    "{pixel:?} {expected:?}"
                );
            }
            assert_eq!(pixel[3], alpha);
        }
        if let Some(gpu) = &gpu {
            let texture = Evaluator::new(gpu.clone())
                .evaluate(&graph, (3, 2), &mut NullFrameSource)
                .unwrap();
            for pixel in read_texture_rgba16f(gpu, &texture, 3, 2) {
                for (actual, target) in pixel[..3].iter().zip(expected) {
                    assert!(
                        (actual - target).abs() < 0.025 * target.abs().max(1.0),
                        "{pixel:?} {expected:?}"
                    );
                }
                assert_eq!(pixel[3], alpha);
            }
        }
    }
}

#[test]
fn native_log_contrast_rejects_wrong_domain_and_invalid_parameters() {
    for (pivot, amount) in [
        (f32::NAN, 0.0),
        (0.5, f32::INFINITY),
        (-0.1, 0.0),
        (1.1, 0.0),
        (0.5, -4.1),
        (0.5, 4.1),
    ] {
        let mut graph = graph(0.0, WorkingColorDomain::SceneLinearAcescg);
        graph.nodes[1].op = IrOp::NativeAcescct {
            direction: AcescctDirection::Encode,
        };
        graph.nodes.push(IrNode {
            op: IrOp::NativeLogContrast { pivot, amount },
            inputs: vec![(IrNodeId(1), OutPort::default())],
            content_hash: ContentHash(902),
        });
        graph.output = Some(IrNodeId(2));
        assert!(graph.validate_working_color_domain().is_err());
    }
    for domain in [
        WorkingColorDomain::SceneLinearAcescg,
        WorkingColorDomain::LegacyLinearRec709,
    ] {
        let mut graph = graph(0.0, domain);
        graph.nodes[1].op = IrOp::NativeLogContrast {
            pivot: 0.5,
            amount: 1.0,
        };
        assert!(graph.validate_working_color_domain().is_err());
    }
}

#[test]
fn native_exposure_over_half_float_range_stays_finite_and_preserves_alpha() {
    let Some(gpu) = GpuContext::request_blocking() else {
        return;
    };
    let mut scene = graph(32.0, WorkingColorDomain::SceneLinearAcescg);
    if let IrOp::SolidColor { color } = &mut scene.nodes[0].op {
        color.b = -0.05;
    }
    let texture = Evaluator::new(gpu.clone())
        .evaluate(&scene, (1, 1), &mut NullFrameSource)
        .unwrap();
    let pixels = read_texture_rgba16f(&gpu, &texture, 1, 1);
    assert_eq!(pixels[0], [65504.0, 65504.0, -65504.0, 0.5]);
    let cpu = eval_cpu::evaluate(&scene, (1, 1), &mut EmptyProvider);
    assert_eq!(cpu.pixels[0], pixels[0]);
}

#[test]
fn native_log_cdl_known_vectors_alpha_negative_power_and_domain() {
    use photonic_render::grade::ResolvedCdl;
    let cdl = ResolvedCdl {
        slope: [1.2, 0.8, 1.1],
        offset: [-0.2, 0.1, -0.1],
        power: [2.0, 0.5, 1.4],
        sat: 1.3,
    };
    let gpu = GpuContext::request_blocking().expect("native CDL qualification requires GPU");
    for (rgb, a) in [
        ([-0.5f32, 0.2, 2.0], 0.5),
        ([0.1, 0.6, 1.4], 1.0),
        ([0.1, 0.5, 0.9], 0.0),
    ] {
        let mut g = graph(0.0, WorkingColorDomain::SceneLinearAcescg);
        g.nodes[0].op = IrOp::SolidColor {
            color: LinearColor {
                r: rgb[0] * a,
                g: rgb[1] * a,
                b: rgb[2] * a,
                a,
            },
        };
        g.nodes[1].op = IrOp::NativeAcescct {
            direction: AcescctDirection::Encode,
        };
        g.nodes.push(IrNode {
            op: IrOp::NativeLogCdl { cdl },
            inputs: vec![(IrNodeId(1), OutPort::default())],
            content_hash: ContentHash(990),
        });
        g.nodes.push(IrNode {
            op: IrOp::NativeAcescct {
                direction: AcescctDirection::Decode,
            },
            inputs: vec![(IrNodeId(2), OutPort::default())],
            content_hash: ContentHash(991),
        });
        g.output = Some(IrNodeId(3));
        assert!(g.validate_working_color_domain().is_ok());
        let encode = |v: f64| {
            if v <= 0.0078125 {
                v * 10.5402377416545 + 0.0729055341958355
            } else {
                (v.log2() + 9.72) / 17.52
            }
        };
        let sop = std::array::from_fn::<_, 3, _>(|c| {
            let v = encode(f64::from(rgb[c])) * f64::from(cdl.slope[c]) + f64::from(cdl.offset[c]);
            if v <= 0.0 {
                v
            } else {
                v.powf(f64::from(cdl.power[c]))
            }
        });
        let y = sop[0] * 0.2126 + sop[1] * 0.7152 + sop[2] * 0.0722;
        let expected = sop.map(|v| {
            let log = y + f64::from(cdl.sat) * (v - y);
            let linear = if log <= 0.155251141552511 {
                (log - 0.0729055341958355) / 10.5402377416545
            } else {
                2f64.powf(log * 17.52 - 9.72).min(65504.0)
            };
            linear * f64::from(a)
        });
        let cpu = eval_cpu::evaluate(&g, (2, 1), &mut EmptyProvider);
        let tex = Evaluator::new(gpu.clone())
            .evaluate(&g, (2, 1), &mut NullFrameSource)
            .unwrap();
        for pixel in cpu
            .pixels
            .iter()
            .chain(read_texture_rgba16f(&gpu, &tex, 2, 1).iter())
        {
            for c in 0..3 {
                assert!(
                    (f64::from(pixel[c]) - expected[c]).abs() < 0.025 * expected[c].abs().max(1.0),
                    "{pixel:?} vs {expected:?}"
                );
            }
            assert_eq!(pixel[3], a);
        }
        g.nodes[2].inputs = vec![(IrNodeId(0), OutPort::default())];
        assert!(g.validate_working_color_domain().is_err());
    }
}
#[test]
fn native_log_cdl_extreme_lut_coordinates_remain_finite() {
    use photonic_render::grade::ResolvedCdl;
    let cdl = ResolvedCdl {
        slope: [8.0; 3],
        offset: [4.0; 3],
        power: [10.0; 3],
        sat: 4.0,
    };
    assert!(photonic_render::native_transfer::validate_native_cdl(&cdl).is_ok());
    let got = photonic_render::native_transfer::cdl_no_clamp([65504.0; 3], cdl);
    assert!(got.iter().all(|v| v.is_finite()), "{got:?}");
}

#[test]
fn native_log_curves_extrapolate_signed_headroom_and_preserve_alpha() {
    use photonic_render::grade::ResolvedCurves;
    let table = |s: f32, o: f32| std::array::from_fn(|i| s * (i as f32 / 255.0) + o);
    let curves = ResolvedCurves {
        master: table(0.85, 0.04),
        red: table(1.1, -0.03),
        green: table(0.9, 0.1),
        blue: table(1.0, 0.0),
        hue_vs_hue: None,
        hue_vs_sat: None,
        hue_vs_luma: None,
        luma_vs_sat: None,
        sat_vs_sat: None,
    };
    assert!(photonic_render::grade_gpu::validate_native_curves(&curves).is_ok());
    let gpu = GpuContext::request_blocking().expect("native curves qualification requires GPU");
    for (rgb, a) in [
        ([-2.0f32, 0.1, 1000.0], 0.5),
        ([0.01, 0.5, 2.0], 1.0),
        ([0.2, 0.5, 1.0], 0.0),
    ] {
        let mut g = graph(0.0, WorkingColorDomain::SceneLinearAcescg);
        g.nodes[0].op = IrOp::SolidColor {
            color: LinearColor {
                r: rgb[0] * a,
                g: rgb[1] * a,
                b: rgb[2] * a,
                a,
            },
        };
        g.nodes[1].op = IrOp::NativeAcescct {
            direction: AcescctDirection::Encode,
        };
        g.nodes.push(IrNode {
            op: IrOp::NativeLogCurves {
                curves: Box::new(curves.clone()),
            },
            inputs: vec![(IrNodeId(1), OutPort::default())],
            content_hash: ContentHash(995),
        });
        g.nodes.push(IrNode {
            op: IrOp::NativeAcescct {
                direction: AcescctDirection::Decode,
            },
            inputs: vec![(IrNodeId(2), OutPort::default())],
            content_hash: ContentHash(996),
        });
        g.output = Some(IrNodeId(3));
        assert!(g.validate_working_color_domain().is_ok());
        let expected = std::array::from_fn::<_, 3, _>(|c| {
            let v = f64::from(rgb[c]);
            let encoded = if v <= 0.0078125 {
                v * 10.5402377416545 + 0.0729055341958355
            } else {
                (v.log2() + 9.72) / 17.52
            };
            let master = encoded * 0.85 + 0.04;
            let log = master * [1.1, 0.9, 1.0][c] + [-0.03, 0.1, 0.0][c];
            let linear = if log <= 0.155251141552511 {
                (log - 0.0729055341958355) / 10.5402377416545
            } else {
                2f64.powf(log * 17.52 - 9.72).min(65504.0)
            };
            linear * f64::from(a)
        });
        let cpu = eval_cpu::evaluate(&g, (2, 1), &mut EmptyProvider);
        let tex = Evaluator::new(gpu.clone())
            .evaluate(&g, (2, 1), &mut NullFrameSource)
            .unwrap();
        for p in cpu
            .pixels
            .iter()
            .chain(read_texture_rgba16f(&gpu, &tex, 2, 1).iter())
        {
            for c in 0..3 {
                assert!(
                    (f64::from(p[c]) - expected[c]).abs() < 0.04 * expected[c].abs().max(1.0),
                    "{p:?} vs {expected:?}"
                );
            }
            assert_eq!(p[3], a);
        }
        g.nodes[2].inputs = vec![(IrNodeId(0), OutPort::default())];
        assert!(g.validate_working_color_domain().is_err());
    }
    let mut invalid = curves.clone();
    invalid.hue_vs_sat = Some([f32::NAN; 256]);
    assert!(photonic_render::grade_gpu::validate_native_curves(&invalid).is_err());
    invalid = curves.clone();
    invalid.master[100] = f32::NAN;
    assert!(photonic_render::grade_gpu::validate_native_curves(&invalid).is_err());
}

#[test]
fn native_log_qualifier_known_keys_soft_edges_and_extended_correction() {
    use photonic_render::grade::{ResolvedCdl, ResolvedHslQualifier, ResolvedQualifierKey};
    let gpu = GpuContext::request_blocking().expect("native qualifier qualification requires GPU");
    let base = ResolvedHslQualifier {
        hue: [0.9, 1.1],
        correction: ResolvedCdl {
            slope: [1.0; 3],
            offset: [-0.05, 0.03, 0.08],
            power: [1.0; 3],
            sat: 1.0,
        },
        ..Default::default()
    };
    let mut added = base;
    added.key_count = 1;
    added.keys[0] = ResolvedQualifierKey {
        hue: [0.3, 0.4],
        sat: [0.0, 1.0],
        lum: [0.0, 1.0],
        softness: 0.0,
        subtract: false,
    };
    let mut subtracted = base;
    subtracted.key_count = 1;
    subtracted.keys[0] = ResolvedQualifierKey {
        hue: [-0.1, 0.1],
        sat: [0.0, 1.0],
        lum: [0.0, 1.0],
        softness: 0.0,
        subtract: true,
    };
    let soft = ResolvedHslQualifier {
        hue: [0.0, 1.0],
        lum: [0.55, 0.7],
        softness: 0.1,
        ..base
    };
    let extended = ResolvedHslQualifier {
        hue: [0.55, 0.65],
        ..base
    };
    let decode = |v: f64| {
        if v <= 0.155251141552511 {
            (v - 0.0729055341958355) / 10.5402377416545
        } else {
            2f64.powf(v * 17.52 - 9.72).min(65504.0)
        }
    };
    for (log, q, gate, alpha) in [
        ([0.7f32, 0.2, 0.2], base, 1.0, 1.0),
        ([0.2, 0.7, 0.2], base, 0.0, 0.5),
        ([0.2, 0.7, 0.2], added, 1.0, 0.5),
        ([0.7, 0.2, 0.2], subtracted, 0.0, 1.0),
        ([0.5; 3], soft, 0.5, 0.5),
        ([-0.1, 0.5, 1.2], extended, 1.0, 1.0),
        ([0.7, 0.2, 0.2], base, 1.0, 0.0),
    ] {
        let mut g = graph(0.0, WorkingColorDomain::SceneLinearAcescg);
        let linear = log.map(|v| decode(f64::from(v)) as f32 * alpha);
        g.nodes[0].op = IrOp::SolidColor {
            color: LinearColor {
                r: linear[0],
                g: linear[1],
                b: linear[2],
                a: alpha,
            },
        };
        g.nodes[1].op = IrOp::NativeAcescct {
            direction: AcescctDirection::Encode,
        };
        g.nodes.push(IrNode {
            op: IrOp::NativeLogQualifier {
                qualifier: Box::new(q),
            },
            inputs: vec![(IrNodeId(1), OutPort::default())],
            content_hash: ContentHash(1201),
        });
        g.nodes.push(IrNode {
            op: IrOp::NativeAcescct {
                direction: AcescctDirection::Decode,
            },
            inputs: vec![(IrNodeId(2), OutPort::default())],
            content_hash: ContentHash(1202),
        });
        g.output = Some(IrNodeId(3));
        assert!(g.validate_working_color_domain().is_ok());
        let expected = std::array::from_fn::<_, 3, _>(|c| {
            decode(f64::from(log[c]) + gate * f64::from(q.correction.offset[c])) * f64::from(alpha)
        });
        let cpu = eval_cpu::evaluate(&g, (2, 1), &mut EmptyProvider);
        let texture = Evaluator::new(gpu.clone())
            .evaluate(&g, (2, 1), &mut NullFrameSource)
            .unwrap();
        for pixel in cpu
            .pixels
            .iter()
            .chain(read_texture_rgba16f(&gpu, &texture, 2, 1).iter())
        {
            for c in 0..3 {
                assert!(
                    (f64::from(pixel[c]) - expected[c]).abs() < 0.025 * expected[c].abs().max(1.0),
                    "gate {gate}, log {log:?}: {pixel:?} vs {expected:?}"
                );
            }
            assert_eq!(pixel[3], alpha);
        }
        g.nodes[2].inputs = vec![(IrNodeId(0), OutPort::default())];
        assert!(g.validate_working_color_domain().is_err());
    }
    for q in [
        ResolvedHslQualifier {
            key_count: 17,
            ..base
        },
        ResolvedHslQualifier {
            hue: [f32::NAN, 1.0],
            ..base
        },
        ResolvedHslQualifier {
            lum: [0.8, 0.2],
            ..base
        },
    ] {
        assert!(photonic_render::native_transfer::validate_native_qualifier(&q).is_err());
    }
}

#[test]
fn native_secondary_curves_known_log_colors_and_extended_residual() {
    use photonic_render::grade::ResolvedCurves;
    let gpu =
        GpuContext::request_blocking().expect("native secondary curve qualification requires GPU");
    let identity = photonic_render::grade::curve_lut(&[]);
    let base = ResolvedCurves {
        master: identity,
        red: identity,
        green: identity,
        blue: identity,
        hue_vs_hue: None,
        hue_vs_sat: None,
        hue_vs_luma: None,
        luma_vs_sat: None,
        sat_vs_sat: None,
    };
    let decode = |v: f64| {
        if v <= 0.155251141552511 {
            (v - 0.0729055341958355) / 10.5402377416545
        } else {
            2f64.powf(v * 17.52 - 9.72).min(65504.0)
        }
    };
    let luma = 0.7 * 0.27222872 + 0.2 * (0.67408174 + 0.053689517);
    for (family, log, expected_log) in [
        (0, [0.7f32, 0.2, 0.2], [0.7, 0.7, 0.2]),
        (1, [0.7, 0.2, 0.2], [0.45, 0.45, 0.45]),
        (2, [0.7, 0.2, 0.2], [0.8, 0.3, 0.3]),
        (
            3,
            [0.7, 0.2, 0.2],
            [0.45 + 0.5 * luma, 0.45 - 0.5 * luma, 0.45 - 0.5 * luma],
        ),
        (4, [0.7, 0.2, 0.2], [0.575, 0.325, 0.325]),
        (0, [-0.1, 0.5, 1.2], [0.4, 0.0, 1.2]),
        (5, [-0.1, 0.5, 1.2], [-0.1, 0.5, 1.2]),
    ] {
        let mut curves = base.clone();
        match family {
            0 => curves.hue_vs_hue = Some([2.0 / 3.0; 256]),
            1 => curves.hue_vs_sat = Some([0.0; 256]),
            2 => curves.hue_vs_luma = Some([0.6; 256]),
            3 => curves.luma_vs_sat = Some(identity),
            4 => curves.sat_vs_sat = Some([0.25; 256]),
            _ => {
                curves.hue_vs_hue = Some([0.5; 256]);
                curves.hue_vs_sat = Some([0.5; 256]);
                curves.hue_vs_luma = Some([0.5; 256]);
                curves.luma_vs_sat = Some([0.5; 256]);
                curves.sat_vs_sat = Some([0.5; 256]);
            }
        }
        assert!(photonic_render::grade_gpu::validate_native_curves(&curves).is_ok());
        for alpha in [1.0, 0.5, 0.0] {
            let mut g = graph(0.0, WorkingColorDomain::SceneLinearAcescg);
            let scene = log.map(|v| decode(f64::from(v)) as f32 * alpha);
            g.nodes[0].op = IrOp::SolidColor {
                color: LinearColor {
                    r: scene[0],
                    g: scene[1],
                    b: scene[2],
                    a: alpha,
                },
            };
            g.nodes[1].op = IrOp::NativeAcescct {
                direction: AcescctDirection::Encode,
            };
            g.nodes.push(IrNode {
                op: IrOp::NativeLogCurves {
                    curves: Box::new(curves.clone()),
                },
                inputs: vec![(IrNodeId(1), OutPort::default())],
                content_hash: ContentHash(1401),
            });
            g.nodes.push(IrNode {
                op: IrOp::NativeAcescct {
                    direction: AcescctDirection::Decode,
                },
                inputs: vec![(IrNodeId(2), OutPort::default())],
                content_hash: ContentHash(1402),
            });
            g.output = Some(IrNodeId(3));
            assert!(g.validate_working_color_domain().is_ok());
            let expected = expected_log.map(|v| decode(v) * f64::from(alpha));
            let cpu = eval_cpu::evaluate(&g, (2, 1), &mut EmptyProvider);
            let texture = Evaluator::new(gpu.clone())
                .evaluate(&g, (2, 1), &mut NullFrameSource)
                .unwrap();
            for pixel in cpu
                .pixels
                .iter()
                .chain(read_texture_rgba16f(&gpu, &texture, 2, 1).iter())
            {
                for c in 0..3 {
                    assert!(
                        (f64::from(pixel[c]) - expected[c]).abs()
                            < 0.03 * expected[c].abs().max(1.0),
                        "family {family}, log {log:?}: {pixel:?} vs {expected:?}"
                    );
                }
                assert_eq!(pixel[3], alpha);
            }
        }
    }
}

#[test]
fn native_typed_matte_graph_preserves_alpha_through_gpu_evaluator() {
    use photonic_core::timeline::GradeKeyMixMode;
    let ops = vec![
        (
            IrOp::SolidColor {
                color: LinearColor {
                    r: 0.125,
                    g: -0.25,
                    b: 1.0,
                    a: 0.5,
                },
            },
            vec![],
        ),
        (IrOp::NativeExposure { stops: 2.0 }, vec![0]),
        (
            IrOp::NativeAcescct {
                direction: AcescctDirection::Encode,
            },
            vec![0],
        ),
        (
            IrOp::QualifierMatte {
                qualifier: Box::default(),
                mask: None,
                native: true,
            },
            vec![2],
        ),
        (IrOp::GradeMatteConstant { weight: 0.25 }, vec![]),
        (
            IrOp::GradeKeyMix {
                mode: GradeKeyMixMode::Multiply,
            },
            vec![3, 4],
        ),
        (IrOp::GradeMatteApply, vec![1, 0, 5]),
    ];
    let mut graph = FrameGraph {
        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
        nodes: ops
            .into_iter()
            .enumerate()
            .map(|(index, (op, inputs))| IrNode {
                op,
                inputs: inputs
                    .into_iter()
                    .map(|id| (IrNodeId(id), OutPort::default()))
                    .collect(),
                content_hash: ContentHash(20000 + index as u128),
            })
            .collect(),
        output: Some(IrNodeId(6)),
    };
    assert_eq!(graph.validate_working_color_domain(), Ok(()));
    assert_eq!(
        graph.node_color_encoding(IrNodeId(3)).unwrap(),
        FrameColorEncoding::MatteWeight
    );
    let gpu = GpuContext::request_blocking().expect("typed matte qualification requires GPU");
    let frame = Evaluator::new(gpu.clone())
        .evaluate(&graph, (17, 9), &mut NullFrameSource)
        .unwrap();
    let cpu = eval_cpu::evaluate(&graph, (17, 9), &mut EmptyProvider);
    let pixels = read_texture_rgba16f(&gpu, &frame, 17, 9);
    for pixel in pixels.iter().chain(cpu.pixels.iter()) {
        for (actual, expected) in pixel.iter().zip([0.21875, -0.4375, 1.75, 0.5]) {
            assert!((*actual - expected).abs() < 0.002, "{pixel:?}");
        }
    }
    graph.nodes.insert(
        6,
        IrNode {
            op: IrOp::GradeMatteRefine {
                refinement: photonic_core::timeline::GradeMatteRefinement {
                    denoise: true,
                    grow: 0.02,
                    blur: 0.02,
                    matte_levels: [0.1, 0.2],
                },
            },
            inputs: vec![(IrNodeId(5), OutPort::default())],
            content_hash: ContentHash(20007),
        },
    );
    graph.nodes[7].inputs[2].0 = IrNodeId(6);
    graph.output = Some(IrNodeId(7));
    assert_eq!(graph.validate_working_color_domain(), Ok(()));
    let refined = Evaluator::new(gpu.clone())
        .evaluate(&graph, (17, 9), &mut NullFrameSource)
        .unwrap();
    let cpu = eval_cpu::evaluate(&graph, (17, 9), &mut EmptyProvider);
    for pixel in read_texture_rgba16f(&gpu, &refined, 17, 9)
        .iter()
        .chain(cpu.pixels.iter())
    {
        // Constant key remains .25 through spatial stages, then (.25-.1)/(.7).
        for (actual, expected) in pixel.iter().zip([0.20535715, -0.4107143, 1.6428572, 0.5]) {
            assert!((*actual - expected).abs() < 0.002, "{pixel:?}");
        }
    }
    graph.nodes[7].inputs[2].0 = IrNodeId(0);
    assert!(
        graph.validate_working_color_domain().is_err(),
        "an image cannot supply a matte port"
    );
    graph.nodes[7].inputs[2].0 = IrNodeId(6);
    graph.output = Some(IrNodeId(3));
    assert!(
        graph.validate_working_color_domain().is_err(),
        "matte weights cannot be displayed as image output"
    );
}

/// Independent f64 extended-AP1 oracle. The polynomial/min/max functions use
/// the W3C separable equations; signed/highlight extension is explicitly ours,
/// rather than a claim that a scene-linear result matches an SDR CSS renderer.
#[test]
fn native_separable_blends_preserve_extended_color_and_alpha() {
    use photonic_core::layer::BlendMode::*;
    let gpu = GpuContext::request_blocking().expect("GPU required for native blend qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut identity = 30000u128;
    for mode in [
        Normal, Multiply, Screen, Darken, Lighten, Difference, Exclusion, Overlay, HardLight,
        PinLight, SoftLight,
    ] {
        for (source_alpha, backdrop_alpha) in
            [(1.0, 1.0), (0.25, 0.5), (0.0, 1.0), (1.0, 0.0), (0.0, 0.0)]
        {
            for opacity in [0.0, 0.5, 1.0] {
                for (cs, cb) in [
                    ([-0.5f64, 2.0, 0.25], [1.5f64, -0.25, 4.0]),
                    ([0.499, 0.5, 0.501], [0.501, 0.5, 0.499]),
                    ([0.0, 1.0, -1.0], [1.0, 0.0, 2.0]),
                    ([0.2, 0.7, 0.8], [0.8, 0.3, 0.2]),
                    ([0.499, 0.5, 0.501], [0.249, 0.25, 0.251]),
                    ([0.75, 1.5, 0.5], [-0.125, 0.25, 0.0]),
                ] {
                    let a_s = source_alpha * opacity;
                    let mut expected = [0f64; 4];
                    for c in 0..3 {
                        let b = match mode {
                            Normal => cs[c],
                            Multiply => cb[c] * cs[c],
                            Screen => 1.0 - (1.0 - cb[c]) * (1.0 - cs[c]),
                            Darken => cb[c].min(cs[c]),
                            Lighten => cb[c].max(cs[c]),
                            Difference => (cb[c] - cs[c]).abs(),
                            Exclusion => cb[c] * (1.0 - cs[c]) + cs[c] * (1.0 - cb[c]),
                            Overlay => {
                                if cb[c] <= 0.5 {
                                    2.0 * cb[c] * cs[c]
                                } else {
                                    1.0 - 2.0 * (1.0 - cb[c]) * (1.0 - cs[c])
                                }
                            }
                            HardLight => {
                                if cs[c] <= 0.5 {
                                    2.0 * cb[c] * cs[c]
                                } else {
                                    1.0 - 2.0 * (1.0 - cb[c]) * (1.0 - cs[c])
                                }
                            }
                            SoftLight => {
                                let backdrop = cb[c];
                                let source = cs[c];
                                if source <= 0.5 {
                                    backdrop - (1.0 - 2.0 * source) * backdrop * (1.0 - backdrop)
                                } else {
                                    let curve = if backdrop <= 0.25 {
                                        16.0 * backdrop.powi(3) - 12.0 * backdrop.powi(2)
                                            + 4.0 * backdrop
                                    } else {
                                        backdrop.sqrt()
                                    };
                                    backdrop + (2.0 * source - 1.0) * (curve - backdrop)
                                }
                            }
                            PinLight => {
                                if cs[c] <= 0.5 {
                                    cb[c].min(2.0 * cs[c])
                                } else {
                                    cb[c].max(2.0 * cs[c] - 1.0)
                                }
                            }
                            _ => unreachable!(),
                        };
                        expected[c] = a_s * ((1.0 - backdrop_alpha) * cs[c] + backdrop_alpha * b)
                            + (1.0 - a_s) * backdrop_alpha * cb[c];
                    }
                    expected[3] = a_s + backdrop_alpha * (1.0 - a_s);
                    identity += 3;
                    let solid = |rgb: [f64; 3], alpha: f64, hash| IrNode {
                        op: IrOp::SolidColor {
                            color: LinearColor {
                                r: (rgb[0] * alpha) as f32,
                                g: (rgb[1] * alpha) as f32,
                                b: (rgb[2] * alpha) as f32,
                                a: alpha as f32,
                            },
                        },
                        inputs: vec![],
                        content_hash: ContentHash(hash),
                    };
                    let graph = FrameGraph {
                        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                        nodes: vec![
                            solid(cs, source_alpha, identity),
                            solid(cb, backdrop_alpha, identity + 1),
                            IrNode {
                                op: IrOp::Merge {
                                    mode,
                                    opacity: opacity as f32,
                                },
                                inputs: vec![
                                    (IrNodeId(0), OutPort::default()),
                                    (IrNodeId(1), OutPort::default()),
                                ],
                                content_hash: ContentHash(identity + 2),
                            },
                        ],
                        output: Some(IrNodeId(2)),
                    };
                    assert!(graph.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&graph, (1, 1), &mut EmptyProvider);
                    let texture = evaluator
                        .evaluate(&graph, (1, 1), &mut NullFrameSource)
                        .unwrap();
                    let pixels = read_texture_rgba16f(&gpu, &texture, 1, 1);
                    for channel in 0..4 {
                        assert!((cpu.pixels[0][channel] as f64 - expected[channel]).abs() < 1e-6,
                        "CPU {mode:?} alpha {source_alpha}/{backdrop_alpha} opacity {opacity}: {:?} vs {expected:?}", cpu.pixels[0]);
                        assert!((pixels[0][channel] as f64 - expected[channel]).abs() < 0.01,
                        "GPU {mode:?} alpha {source_alpha}/{backdrop_alpha} opacity {opacity}: {:?} vs {expected:?}", pixels[0]);
                    }
                }
            }
        }
    }
}

#[test]
fn native_arithmetic_storage_bounds_and_legacy_equations_stay_separate() {
    use photonic_core::layer::BlendMode::*;
    let gpu =
        GpuContext::request_blocking().expect("GPU required for arithmetic storage qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut id = 51000u128;
    for mode in [LinearDodge, LinearBurn, Subtract, LinearLight] {
        for (source_alpha, backdrop_alpha) in [(1.0f64, 1.0f64), (0.5, 0.5)] {
            let cs = [65504.0f64, -65504.0, 0.0];
            let cb = [65504.0f64, -65504.0, 65504.0];
            let solid = |rgb: [f64; 3], alpha: f64, hash| IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: (rgb[0] * alpha) as f32,
                        g: (rgb[1] * alpha) as f32,
                        b: (rgb[2] * alpha) as f32,
                        a: alpha as f32,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(hash),
            };
            let graph = FrameGraph {
                working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                nodes: vec![
                    solid(cs, source_alpha, id),
                    solid(cb, backdrop_alpha, id + 1),
                    IrNode {
                        op: IrOp::NativeArithmeticMerge { mode, opacity: 1.0 },
                        inputs: vec![
                            (IrNodeId(0), OutPort::default()),
                            (IrNodeId(1), OutPort::default()),
                        ],
                        content_hash: ContentHash(id + 2),
                    },
                ],
                output: Some(IrNodeId(2)),
            };
            id += 3;
            let cpu = eval_cpu::evaluate(&graph, (1, 1), &mut EmptyProvider);
            let frame = evaluator
                .evaluate(&graph, (1, 1), &mut NullFrameSource)
                .unwrap();
            let gpu_pixel = read_texture_rgba16f(&gpu, &frame, 1, 1)[0];
            for c in 0..3 {
                let blended = match mode {
                    LinearDodge => cb[c] + cs[c],
                    LinearBurn => cb[c] + cs[c] - 1.0,
                    Subtract => cb[c] - cs[c],
                    LinearLight => cb[c] + 2.0 * cs[c] - 1.0,
                    _ => unreachable!(),
                };
                let expected = (source_alpha
                    * ((1.0 - backdrop_alpha) * cs[c] + backdrop_alpha * blended)
                    + (1.0 - source_alpha) * backdrop_alpha * cb[c])
                    .clamp(-65504.0, 65504.0);
                assert!((cpu.pixels[0][c] as f64 - expected).abs() < 0.01);
                assert!(
                    gpu_pixel[c].is_finite() && (gpu_pixel[c] as f64 - expected).abs() <= 32.0,
                    "{mode:?} {gpu_pixel:?} vs {expected}"
                );
            }
            assert_eq!(
                gpu_pixel[3] as f64,
                source_alpha + backdrop_alpha * (1.0 - source_alpha)
            );
            assert_eq!(cpu.pixels[0][3], gpu_pixel[3]);
        }
        // The same serialized blend choice retains its bounded Legacy equation.
        let source = [0.75f32, 0.25, 0.5, 1.0];
        let bottom = [0.75f32, 0.25, 0.125, 1.0];
        let mut top_image = photonic_video::graph::ops::Image::new(1, 1);
        top_image.pixels[0] = source;
        let mut bottom_image = photonic_video::graph::ops::Image::new(1, 1);
        bottom_image.pixels[0] = bottom;
        let legacy = photonic_video::graph::ops::merge(&top_image, &bottom_image, mode, 1.0);
        let native = photonic_video::graph::ops::native_arithmetic_merge(
            &top_image,
            &bottom_image,
            mode,
            1.0,
        );
        let mut differs = false;
        for c in 0..3 {
            let raw = match mode {
                LinearDodge => bottom[c] + source[c],
                LinearBurn => bottom[c] + source[c] - 1.0,
                Subtract => bottom[c] - source[c],
                LinearLight => bottom[c] + 2.0 * source[c] - 1.0,
                _ => unreachable!(),
            };
            let expected = match mode {
                LinearDodge => raw.min(1.0),
                LinearBurn | Subtract => raw.max(0.0),
                LinearLight => raw.clamp(0.0, 1.0),
                _ => unreachable!(),
            };
            assert_eq!(legacy.pixels[0][c], expected);
            assert_eq!(native.pixels[0][c], raw);
            differs |= raw != expected;
        }
        assert!(differs, "fixture must distinguish {mode:?} semantics");
    }
}

/// Native affine equations are intentionally distinct from bounded Legacy blends.
#[test]
fn native_nonseparable_blends_match_ap1_ray_reference_and_alpha() {
    use photonic_core::layer::BlendMode::*;
    let gpu = GpuContext::request_blocking().expect("GPU required for native blend qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    // Independent geometric reference: form a chroma vector, shift its Y,
    // then intersect the ray from neutral target-Y with the unit RGB cube.
    let oracle =
        |mode, cs: [f64; 3], cb: [f64; 3], source_alpha: f64, backdrop_alpha: f64, opacity: f64| {
            let y = |c: [f64; 3]| c[0] * 0.27222872 + c[1] * 0.67408174 + c[2] * 0.053689517;
            let range = |c: [f64; 3]| {
                c.into_iter().fold(f64::NEG_INFINITY, f64::max)
                    - c.into_iter().fold(f64::INFINITY, f64::min)
            };
            let b = cb.map(|v| v.clamp(0.0, 1.0));
            let source = cs.map(|v| v.clamp(0.0, 1.0));
            let target = if mode == Luminosity { y(source) } else { y(b) };
            let base = if matches!(mode, Hue | Color) {
                source
            } else {
                b
            };
            let base = if matches!(mode, Hue | Saturation) {
                let lo = base.into_iter().fold(f64::INFINITY, f64::min);
                let span = range(base);
                let chroma = if mode == Hue { range(b) } else { range(source) };
                if span > 0.0 {
                    base.map(|v| (v - lo) * chroma / span)
                } else {
                    [0.0; 3]
                }
            } else {
                base
            };
            let candidate = base.map(|v| v + target - y(base));
            let mut factor = 1.0f64;
            for value in candidate {
                if value < 0.0 {
                    factor = factor.min(target / (target - value));
                }
                if value > 1.0 {
                    factor = factor.min((1.0 - target) / (value - target));
                }
            }
            let bounded = candidate.map(|v| (target + factor * (v - target)).clamp(0.0, 1.0));
            let a_s = source_alpha * opacity;
            let mut out = [0.0; 4];
            for c in 0..3 {
                out[c] = (a_s * ((1.0 - backdrop_alpha) * cs[c] + backdrop_alpha * bounded[c])
                    + (1.0 - a_s) * backdrop_alpha * cb[c])
                    .clamp(-65504.0, 65504.0);
            }
            out[3] = a_s + backdrop_alpha * (1.0 - a_s);
            out
        };
    let mut identity = 100000u128;
    for mode in [Hue, Saturation, Color, Luminosity] {
        for (source_alpha, backdrop_alpha) in
            [(1.0, 1.0), (0.25, 0.5), (0.0, 1.0), (1.0, 0.0), (0.0, 0.0)]
        {
            for opacity in [0.0, 0.5, 1.0] {
                for (cs, cb) in [
                    ([-0.5f64, 2.0, 0.25], [1.5f64, -0.25, 4.0]),
                    ([0.499, 0.5, 0.501], [0.501, 0.5, 0.499]),
                    ([0.0, 1.0, -1.0], [1.0, 0.0, 2.0]),
                    ([0.125, 0.625, 0.875], [0.875, 0.375, 0.125]),
                    ([0.499, 0.5, 0.501], [0.249, 0.25, 0.251]),
                    ([0.75, 1.5, 0.5], [-0.125, 0.25, 0.0]),
                    ([65504.0, -65504.0, 0.0], [65504.0, -65504.0, 65504.0]),
                    ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
                    ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
                    ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
                    ([0.5, 0.5, 0.5], [0.125, 0.5, 0.875]),
                    ([0.125, 0.5, 0.875], [0.5, 0.5, 0.5]),
                ] {
                    let expected = oracle(mode, cs, cb, source_alpha, backdrop_alpha, opacity);
                    identity += 3;
                    let solid = |rgb: [f64; 3], alpha: f64, hash| IrNode {
                        op: IrOp::SolidColor {
                            color: LinearColor {
                                r: (rgb[0] * alpha) as f32,
                                g: (rgb[1] * alpha) as f32,
                                b: (rgb[2] * alpha) as f32,
                                a: alpha as f32,
                            },
                        },
                        inputs: vec![],
                        content_hash: ContentHash(hash),
                    };
                    let graph = FrameGraph {
                        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                        nodes: vec![
                            solid(cs, source_alpha, identity),
                            solid(cb, backdrop_alpha, identity + 1),
                            IrNode {
                                op: IrOp::NativeBoundedMerge {
                                    mode,
                                    opacity: opacity as f32,
                                },
                                inputs: vec![
                                    (IrNodeId(0), OutPort::default()),
                                    (IrNodeId(1), OutPort::default()),
                                ],
                                content_hash: ContentHash(identity + 2),
                            },
                        ],
                        output: Some(IrNodeId(2)),
                    };
                    assert!(graph.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&graph, (1, 1), &mut EmptyProvider);
                    let texture = evaluator
                        .evaluate(&graph, (1, 1), &mut NullFrameSource)
                        .unwrap();
                    let pixels = read_texture_rgba16f(&gpu, &texture, 1, 1);
                    // Independently account for valid RGBA16F source quantization.
                    // Read actual source textures and check their quantization
                    // independently before using a f64 oracle for GPU arithmetic.
                    let mut source_graph = graph.clone();
                    let mut stored = [[0.0f32; 4]; 2];
                    for (index, stored_pixel) in stored.iter_mut().enumerate() {
                        source_graph.output = Some(IrNodeId(index as u32));
                        let source_frame = evaluator
                            .evaluate(&source_graph, (1, 1), &mut NullFrameSource)
                            .unwrap();
                        *stored_pixel = read_texture_rgba16f(&gpu, &source_frame, 1, 1)[0];
                        let (original, alpha) = if index == 0 {
                            (cs, source_alpha)
                        } else {
                            (cb, backdrop_alpha)
                        };
                        for c in 0..3 {
                            assert!(
                                (f64::from(stored_pixel[c]) - original[c] * alpha).abs() < 0.004
                            );
                        }
                        assert_eq!(f64::from(stored_pixel[3]), alpha);
                    }
                    let straight = |p: [f32; 4]| {
                        if p[3] > 1e-6 {
                            std::array::from_fn(|c| f64::from(p[c]) / f64::from(p[3]))
                        } else {
                            [0.0; 3]
                        }
                    };
                    let expected_gpu = oracle(
                        mode,
                        straight(stored[0]),
                        straight(stored[1]),
                        f64::from(stored[0][3]),
                        f64::from(stored[1][3]),
                        opacity,
                    );
                    for channel in 0..4 {
                        assert!((cpu.pixels[0][channel] as f64 - expected[channel]).abs() < if expected[channel].abs() > 10000.0 {0.004} else {2e-6},
                        "CPU {mode:?} alpha {source_alpha}/{backdrop_alpha} opacity {opacity}: {:?} vs {expected:?}", cpu.pixels[0]);
                        assert!((pixels[0][channel] as f64 - expected_gpu[channel]).abs() < if expected_gpu[channel].abs() > 10000.0 {32.0} else {0.01},
                        "GPU {mode:?} alpha {source_alpha}/{backdrop_alpha} opacity {opacity}: {:?} vs {expected_gpu:?}", pixels[0]);
                    }
                }
            }
        }
    }
}
#[test]
fn native_bounded_blends_preserve_nonoverlap_and_match_independent_oracle() {
    use photonic_core::layer::BlendMode::*;
    let gpu = GpuContext::request_blocking().expect("GPU required for native blend qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let oracle =
        |mode, cs: [f64; 3], cb: [f64; 3], source_alpha: f64, backdrop_alpha: f64, opacity: f64| {
            let a_s = source_alpha * opacity;
            let mut expected = [0.0; 4];
            for c in 0..3 {
                let source = cs[c].clamp(0.0, 1.0);
                let backdrop = cb[c].clamp(0.0, 1.0);
                let dodge = |b: f64, s: f64| {
                    if b == 0.0 {
                        0.0
                    } else if s == 1.0 {
                        1.0
                    } else {
                        (b / (1.0 - s)).min(1.0)
                    }
                };
                let burn = |b: f64, s: f64| {
                    if b == 1.0 {
                        1.0
                    } else if s == 0.0 {
                        0.0
                    } else {
                        (1.0 - (1.0 - b) / s).max(0.0)
                    }
                };
                let vivid = if source <= 0.5 {
                    burn(backdrop, 2.0 * source)
                } else {
                    dodge(backdrop, 2.0 * source - 1.0)
                };
                let blended = match mode {
                    ColorDodge => dodge(backdrop, source),
                    ColorBurn => burn(backdrop, source),
                    Divide => {
                        if source == 0.0 {
                            1.0
                        } else {
                            (backdrop / source).min(1.0)
                        }
                    }
                    VividLight => vivid,
                    // Exact mathematical threshold, including VividLight's
                    // special black-backdrop / white-source discontinuity.
                    HardMix => {
                        if (backdrop == 0.0 && source == 1.0) || backdrop + source < 1.0 {
                            0.0
                        } else {
                            1.0
                        }
                    }
                    _ => unreachable!(),
                };
                expected[c] = (a_s * ((1.0 - backdrop_alpha) * cs[c] + backdrop_alpha * blended)
                    + (1.0 - a_s) * backdrop_alpha * cb[c])
                    .clamp(-65504.0, 65504.0);
            }
            expected[3] = a_s + backdrop_alpha * (1.0 - a_s);
            expected
        };
    let mut identity = 85000u128;
    for mode in [ColorDodge, ColorBurn, Divide, VividLight, HardMix] {
        for (source_alpha, backdrop_alpha) in
            [(1.0, 1.0), (0.25, 0.5), (0.0, 1.0), (1.0, 0.0), (0.0, 0.0)]
        {
            for opacity in [0.0, 0.5, 1.0] {
                for (cs, cb) in [
                    ([-0.5f64, 2.0, 0.25], [1.5f64, -0.25, 4.0]),
                    ([0.499, 0.5, 0.501], [0.501, 0.5, 0.499]),
                    ([0.0, 1.0, -1.0], [1.0, 0.0, 2.0]),
                    ([0.125, 0.625, 0.875], [0.875, 0.375, 0.125]),
                    ([0.499, 0.5, 0.501], [0.249, 0.25, 0.251]),
                    ([0.75, 1.5, 0.5], [-0.125, 0.25, 0.0]),
                    ([65504.0, -65504.0, 0.0], [65504.0, -65504.0, 65504.0]),
                ] {
                    let expected = oracle(mode, cs, cb, source_alpha, backdrop_alpha, opacity);
                    identity += 3;
                    let solid = |rgb: [f64; 3], alpha: f64, hash| IrNode {
                        op: IrOp::SolidColor {
                            color: LinearColor {
                                r: (rgb[0] * alpha) as f32,
                                g: (rgb[1] * alpha) as f32,
                                b: (rgb[2] * alpha) as f32,
                                a: alpha as f32,
                            },
                        },
                        inputs: vec![],
                        content_hash: ContentHash(hash),
                    };
                    let graph = FrameGraph {
                        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                        nodes: vec![
                            solid(cs, source_alpha, identity),
                            solid(cb, backdrop_alpha, identity + 1),
                            IrNode {
                                op: IrOp::NativeBoundedMerge {
                                    mode,
                                    opacity: opacity as f32,
                                },
                                inputs: vec![
                                    (IrNodeId(0), OutPort::default()),
                                    (IrNodeId(1), OutPort::default()),
                                ],
                                content_hash: ContentHash(identity + 2),
                            },
                        ],
                        output: Some(IrNodeId(2)),
                    };
                    assert!(graph.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&graph, (1, 1), &mut EmptyProvider);
                    let texture = evaluator
                        .evaluate(&graph, (1, 1), &mut NullFrameSource)
                        .unwrap();
                    let pixels = read_texture_rgba16f(&gpu, &texture, 1, 1);
                    // Hard thresholds amplify even valid half-storage rounding.
                    // Read actual source textures and check their quantization
                    // independently before using a f64 oracle for GPU arithmetic.
                    let mut source_graph = graph.clone();
                    let mut stored = [[0.0f32; 4]; 2];
                    for (index, stored_pixel) in stored.iter_mut().enumerate() {
                        source_graph.output = Some(IrNodeId(index as u32));
                        let source_frame = evaluator
                            .evaluate(&source_graph, (1, 1), &mut NullFrameSource)
                            .unwrap();
                        *stored_pixel = read_texture_rgba16f(&gpu, &source_frame, 1, 1)[0];
                        let (original, alpha) = if index == 0 {
                            (cs, source_alpha)
                        } else {
                            (cb, backdrop_alpha)
                        };
                        for c in 0..3 {
                            assert!(
                                (f64::from(stored_pixel[c]) - original[c] * alpha).abs() < 0.004
                            );
                        }
                        assert_eq!(f64::from(stored_pixel[3]), alpha);
                    }
                    let straight = |p: [f32; 4]| {
                        if p[3] > 1e-6 {
                            std::array::from_fn(|c| f64::from(p[c]) / f64::from(p[3]))
                        } else {
                            [0.0; 3]
                        }
                    };
                    let expected_gpu = oracle(
                        mode,
                        straight(stored[0]),
                        straight(stored[1]),
                        f64::from(stored[0][3]),
                        f64::from(stored[1][3]),
                        opacity,
                    );
                    for channel in 0..4 {
                        assert!((cpu.pixels[0][channel] as f64 - expected[channel]).abs() < 1e-6,
                        "CPU {mode:?} alpha {source_alpha}/{backdrop_alpha} opacity {opacity}: {:?} vs {expected:?}", cpu.pixels[0]);
                        assert!((pixels[0][channel] as f64 - expected_gpu[channel]).abs() < if expected_gpu[channel].abs() > 10000.0 {32.0} else {0.01},
                        "GPU {mode:?} alpha {source_alpha}/{backdrop_alpha} opacity {opacity}: {:?} vs {expected_gpu:?}", pixels[0]);
                    }
                }
            }
        }
    }
}

#[test]
fn native_arithmetic_blends_preserve_extended_color_and_alpha() {
    use photonic_core::layer::BlendMode::*;
    let gpu = GpuContext::request_blocking().expect("GPU required for native blend qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut identity = 30000u128;
    for mode in [LinearDodge, LinearBurn, Subtract, LinearLight] {
        for (source_alpha, backdrop_alpha) in
            [(1.0, 1.0), (0.25, 0.5), (0.0, 1.0), (1.0, 0.0), (0.0, 0.0)]
        {
            for opacity in [0.0, 0.5, 1.0] {
                for (cs, cb) in [
                    ([-0.5f64, 2.0, 0.25], [1.5f64, -0.25, 4.0]),
                    ([0.499, 0.5, 0.501], [0.501, 0.5, 0.499]),
                    ([0.0, 1.0, -1.0], [1.0, 0.0, 2.0]),
                    ([0.2, 0.7, 0.8], [0.8, 0.3, 0.2]),
                    ([0.499, 0.5, 0.501], [0.249, 0.25, 0.251]),
                    ([0.75, 1.5, 0.5], [-0.125, 0.25, 0.0]),
                ] {
                    let a_s = source_alpha * opacity;
                    let mut expected = [0f64; 4];
                    for c in 0..3 {
                        // Independent f64 affine oracle; implementation uses
                        // separate f32 CPU and WGSL arithmetic paths.
                        let (source_gain, constant) = match mode {
                            LinearDodge => (1.0, 0.0),
                            LinearBurn => (1.0, -1.0),
                            Subtract => (-1.0, 0.0),
                            LinearLight => (2.0, -1.0),
                            _ => unreachable!(),
                        };
                        let b = cb[c] + source_gain * cs[c] + constant;
                        expected[c] = a_s * ((1.0 - backdrop_alpha) * cs[c] + backdrop_alpha * b)
                            + (1.0 - a_s) * backdrop_alpha * cb[c];
                    }
                    expected[3] = a_s + backdrop_alpha * (1.0 - a_s);
                    identity += 3;
                    let solid = |rgb: [f64; 3], alpha: f64, hash| IrNode {
                        op: IrOp::SolidColor {
                            color: LinearColor {
                                r: (rgb[0] * alpha) as f32,
                                g: (rgb[1] * alpha) as f32,
                                b: (rgb[2] * alpha) as f32,
                                a: alpha as f32,
                            },
                        },
                        inputs: vec![],
                        content_hash: ContentHash(hash),
                    };
                    let graph = FrameGraph {
                        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                        nodes: vec![
                            solid(cs, source_alpha, identity),
                            solid(cb, backdrop_alpha, identity + 1),
                            IrNode {
                                op: IrOp::NativeArithmeticMerge {
                                    mode,
                                    opacity: opacity as f32,
                                },
                                inputs: vec![
                                    (IrNodeId(0), OutPort::default()),
                                    (IrNodeId(1), OutPort::default()),
                                ],
                                content_hash: ContentHash(identity + 2),
                            },
                        ],
                        output: Some(IrNodeId(2)),
                    };
                    assert!(graph.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&graph, (1, 1), &mut EmptyProvider);
                    let texture = evaluator
                        .evaluate(&graph, (1, 1), &mut NullFrameSource)
                        .unwrap();
                    let pixels = read_texture_rgba16f(&gpu, &texture, 1, 1);
                    for channel in 0..4 {
                        assert!((cpu.pixels[0][channel] as f64 - expected[channel]).abs() < 1e-6,
                        "CPU {mode:?} alpha {source_alpha}/{backdrop_alpha} opacity {opacity}: {:?} vs {expected:?}", cpu.pixels[0]);
                        assert!((pixels[0][channel] as f64 - expected[channel]).abs() < 0.01,
                        "GPU {mode:?} alpha {source_alpha}/{backdrop_alpha} opacity {opacity}: {:?} vs {expected:?}", pixels[0]);
                    }
                }
            }
        }
    }
}

#[test]
fn native_luminance_blends_select_whole_extended_rgb_and_preserve_alpha() {
    use photonic_core::layer::BlendMode::*;
    let gpu = GpuContext::request_blocking().expect("GPU required for native blend qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut identity = 30000u128;
    for mode in [DarkerColor, LighterColor] {
        for (source_alpha, backdrop_alpha) in
            [(1.0, 1.0), (0.25, 0.5), (0.0, 1.0), (1.0, 0.0), (0.0, 0.0)]
        {
            for opacity in [0.0, 0.5, 1.0] {
                for (cs, cb) in [
                    ([-0.5f64, 2.0, 0.25], [1.5f64, -0.25, 4.0]),
                    ([0.499, 0.5, 0.501], [0.501, 0.5, 0.499]),
                    ([0.0, 1.0, -1.0], [1.0, 0.0, 2.0]),
                    ([0.2, 0.7, 0.8], [0.8, 0.3, 0.2]),
                    ([0.499, 0.5, 0.501], [0.249, 0.25, 0.251]),
                    ([0.75, 1.5, 0.5], [-0.125, 0.25, 0.0]),
                    ([0.0, 0.0, 1.0], [0.25, 0.0, 0.0]), // AP1 order reverses Legacy luma order
                    ([1.0, -0.5, 4.0], [1.0, -0.5, 4.0]), // exact identical-color tie
                ] {
                    let a_s = source_alpha * opacity;
                    let mut expected = [0f64; 4];
                    // Independently pinned high-precision AP1-to-XYZ Y row.
                    // Derived from published AP1 primaries/white in scene_measurement tests.
                    let y = |rgb: [f64; 3]| {
                        0.27222871678091454 * rgb[0]
                            + 0.6740817658111484 * rgb[1]
                            + 0.05368951740793705 * rgb[2]
                    };
                    let choose_backdrop = match mode {
                        DarkerColor => y(cb) <= y(cs),
                        LighterColor => y(cb) >= y(cs),
                        _ => unreachable!(),
                    };
                    let blended = if choose_backdrop { cb } else { cs };
                    for c in 0..3 {
                        expected[c] = a_s
                            * ((1.0 - backdrop_alpha) * cs[c] + backdrop_alpha * blended[c])
                            + (1.0 - a_s) * backdrop_alpha * cb[c];
                    }
                    expected[3] = a_s + backdrop_alpha * (1.0 - a_s);
                    identity += 3;
                    let solid = |rgb: [f64; 3], alpha: f64, hash| IrNode {
                        op: IrOp::SolidColor {
                            color: LinearColor {
                                r: (rgb[0] * alpha) as f32,
                                g: (rgb[1] * alpha) as f32,
                                b: (rgb[2] * alpha) as f32,
                                a: alpha as f32,
                            },
                        },
                        inputs: vec![],
                        content_hash: ContentHash(hash),
                    };
                    let graph = FrameGraph {
                        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                        nodes: vec![
                            solid(cs, source_alpha, identity),
                            solid(cb, backdrop_alpha, identity + 1),
                            IrNode {
                                op: IrOp::NativeLuminanceMerge {
                                    mode,
                                    opacity: opacity as f32,
                                },
                                inputs: vec![
                                    (IrNodeId(0), OutPort::default()),
                                    (IrNodeId(1), OutPort::default()),
                                ],
                                content_hash: ContentHash(identity + 2),
                            },
                        ],
                        output: Some(IrNodeId(2)),
                    };
                    assert!(graph.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&graph, (1, 1), &mut EmptyProvider);
                    let texture = evaluator
                        .evaluate(&graph, (1, 1), &mut NullFrameSource)
                        .unwrap();
                    let pixels = read_texture_rgba16f(&gpu, &texture, 1, 1);
                    for channel in 0..4 {
                        assert!((cpu.pixels[0][channel] as f64 - expected[channel]).abs() < 1e-6,
                        "CPU {mode:?} alpha {source_alpha}/{backdrop_alpha} opacity {opacity}: {:?} vs {expected:?}", cpu.pixels[0]);
                        assert!((pixels[0][channel] as f64 - expected[channel]).abs() < 0.01,
                        "GPU {mode:?} alpha {source_alpha}/{backdrop_alpha} opacity {opacity}: {:?} vs {expected:?}", pixels[0]);
                    }
                }
            }
        }
    }
}

#[test]
fn native_crop_and_transform_keep_extended_premultiplied_pixels_and_transparent_edges() {
    let gpu =
        GpuContext::request_blocking().expect("GPU required for native geometry qualification");
    let mut graph = FrameGraph {
        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
        nodes: vec![
            IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: -0.25,
                        g: 1.5,
                        b: 0.125,
                        a: 0.5,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(40001),
            },
            IrNode {
                op: IrOp::Crop {
                    left: 0.25,
                    top: 1.0 / 6.0,
                    right: 0.125,
                    bottom: 1.0 / 6.0,
                },
                inputs: vec![(IrNodeId(0), OutPort::default())],
                content_hash: ContentHash(40002),
            },
            IrNode {
                op: IrOp::Transform2DTransparent {
                    mat: glam::Mat3::from_translation(glam::Vec2::new(1.0, 0.0)),
                    sampling: Sampling::Bilinear,
                },
                inputs: vec![(IrNodeId(1), OutPort::default())],
                content_hash: ContentHash(40003),
            },
        ],
        output: Some(IrNodeId(2)),
    };
    assert!(graph.validate_working_color_domain().is_ok());
    let cpu = eval_cpu::evaluate(&graph, (8, 6), &mut EmptyProvider);
    let texture = Evaluator::new(gpu.clone())
        .evaluate(&graph, (8, 6), &mut NullFrameSource)
        .unwrap();
    let pixels = read_texture_rgba16f(&gpu, &texture, 8, 6);
    for y in 0..6usize {
        for x in 0..8usize {
            let expected = if x >= 3 && (1..5).contains(&y) {
                [-0.25, 1.5, 0.125, 0.5]
            } else {
                [0.0; 4]
            };
            for channel in 0..4 {
                assert!(
                    (cpu.pixels[y * 8 + x][channel] - expected[channel]).abs() < 1e-5,
                    "CPU {x},{y}: {:?} vs {expected:?}",
                    cpu.pixels[y * 8 + x]
                );
                assert!(
                    (pixels[y * 8 + x][channel] - expected[channel]).abs() < 0.002,
                    "GPU {x},{y}: {:?} vs {expected:?}",
                    pixels[y * 8 + x]
                );
            }
        }
    }
    graph.nodes[2].op = IrOp::Transform2DTransparent {
        mat: glam::Mat3::ZERO,
        sampling: Sampling::Bilinear,
    };
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes.pop();
    graph.output = Some(IrNodeId(1));
    graph.nodes[1].op = IrOp::Crop {
        left: f32::NAN,
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
    };
    assert!(graph.validate_working_color_domain().is_err());
}

#[test]
fn native_draft_canvas_scales_translation_and_blur_without_upsampling_or_stale_cache() {
    let gpu = GpuContext::request_blocking().expect("GPU required for native Draft qualification");
    let make = |translation, sigma, authored: bool| {
        let mut nodes = vec![
            IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: -0.25,
                        g: 1.5,
                        b: 0.125,
                        a: 0.5,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(50001),
            },
            IrNode {
                op: IrOp::Transform2DTransparent {
                    mat: glam::Mat3::from_translation(glam::Vec2::new(translation, 0.0)),
                    sampling: Sampling::Bilinear,
                },
                inputs: vec![(IrNodeId(0), OutPort::default())],
                content_hash: ContentHash(50002),
            },
            IrNode {
                op: IrOp::NativeGaussianBlur { sigma },
                inputs: vec![(IrNodeId(1), OutPort::default())],
                content_hash: ContentHash(50003),
            },
        ];
        if authored {
            nodes.push(IrNode {
                op: IrOp::NativeCanvas { w: 16, h: 16 },
                inputs: vec![(IrNodeId(2), OutPort::default())],
                content_hash: ContentHash(50004),
            });
        }
        FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            output: Some(IrNodeId(nodes.len() as u32 - 1)),
            nodes,
        }
    };
    let graph = make(4.0, 2.0, true);
    let mut evaluator = Evaluator::new(gpu.clone());
    for size in [8, 16, 8] {
        let ratio = size as f32 / 16.0;
        let reference = make(4.0 * ratio, 2.0 * ratio, false);
        let expected = eval_cpu::evaluate(&reference, (size, size), &mut EmptyProvider);
        let cpu = eval_cpu::evaluate(&graph, (size, size), &mut EmptyProvider);
        assert_eq!((cpu.width, cpu.height), (size, size));
        let frame = evaluator
            .evaluate_with_tap_frame(&graph, (size, size), &mut NullFrameSource, None)
            .0
            .unwrap();
        assert_eq!((frame.width, frame.height), (size, size));
        let pixels = read_texture_rgba16f(&gpu, &frame.texture, size, size);
        for ((cpu, actual), expected) in cpu.pixels.iter().zip(pixels).zip(expected.pixels) {
            for channel in 0..4 {
                assert!(
                    (cpu[channel] - expected[channel]).abs() < 1e-5,
                    "CPU size {size}: {cpu:?} vs {expected:?}"
                );
                assert!(
                    (actual[channel] - expected[channel]).abs() < 0.005,
                    "GPU size {size}: {actual:?} vs {expected:?}"
                );
            }
        }
    }
}

#[test]
fn native_dissolve_interpolates_premultiplied_color_and_coverage() {
    let gpu =
        GpuContext::request_blocking().expect("GPU required for native dissolve qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 60000;
    for (incoming_alpha, outgoing_alpha) in
        [(1.0, 1.0), (0.5, 0.25), (0.0, 1.0), (1.0, 0.0), (0.0, 0.0)]
    {
        let incoming = [
            -0.25 * incoming_alpha,
            2.0 * incoming_alpha,
            0.5 * incoming_alpha,
            incoming_alpha,
        ];
        let outgoing = [
            0.75 * outgoing_alpha,
            -0.5 * outgoing_alpha,
            3.0 * outgoing_alpha,
            outgoing_alpha,
        ];
        for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
            hash += 3;
            let solid = |p: [f32; 4], h| IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: p[0],
                        g: p[1],
                        b: p[2],
                        a: p[3],
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(h),
            };
            let mut graph = FrameGraph {
                working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                nodes: vec![
                    solid(incoming, hash),
                    solid(outgoing, hash + 1),
                    IrNode {
                        op: IrOp::NativeDissolve { t },
                        inputs: vec![
                            (IrNodeId(0), OutPort::default()),
                            (IrNodeId(1), OutPort::default()),
                        ],
                        content_hash: ContentHash(hash + 2),
                    },
                ],
                output: Some(IrNodeId(2)),
            };
            assert!(graph.validate_working_color_domain().is_ok());
            let cpu = eval_cpu::evaluate(&graph, (1, 1), &mut EmptyProvider);
            let texture = evaluator
                .evaluate(&graph, (1, 1), &mut NullFrameSource)
                .unwrap();
            let pixel = read_texture_rgba16f(&gpu, &texture, 1, 1)[0];
            for c in 0..4 {
                let expected =
                    outgoing[c] as f64 * (1.0 - t as f64) + incoming[c] as f64 * t as f64;
                assert!((cpu.pixels[0][c] as f64 - expected).abs() < 1e-6);
                assert!(
                    (pixel[c] as f64 - expected).abs() < 0.003,
                    "t {t}, alpha {incoming_alpha}/{outgoing_alpha}: {pixel:?}"
                );
            }
            graph.nodes[2].op = IrOp::NativeDissolve { t: f32::NAN };
            assert!(graph.validate_working_color_domain().is_err());
            graph.nodes[2].op = IrOp::NativeDissolve { t };
            graph.working_color_domain = WorkingColorDomain::LegacyLinearRec709;
            assert!(graph.validate_working_color_domain().is_err());
        }
    }
}

#[test]
fn native_spatial_transitions_match_independent_extended_rgba_maps_and_endpoints() {
    use photonic_video::graph::{ir::WipeDirection, luma_wipe::LumaWipeKind};
    let gpu = GpuContext::request_blocking()
        .expect("GPU required for native spatial transition qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let incoming = [-0.25f32, 1.5, 0.125, 0.5];
    let outgoing = [0.75f32, -0.125, 2.0, 0.25];
    let mut cases = Vec::new();
    for t in [0.0f32, 0.37, 1.0] {
        for direction in [
            WipeDirection::LeftToRight,
            WipeDirection::RightToLeft,
            WipeDirection::TopToBottom,
            WipeDirection::BottomToTop,
        ] {
            cases.push(IrOp::PushMix { direction, t });
            for softness in [0.0, 0.2] {
                cases.push(IrOp::WipeMix {
                    direction,
                    softness,
                    t,
                });
            }
        }
        for kind in [
            LumaWipeKind::LinearH,
            LumaWipeKind::LinearV,
            LumaWipeKind::Radial,
            LumaWipeKind::BarnDoorH,
            LumaWipeKind::Clock,
        ] {
            for invert in [false, true] {
                for softness in [0.0, 0.2] {
                    cases.push(IrOp::NativeLumaWipe {
                        kind,
                        softness,
                        invert,
                        t,
                    });
                }
            }
        }
    }
    let smooth = |value: f64| {
        let x = value.clamp(0.0, 1.0);
        x * x * (3.0 - 2.0 * x)
    };
    for (index, op) in cases.into_iter().enumerate() {
        let hash = 70000 + index as u128 * 3;
        let solid = |p: [f32; 4], h| IrNode {
            op: IrOp::SolidColor {
                color: LinearColor {
                    r: p[0],
                    g: p[1],
                    b: p[2],
                    a: p[3],
                },
            },
            inputs: vec![],
            content_hash: ContentHash(h),
        };
        let graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![
                solid(incoming, hash),
                solid(outgoing, hash + 1),
                IrNode {
                    op: op.clone(),
                    inputs: vec![
                        (IrNodeId(0), OutPort::default()),
                        (IrNodeId(1), OutPort::default()),
                    ],
                    content_hash: ContentHash(hash + 2),
                },
            ],
            output: Some(IrNodeId(2)),
        };
        assert!(graph.validate_working_color_domain().is_ok());
        // Odd dimensions exercise the exact center (zero-valued radial/barn-door maps).
        let cpu = eval_cpu::evaluate(&graph, (5, 5), &mut EmptyProvider);
        let texture = evaluator
            .evaluate(&graph, (5, 5), &mut NullFrameSource)
            .unwrap();
        let pixels = read_texture_rgba16f(&gpu, &texture, 5, 5);
        for y in 0..5usize {
            for x in 0..5usize {
                let u = (x as f64 + 0.5) / 5.0;
                let v = (y as f64 + 0.5) / 5.0;
                let coordinate = |direction| match direction {
                    WipeDirection::LeftToRight => u,
                    WipeDirection::RightToLeft => 1.0 - u,
                    WipeDirection::TopToBottom => v,
                    WipeDirection::BottomToTop => 1.0 - v,
                };
                let weight = match op {
                    IrOp::PushMix { direction, t } => {
                        if coordinate(direction) < t as f64 {
                            1.0
                        } else {
                            0.0
                        }
                    }
                    IrOp::WipeMix {
                        direction,
                        softness,
                        t,
                    } => {
                        let s = softness as f64;
                        let edge = -s + t as f64 * (1.0 + 2.0 * s);
                        if s == 0.0 {
                            if coordinate(direction) < edge {
                                1.0
                            } else if coordinate(direction) > edge {
                                0.0
                            } else {
                                0.5
                            }
                        } else {
                            1.0 - smooth((coordinate(direction) - (edge - s)) / (2.0 * s))
                        }
                    }
                    IrOp::NativeLumaWipe {
                        kind,
                        softness,
                        invert,
                        t,
                    } => {
                        let dx = u - 0.5;
                        let dy = v - 0.5;
                        let mut m = match kind {
                            LumaWipeKind::LinearH => u,
                            LumaWipeKind::LinearV => v,
                            LumaWipeKind::Radial => dx.hypot(dy) / 0.5_f64.sqrt(),
                            LumaWipeKind::BarnDoorH => (2.0 * u - 1.0).abs(),
                            LumaWipeKind::Clock => ((-dy.atan2(dx) + std::f64::consts::PI)
                                / std::f64::consts::TAU)
                                .rem_euclid(1.0),
                        }
                        .clamp(0.0, 1.0);
                        if invert {
                            m = 1.0 - m;
                        }
                        if t == 0.0 {
                            0.0
                        } else if t == 1.0 {
                            1.0
                        } else {
                            let s = softness as f64;
                            let edge = -s + t as f64 * (1.0 + 2.0 * s);
                            if s == 0.0 {
                                if edge >= m {
                                    1.0
                                } else {
                                    0.0
                                }
                            } else {
                                smooth((edge - (m - s)) / (2.0 * s))
                            }
                        }
                    }
                    _ => unreachable!(),
                };
                for c in 0..4 {
                    let expected =
                        outgoing[c] as f64 * (1.0 - weight) + incoming[c] as f64 * weight;
                    assert!(
                        (cpu.pixels[y * 5 + x][c] as f64 - expected).abs() < 1e-5,
                        "CPU {op:?} {x},{y}"
                    );
                    assert!(
                        (pixels[y * 5 + x][c] as f64 - expected).abs() < 0.006,
                        "GPU {op:?} {x},{y}: {:?} target {expected}",
                        pixels[y * 5 + x]
                    );
                }
            }
        }
    }
}

#[test]
fn native_unsharp_preserves_coverage_and_matches_independent_spatial_reference() {
    let gpu =
        GpuContext::request_blocking().expect("GPU required for native unsharp qualification");
    let mut serial = 90000u128;
    for sigma in [0.0_f32, 0.4, 1.0, 2.5, 12.0, 48.0, 100.0] {
        for amount in [0.0_f32, 0.5, 2.0, 10.0] {
            let mut add = |op, inputs: Vec<u32>| {
                serial += 1;
                IrNode {
                    op,
                    inputs: inputs
                        .into_iter()
                        .map(|i| (IrNodeId(i), OutPort::default()))
                        .collect(),
                    content_hash: ContentHash(serial),
                }
            };
            let mut graph = FrameGraph {
                working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                nodes: vec![
                    add(
                        IrOp::SolidColor {
                            color: LinearColor {
                                r: -0.25,
                                g: 1.5,
                                b: 0.125,
                                a: 0.5,
                            },
                        },
                        vec![],
                    ),
                    add(
                        IrOp::Crop {
                            left: 0.0,
                            top: 0.0,
                            right: 0.6,
                            bottom: 0.0,
                        },
                        vec![0],
                    ),
                    add(
                        IrOp::SolidColor {
                            color: LinearColor {
                                r: 0.75,
                                g: 0.0625,
                                b: -0.125,
                                a: 0.25,
                            },
                        },
                        vec![],
                    ),
                    add(
                        IrOp::Crop {
                            left: 0.6,
                            top: 0.0,
                            right: 0.0,
                            bottom: 0.0,
                        },
                        vec![2],
                    ),
                    add(
                        IrOp::Merge {
                            mode: BlendMode::Normal,
                            opacity: 1.0,
                        },
                        vec![3, 1],
                    ),
                    add(IrOp::NativeUnsharp { amount, sigma }, vec![4]),
                ],
                output: Some(IrNodeId(5)),
            };
            assert!(graph.validate_working_color_domain().is_ok());
            let original: Vec<[f64; 4]> = (0..17)
                .map(|x| {
                    if x < 7 {
                        [-0.25, 1.5, 0.125, 0.5]
                    } else if x >= 10 {
                        [0.75, 0.0625, -0.125, 0.25]
                    } else {
                        [0.0; 4]
                    }
                })
                .collect();
            // Independent f64 separable Gaussian plan. Rows are constant, so
            // vertical convolution is identity and only one row is needed.
            let mut low = original.clone();
            if sigma >= 0.5 {
                let n = if sigma <= 12.0 {
                    1
                } else {
                    ((sigma as f64 / 12.0).powi(2).ceil() as usize).clamp(2, 16)
                };
                let sp = sigma as f64 / (n as f64).sqrt();
                let step = ((sp * 3.0).ceil() / 128.0).ceil().clamp(1.0, 4.0);
                let radius = (((sp * 3.0).ceil() / step).ceil() as i32).min(128);
                let weights: Vec<_> = (-radius..=radius)
                    .map(|k| (-((k as f64 * step).powi(2)) / (2.0 * sp * sp)).exp())
                    .collect();
                let total: f64 = weights.iter().sum();
                for _ in 0..n {
                    low = (0..17)
                        .map(|x| {
                            let mut p = [0.0; 4];
                            for (i, w) in weights.iter().enumerate() {
                                let offset = ((i as i32 - radius) as f64 * step).round() as i32;
                                let sample = low[(x + offset).clamp(0, 16) as usize];
                                for c in 0..4 {
                                    p[c] += sample[c] * w / total;
                                }
                            }
                            p
                        })
                        .collect();
                }
            }
            let expected: Vec<_> = original
                .iter()
                .zip(&low)
                .map(|(src, low)| {
                    if amount == 0.0 || sigma < 0.5 {
                        return *src;
                    }
                    if src[3] == 0.0 {
                        return [0.0; 4];
                    }
                    let mut out = *src;
                    for c in 0..3 {
                        out[c] = src[c] + amount as f64 * (src[c] - low[c] * src[3] / low[3]);
                    }
                    out
                })
                .collect();
            let cpu = eval_cpu::evaluate(&graph, (17, 9), &mut EmptyProvider);
            let texture = Evaluator::new(gpu.clone())
                .evaluate(&graph, (17, 9), &mut NullFrameSource)
                .unwrap();
            let pixels = read_texture_rgba16f(&gpu, &texture, 17, 9);
            for (index, (cpu, gpu)) in cpu.pixels.iter().zip(&pixels).enumerate() {
                let expected = expected[index % 17];
                for c in 0..3 {
                    assert!(
                        (cpu[c] as f64 - expected[c]).abs() < 0.0003,
                        "CPU sigma{sigma} amount{amount}: {cpu:?} vs {expected:?}"
                    );
                    assert!(
                        (gpu[c] as f64 - expected[c]).abs() < 0.035 * expected[c].abs().max(1.0),
                        "GPU sigma{sigma} amount{amount}: {gpu:?} vs {expected:?}"
                    );
                }
                assert_eq!(cpu[3], expected[3] as f32);
                assert_eq!(gpu[3], expected[3] as f32);
            }
            for (amount, sigma) in [
                (f32::NAN, 1.0),
                (11.0, 1.0),
                (-1.0, 1.0),
                (1.0, 101.0),
                (1.0, f32::INFINITY),
            ] {
                graph.nodes[5].op = IrOp::NativeUnsharp { amount, sigma };
                assert!(graph.validate_working_color_domain().is_err());
            }
            graph.nodes[5].op = IrOp::NativeUnsharp {
                amount: 1.0,
                sigma: 1.0,
            };
            graph.nodes[5].inputs.clear();
            assert!(graph.validate_working_color_domain().is_err());
        }
    }
}

#[test]
fn native_unsharp_draft_radius_and_cache_follow_authored_canvas() {
    let gpu = GpuContext::request_blocking()
        .expect("GPU required for native unsharp Draft qualification");
    let make = |sigma, authored| {
        let mut nodes = vec![
            IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: -0.25,
                        g: 1.5,
                        b: 0.125,
                        a: 0.5,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(91001),
            },
            IrNode {
                op: IrOp::Crop {
                    left: 0.25,
                    top: 0.0,
                    right: 0.0,
                    bottom: 0.0,
                },
                inputs: vec![(IrNodeId(0), OutPort::default())],
                content_hash: ContentHash(91002),
            },
            IrNode {
                op: IrOp::NativeUnsharp { amount: 2.0, sigma },
                inputs: vec![(IrNodeId(1), OutPort::default())],
                content_hash: ContentHash(91003),
            },
        ];
        if authored {
            nodes.push(IrNode {
                op: IrOp::NativeCanvas { w: 34, h: 18 },
                inputs: vec![(IrNodeId(2), OutPort::default())],
                content_hash: ContentHash(91004),
            });
        }
        FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            output: Some(IrNodeId(nodes.len() as u32 - 1)),
            nodes,
        }
    };
    let graph = make(4.0, true);
    let mut evaluator = Evaluator::new(gpu.clone());
    for (w, h) in [(17, 9), (34, 18), (17, 9)] {
        let reference = make(4.0 * w as f32 / 34.0, false);
        let expected = eval_cpu::evaluate(&reference, (w, h), &mut EmptyProvider);
        let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
        let texture = evaluator
            .evaluate(&graph, (w, h), &mut NullFrameSource)
            .unwrap();
        let pixels = read_texture_rgba16f(&gpu, &texture, w, h);
        for ((actual, gpu), expected) in cpu.pixels.iter().zip(&pixels).zip(&expected.pixels) {
            for c in 0..4 {
                assert!((actual[c] - expected[c]).abs() < 1e-5);
                assert!((gpu[c] - expected[c]).abs() < 0.005);
            }
        }
    }
}

#[test]
fn native_invert_has_explicit_bounded_ap1_semantics_and_retains_alpha() {
    let gpu = GpuContext::request_blocking().expect("GPU required for native invert qualification");
    for alpha in [0.0_f32, 0.25, 0.5, 1.0] {
        for rgb in [[-1.0_f32, 0.25, 3.0], [0.0, 0.5, 1.0], [2.0, -0.25, 0.125]] {
            let mut graph = FrameGraph {
                working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                nodes: vec![
                    IrNode {
                        op: IrOp::SolidColor {
                            color: LinearColor {
                                r: rgb[0] * alpha,
                                g: rgb[1] * alpha,
                                b: rgb[2] * alpha,
                                a: alpha,
                            },
                        },
                        inputs: vec![],
                        content_hash: ContentHash(92001),
                    },
                    IrNode {
                        op: IrOp::NativeInvert,
                        inputs: vec![(IrNodeId(0), OutPort::default())],
                        content_hash: ContentHash(92002),
                    },
                ],
                output: Some(IrNodeId(1)),
            };
            assert!(graph.validate_working_color_domain().is_ok());
            let cpu = eval_cpu::evaluate(&graph, (17, 9), &mut EmptyProvider);
            let frame = Evaluator::new(gpu.clone())
                .evaluate(&graph, (17, 9), &mut NullFrameSource)
                .unwrap();
            let pixels = read_texture_rgba16f(&gpu, &frame, 17, 9);
            for pixel in cpu.pixels.iter().chain(&pixels) {
                for c in 0..3 {
                    let expected = (1.0 - (rgb[c] as f64).clamp(0.0, 1.0)) * alpha as f64;
                    assert!(
                        (pixel[c] as f64 - expected).abs() < 0.0015,
                        "{rgb:?}/{alpha}: {pixel:?}"
                    );
                }
                assert_eq!(pixel[3], alpha);
            }
            graph.nodes[1].inputs.clear();
            assert!(graph.validate_working_color_domain().is_err());
            graph.nodes[1].inputs = vec![(IrNodeId(0), OutPort::default())];
            graph.nodes[0].op = IrOp::GradeMatteConstant { weight: 0.5 };
            assert!(graph.validate_working_color_domain().is_err());
        }
    }
}

#[test]
fn native_luma_key_uses_ap1_luminance_and_preserves_extended_straight_color() {
    let gpu =
        GpuContext::request_blocking().expect("GPU required for native luma key qualification");
    for alpha in [0.0_f32, 0.25, 1.0] {
        for rgb in [
            [1.0_f32, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [-0.5, 0.5, 2.0],
            [2.0, 2.0, 2.0],
        ] {
            for (threshold, softness) in [(0.25_f32, 0.0_f32), (0.125, 0.5), (1.0, 1.0)] {
                for invert in [false, true] {
                    let mut graph = FrameGraph {
                        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                        nodes: vec![
                            IrNode {
                                op: IrOp::SolidColor {
                                    color: LinearColor {
                                        r: rgb[0] * alpha,
                                        g: rgb[1] * alpha,
                                        b: rgb[2] * alpha,
                                        a: alpha,
                                    },
                                },
                                inputs: vec![],
                                content_hash: ContentHash(93001),
                            },
                            IrNode {
                                op: IrOp::NativeLumaKey {
                                    threshold,
                                    softness,
                                    invert,
                                },
                                inputs: vec![(IrNodeId(0), OutPort::default())],
                                content_hash: ContentHash(93002),
                            },
                        ],
                        output: Some(IrNodeId(1)),
                    };
                    assert!(graph.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&graph, (17, 9), &mut EmptyProvider);
                    let frame = Evaluator::new(gpu.clone())
                        .evaluate(&graph, (17, 9), &mut NullFrameSource)
                        .unwrap();
                    let pixels = read_texture_rgba16f(&gpu, &frame, 17, 9);
                    let y = rgb[0] as f64 * 0.2722287168
                        + rgb[1] as f64 * 0.6740817658
                        + rgb[2] as f64 * 0.0536895174;
                    let t = ((y - threshold as f64) / (softness as f64).max(1e-4)).clamp(0.0, 1.0);
                    let k = t * t * (3.0 - 2.0 * t);
                    let keep = if invert { 1.0 - k } else { k };
                    for pixel in cpu.pixels.iter().chain(&pixels) {
                        for c in 0..4 {
                            let expected = if c == 3 {
                                alpha as f64 * keep
                            } else {
                                rgb[c] as f64 * alpha as f64 * keep
                            };
                            assert!((pixel[c] as f64-expected).abs() < 0.003, "{rgb:?}/{alpha}/{threshold}/{softness}/{invert}: {pixel:?}, expected {expected} channel {c}");
                        }
                    }
                    graph.nodes[1].inputs.clear();
                    assert!(graph.validate_working_color_domain().is_err());
                    graph.nodes[1].inputs = vec![(IrNodeId(0), OutPort::default())];
                    graph.nodes[0].op = IrOp::GradeMatteConstant { weight: 0.5 };
                    assert!(graph.validate_working_color_domain().is_err());
                }
            }
        }
    }
}

#[test]
fn native_chroma_key_matches_ap1_distance_spill_and_alpha_reference() {
    let gpu =
        GpuContext::request_blocking().expect("GPU required for native chroma key qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut id = 94000_u128;
    for key in [[0.0_f32, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 1.0, 0.0]] {
        for rgb in [
            key,
            [0.0_f32, 0.75, 0.25],
            [-0.5, 0.5, 2.0],
            [2.0, 2.0, 2.0],
            [0.0, 0.0, 0.0],
        ] {
            for alpha in [0.0_f32, 0.25, 1.0] {
                for (tolerance, softness, spill) in [
                    (0.0_f32, 0.0_f32, 0.0_f32),
                    (0.125, 0.5, 0.5),
                    (0.5, 1.0, 1.0),
                ] {
                    id += 2;
                    let mut graph = FrameGraph {
                        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                        nodes: vec![
                            IrNode {
                                op: IrOp::SolidColor {
                                    color: LinearColor {
                                        r: rgb[0] * alpha,
                                        g: rgb[1] * alpha,
                                        b: rgb[2] * alpha,
                                        a: alpha,
                                    },
                                },
                                inputs: vec![],
                                content_hash: ContentHash(id),
                            },
                            IrNode {
                                op: IrOp::NativeChromaKey {
                                    key,
                                    tolerance,
                                    softness,
                                    spill,
                                },
                                inputs: vec![(IrNodeId(0), OutPort::default())],
                                content_hash: ContentHash(id + 1),
                            },
                        ],
                        output: Some(IrNodeId(1)),
                    };
                    assert!(graph.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&graph, (17, 9), &mut EmptyProvider);
                    let frame = evaluator
                        .evaluate(&graph, (17, 9), &mut NullFrameSource)
                        .unwrap();
                    let gpu_pixels = read_texture_rgba16f(&gpu, &frame, 17, 9);
                    let distance = (0..3)
                        .map(|c| (rgb[c] as f64 - key[c] as f64).powi(2))
                        .sum::<f64>()
                        .sqrt();
                    let t = ((distance - tolerance as f64) / (softness as f64).max(0.0001))
                        .clamp(0.0, 1.0);
                    let coverage = alpha as f64 * t * t * (3.0 - 2.0 * t);
                    let dom = if key[0] >= key[1] && key[0] >= key[2] {
                        0
                    } else if key[1] >= key[2] {
                        1
                    } else {
                        2
                    };
                    let mut adjusted = rgb.map(|v| v as f64);
                    let mean = (adjusted[(dom + 1) % 3] + adjusted[(dom + 2) % 3]) / 2.0;
                    if adjusted[dom] > mean {
                        adjusted[dom] += (mean - adjusted[dom]) * spill as f64;
                    }
                    for p in cpu.pixels.iter().chain(&gpu_pixels) {
                        for c in 0..4 {
                            let expected = if c == 3 {
                                coverage
                            } else {
                                adjusted[c] * coverage
                            };
                            assert!((p[c] as f64-expected).abs()<0.003,"key{key:?},rgb{rgb:?},a{alpha},controls{tolerance}/{softness}/{spill}: {p:?} expected channel{c}={expected}");
                        }
                    }
                    graph.nodes[1].inputs.clear();
                    assert!(graph.validate_working_color_domain().is_err());
                    graph.nodes[1].inputs = vec![(IrNodeId(0), OutPort::default())];
                    graph.nodes[0].op = IrOp::GradeMatteConstant { weight: 0.5 };
                    assert!(graph.validate_working_color_domain().is_err());
                }
            }
        }
    }
}

#[test]
fn native_glow_matches_independent_ap1_extraction_gaussian_and_screen_reference() {
    let gpu = GpuContext::request_blocking().expect("GPU required for native glow qualification");
    let mut serial = 96000u128;
    let mut evaluator = Evaluator::new(gpu.clone());
    for sigma in [0.0_f32, 0.4, 1.0, 12.0, 48.0, 500.0] {
        for amount in [0.0_f32, 0.5, 2.0] {
            for threshold in [0.25_f32, 1.0] {
                let mut add = |op, inputs: Vec<u32>| {
                    serial += 1;
                    IrNode {
                        op,
                        inputs: inputs
                            .into_iter()
                            .map(|i| (IrNodeId(i), OutPort::default()))
                            .collect(),
                        content_hash: ContentHash(serial),
                    }
                };
                let mut graph = FrameGraph {
                    working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                    nodes: vec![
                        add(
                            IrOp::SolidColor {
                                color: LinearColor {
                                    r: -0.25,
                                    g: 1.5,
                                    b: 0.125,
                                    a: 0.5,
                                },
                            },
                            vec![],
                        ),
                        add(
                            IrOp::Crop {
                                left: 0.0,
                                top: 0.0,
                                right: 0.6,
                                bottom: 0.0,
                            },
                            vec![0],
                        ),
                        add(
                            IrOp::SolidColor {
                                color: LinearColor {
                                    r: 0.75,
                                    g: 0.0625,
                                    b: -0.125,
                                    a: 0.25,
                                },
                            },
                            vec![],
                        ),
                        add(
                            IrOp::Crop {
                                left: 0.6,
                                top: 0.0,
                                right: 0.0,
                                bottom: 0.0,
                            },
                            vec![2],
                        ),
                        add(
                            IrOp::Merge {
                                mode: BlendMode::Normal,
                                opacity: 1.0,
                            },
                            vec![3, 1],
                        ),
                        add(
                            IrOp::NativeGlow {
                                sigma,
                                threshold,
                                intensity: amount,
                                tint: [1.0, 0.5, 0.25],
                            },
                            vec![4],
                        ),
                    ],
                    output: Some(IrNodeId(5)),
                };
                assert!(graph.validate_working_color_domain().is_ok());
                let original: Vec<[f64; 4]> = (0..17)
                    .map(|x| {
                        if x < 7 {
                            [-0.25, 1.5, 0.125, 0.5]
                        } else if x >= 10 {
                            [0.75, 0.0625, -0.125, 0.25]
                        } else {
                            [0.0; 4]
                        }
                    })
                    .collect();
                // Independent f64 separable Gaussian plan. Rows are constant, so
                // vertical convolution is identity and only one row is needed.
                let mut low: Vec<[f64; 4]> = original
                    .iter()
                    .map(|p| {
                        let y = if p[3] > 0.0 {
                            (p[0] * 0.2722287168 + p[1] * 0.6740817658 + p[2] * 0.0536895174) / p[3]
                        } else {
                            0.0
                        };
                        if y >= threshold as f64 {
                            *p
                        } else {
                            [0.0; 4]
                        }
                    })
                    .collect();
                if sigma >= 0.5 {
                    let n = if sigma <= 12.0 {
                        1
                    } else {
                        ((sigma as f64 / 12.0).powi(2).ceil() as usize).clamp(2, 16)
                    };
                    let sp = sigma as f64 / (n as f64).sqrt();
                    let step = ((sp * 3.0).ceil() / 128.0).ceil().clamp(1.0, 4.0);
                    let radius = (((sp * 3.0).ceil() / step).ceil() as i32).min(128);
                    let weights: Vec<_> = (-radius..=radius)
                        .map(|k| (-((k as f64 * step).powi(2)) / (2.0 * sp * sp)).exp())
                        .collect();
                    let total: f64 = weights.iter().sum();
                    for _ in 0..n {
                        low = (0..17)
                            .map(|x| {
                                let mut p = [0.0; 4];
                                for (i, w) in weights.iter().enumerate() {
                                    let offset = ((i as i32 - radius) as f64 * step).round() as i32;
                                    let sample = low[(x + offset).clamp(0, 16) as usize];
                                    for c in 0..4 {
                                        p[c] += sample[c] * w / total;
                                    }
                                }
                                p
                            })
                            .collect();
                    }
                }
                let expected: Vec<_> = original
                    .iter()
                    .zip(&low)
                    .map(|(src, low)| {
                        if amount == 0.0 {
                            return *src;
                        }
                        let mut out = *src;
                        for c in 0..3 {
                            out[c] = src[c]
                                + low[c]
                                    * [1.0, 0.5, 0.25][c]
                                    * amount as f64
                                    * (1.0 - src[c]).max(0.0);
                        }
                        out[3] = src[3] + (low[3] * amount as f64).clamp(0.0, 1.0) * (1.0 - src[3]);
                        out
                    })
                    .collect();
                let cpu = eval_cpu::evaluate(&graph, (17, 9), &mut EmptyProvider);
                let texture = evaluator
                    .evaluate(&graph, (17, 9), &mut NullFrameSource)
                    .unwrap();
                let pixels = read_texture_rgba16f(&gpu, &texture, 17, 9);
                for (index, (cpu, gpu)) in cpu.pixels.iter().zip(&pixels).enumerate() {
                    let expected = expected[index % 17];
                    for c in 0..4 {
                        assert!(
                            (cpu[c] as f64 - expected[c]).abs() < 0.0003,
                            "CPU sigma{sigma} amount{amount}: {cpu:?} vs {expected:?}"
                        );
                        assert!(
                            (gpu[c] as f64 - expected[c]).abs()
                                < 0.035 * expected[c].abs().max(1.0),
                            "GPU sigma{sigma} amount{amount}: {gpu:?} vs {expected:?}"
                        );
                    }
                }
                graph.nodes[5].inputs.clear();
                assert!(graph.validate_working_color_domain().is_err());
            }
        }
    }
}

#[test]
fn native_glow_draft_radius_and_cache_follow_authored_canvas() {
    let gpu =
        GpuContext::request_blocking().expect("GPU required for native glow Draft qualification");
    let make = |sigma, authored| {
        let mut nodes = vec![
            IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: -0.25,
                        g: 1.5,
                        b: 0.125,
                        a: 0.5,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(97001),
            },
            IrNode {
                op: IrOp::Crop {
                    left: 0.25,
                    top: 0.0,
                    right: 0.0,
                    bottom: 0.0,
                },
                inputs: vec![(IrNodeId(0), OutPort::default())],
                content_hash: ContentHash(97002),
            },
            IrNode {
                op: IrOp::NativeGlow {
                    sigma,
                    threshold: 0.5,
                    intensity: 0.5,
                    tint: [1.0, 0.5, 0.25],
                },
                inputs: vec![(IrNodeId(1), OutPort::default())],
                content_hash: ContentHash(97003),
            },
        ];
        if authored {
            nodes.push(IrNode {
                op: IrOp::NativeCanvas { w: 34, h: 18 },
                inputs: vec![(IrNodeId(2), OutPort::default())],
                content_hash: ContentHash(97004),
            });
        }
        FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            output: Some(IrNodeId(nodes.len() as u32 - 1)),
            nodes,
        }
    };
    let graph = make(4.0, true);
    let mut evaluator = Evaluator::new(gpu.clone());
    for (w, h) in [(17, 9), (34, 18), (17, 9)] {
        let reference = make(4.0 * w as f32 / 34.0, false);
        let expected = eval_cpu::evaluate(&reference, (w, h), &mut EmptyProvider);
        let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
        let texture = evaluator
            .evaluate(&graph, (w, h), &mut NullFrameSource)
            .unwrap();
        let pixels = read_texture_rgba16f(&gpu, &texture, w, h);
        for ((actual, gpu), expected) in cpu.pixels.iter().zip(&pixels).zip(&expected.pixels) {
            for c in 0..4 {
                assert!((actual[c] - expected[c]).abs() < 1e-5);
                assert!((gpu[c] - expected[c]).abs() < 0.005);
            }
        }
    }
}

#[test]
fn native_shape_generators_match_independent_normalized_geometry_and_cache() {
    let gpu = GpuContext::request_blocking().expect("GPU required for native shape qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut id = 98000u128;
    for rect in [false, true] {
        for center in [[0.5_f32, 0.5], [0.25, 0.75]] {
            for size in [[0.0_f32, 0.25], [0.25, 0.375], [1.0, 0.5]] {
                for rotation in [0.0_f32, 0.6, 1.5] {
                    for feather in [0.0_f32, 0.25, 1.0] {
                        id += 1;
                        let mut graph = FrameGraph {
                            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                            nodes: vec![IrNode {
                                op: IrOp::NativeMaskShape {
                                    rect,
                                    center,
                                    size,
                                    rotation,
                                    feather,
                                },
                                inputs: vec![],
                                content_hash: ContentHash(id),
                            }],
                            output: Some(IrNodeId(0)),
                        };
                        assert!(graph.validate_working_color_domain().is_ok());
                        for (w, h) in [(17, 9), (34, 18), (17, 9)] {
                            let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
                            let frame = evaluator
                                .evaluate(&graph, (w, h), &mut NullFrameSource)
                                .unwrap();
                            let pixels = read_texture_rgba16f(&gpu, &frame, w, h);
                            for (i, (c, g)) in cpu.pixels.iter().zip(&pixels).enumerate() {
                                let expected = if size[0] <= 1e-6 || size[1] <= 1e-6 {
                                    0.0
                                } else {
                                    let dx = (i % w as usize) as f64 / w as f64 + 0.5 / w as f64
                                        - center[0] as f64;
                                    let dy = (i / w as usize) as f64 / h as f64 + 0.5 / h as f64
                                        - center[1] as f64;
                                    let (sin, cos) = (-(rotation as f64)).sin_cos();
                                    let x = (dx * cos - dy * sin) / size[0] as f64;
                                    let y = (dx * sin + dy * cos) / size[1] as f64;
                                    let r = if rect {
                                        x.abs().max(y.abs())
                                    } else {
                                        x.hypot(y)
                                    };
                                    let inner = 1.0 - (feather as f64).max(0.0001);
                                    let t = ((r - inner) / (1.0 - inner)).clamp(0.0, 1.0);
                                    1.0 - t * t * (3.0 - 2.0 * t)
                                };
                                for channel in 0..4 {
                                    assert!((c[channel] as f64-expected).abs()<0.001,"CPU {rect}/{center:?}/{size:?}/{rotation}/{feather}/{w}x{h}: {c:?} vs {expected}");
                                    assert!((g[channel] as f64-expected).abs()<0.002,"GPU {rect}/{center:?}/{size:?}/{rotation}/{feather}/{w}x{h}: {g:?} vs {expected}");
                                }
                            }
                        }
                        graph.nodes[0].inputs = vec![(IrNodeId(0), OutPort::default())];
                        assert!(graph.validate_working_color_domain().is_err());
                    }
                }
            }
        }
    }
}

#[test]
fn native_spatial_qualifier_log_mix_preserves_coverage_and_cache_scale() {
    check_native_spatial_qualifier_precision(false);
}

#[test]
fn native_spatial_qualifier_fused_apply_preserves_precision() {
    check_native_spatial_qualifier_precision(true);
}

fn check_native_spatial_qualifier_precision(fused: bool) {
    use photonic_core::timeline::{GradeMatteRefinement, WindowShape};
    use photonic_render::grade::{ResolvedCdl, ResolvedHslQualifier, ResolvedMask};
    // Independent binary16 store references. Render-target conversion may
    // round to nearest or truncate; the software GPU used here truncates.
    let half_store = |v: f64, nearest: bool| {
        if v == 0.0 {
            return v;
        }
        let step = 2.0f64.powi(if v.abs() < 2.0f64.powi(-14) {
            -24
        } else {
            v.abs().log2().floor() as i32 - 10
        });
        let units = v / step;
        if nearest {
            units.round_ties_even() * step
        } else {
            units.trunc() * step
        }
    };
    let encode = |x: f64| {
        if x <= 0.0078125 {
            10.5402377416545 * x + 0.0729055341958355
        } else {
            (x.log2() + 9.72) / 17.52
        }
    };
    let decode = |x: f64| {
        if x <= 0.155251141552511 {
            (x - 0.0729055341958355) / 10.5402377416545
        } else {
            2.0f64.powf(x * 17.52 - 9.72)
        }
    };
    let gpu_reference = |v: f32, alpha: f32, weight: f32, nearest: bool| {
        if alpha == 0.0 {
            return 0.0;
        }
        let a = f64::from(alpha);
        let source = half_store(f64::from(v * alpha), nearest);
        if fused {
            return half_store(
                decode(encode(source / a) + 0.04 * f64::from(weight)) * a,
                nearest,
            ) as f32;
        }
        let encoded = half_store(encode(source / a) * a, nearest);
        let corrected = half_store((encoded / a + 0.04) * a, nearest);
        let mixed = half_store(encoded + (corrected - encoded) * f64::from(weight), nearest);
        half_store(decode(mixed / a) * a, nearest) as f32
    };
    let gpu = GpuContext::request_blocking();
    for source_rgb in [[0.18f32; 3], [-0.03, 0.18, 3.0]] {
        for alpha in [0.0f32, 0.25, 0.5, 1.0] {
            let refinement = GradeMatteRefinement {
                denoise: true,
                grow: 0.02,
                blur: 0.02,
                matte_levels: [0.0; 2],
            };
            let mut graph = FrameGraph {
                working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                nodes: vec![],
                output: None,
            };
            let mut push = |op, inputs: Vec<usize>| {
                let id = graph.nodes.len();
                graph.nodes.push(IrNode {
                    op,
                    inputs: inputs
                        .into_iter()
                        .map(|i| (IrNodeId(i as u32), OutPort::default()))
                        .collect(),
                    content_hash: ContentHash(700 + id as u128 + u128::from(alpha.to_bits())),
                });
                id
            };
            let source = push(
                IrOp::SolidColor {
                    color: LinearColor {
                        r: source_rgb[0] * alpha,
                        g: source_rgb[1] * alpha,
                        b: source_rgb[2] * alpha,
                        a: alpha,
                    },
                },
                vec![],
            );
            let encoded = push(
                IrOp::NativeAcescct {
                    direction: AcescctDirection::Encode,
                },
                vec![source],
            );
            let key = push(
                IrOp::QualifierMatte {
                    qualifier: Box::new(ResolvedHslQualifier::default()),
                    mask: Some(ResolvedMask {
                        shape: WindowShape::Rectangle,
                        center: [0.5; 2],
                        size: [0.233, 0.193],
                        rotation: 0.0,
                        softness: 0.0,
                        invert: false,
                    }),
                    native: true,
                },
                vec![encoded],
            );
            let refined = push(IrOp::GradeMatteRefine { refinement }, vec![key]);
            let (mixed, decoded) = if fused {
                let fused = push(
                    IrOp::NativeMatteCdl {
                        cdl: ResolvedCdl {
                            slope: [1.0; 3],
                            offset: [0.04; 3],
                            power: [1.0; 3],
                            sat: 1.0,
                        },
                    },
                    vec![source, refined],
                );
                (fused, fused)
            } else {
                let corrected = push(
                    IrOp::NativeLogCdl {
                        cdl: ResolvedCdl {
                            slope: [1.0; 3],
                            offset: [0.04; 3],
                            power: [1.0; 3],
                            sat: 1.0,
                        },
                    },
                    vec![encoded],
                );
                let mixed = push(IrOp::GradeMatteApply, vec![corrected, encoded, refined]);
                let decoded = push(
                    IrOp::NativeAcescct {
                        direction: AcescctDirection::Decode,
                    },
                    vec![mixed],
                );
                (mixed, decoded)
            };
            graph.output = Some(IrNodeId(decoded as u32));
            assert!(graph.validate_working_color_domain().is_ok());
            let mut evaluator = gpu.as_ref().map(|gpu| Evaluator::new(gpu.clone()));
            for n in [50u32, 25, 50] {
                let mut weights = Vec::new();
                for y in 0..n {
                    for x in 0..n {
                        let selected = ((x as f64 + 0.5) / n as f64 - 0.5).abs() < 0.233
                            && ((y as f64 + 0.5) / n as f64 - 0.5).abs() < 0.193;
                        weights.push([if selected && alpha > 0.0 { 1.0 } else { 0.0 }; 4]);
                    }
                }
                photonic_render::grade_graph::refine_matte_cpu(&mut weights, n, n, refinement)
                    .unwrap();
                let expected: Vec<[f32; 4]> = weights
                    .iter()
                    .map(|w| {
                        let rgb = source_rgb.map(|v| {
                            let x = f64::from(v);
                            let encoded = if x <= 0.0078125 {
                                10.5402377416545 * x + 0.0729055341958355
                            } else {
                                (x.log2() + 9.72) / 17.52
                            };
                            let corrected = encoded + 0.04 * f64::from(w[0]);
                            let decoded = if corrected <= 0.155251141552511 {
                                (corrected - 0.0729055341958355) / 10.5402377416545
                            } else {
                                2.0f64.powf(corrected * 17.52 - 9.72)
                            };
                            decoded as f32 * alpha
                        });
                        [rgb[0], rgb[1], rgb[2], alpha]
                    })
                    .collect();
                let cpu = eval_cpu::evaluate(&graph, (n, n), &mut EmptyProvider);
                for (actual, want) in cpu.pixels.iter().zip(&expected) {
                    for c in 0..4 {
                        assert!(
                            (actual[c] - want[c]).abs() < 0.00001 * want[c].abs().max(1.0),
                            "CPU {n} {alpha} {actual:?} {want:?}"
                        );
                    }
                }
                if let (Some(gpu), Some(evaluator)) = (&gpu, &mut evaluator) {
                    let output = evaluator
                        .evaluate(&graph, (n, n), &mut NullFrameSource)
                        .unwrap();
                    for (index, (actual, want)) in read_texture_rgba16f(gpu, &output, n, n)
                        .iter()
                        .zip(&expected)
                        .enumerate()
                    {
                        for c in 0..4 {
                            if c < 3 {
                                let first =
                                    gpu_reference(source_rgb[c], alpha, weights[index][0], false);
                                let second =
                                    gpu_reference(source_rgb[c], alpha, weights[index][0], true);
                                let tolerance = 0.003 * want[c].abs().max(1.0);
                                assert!(actual[c]>=first.min(second)-tolerance&&actual[c]<=first.max(second)+tolerance,"binary16 GPU {n} {alpha} {index} {c}: {} outside {first}..{second}",actual[c]);
                            }
                            // Explicit format-precision limit, separate from
                            // the much tighter f64 CPU mathematical reference.
                            assert!(
                                (actual[c] - want[c]).abs()
                                    < (if fused { 0.0015 } else { 0.016 }) * want[c].abs().max(1.0),
                                "GPU ideal bound {n} {alpha} {actual:?} {want:?}"
                            );
                        }
                        assert_eq!(actual[3], alpha);
                    }
                }
            }
            let mut invalid = graph.clone();
            invalid.nodes[mixed].inputs.pop();
            assert!(invalid.validate_working_color_domain().is_err());
            let mut invalid = graph.clone();
            invalid.nodes[mixed].op = IrOp::GradeLayerMix { opacity: 0.5 };
            invalid.nodes[mixed].inputs.pop();
            assert!(invalid.validate_working_color_domain().is_err());
        }
    }
}

#[test]
fn native_matte_cdl_matches_independent_signed_log_reference() {
    use photonic_render::grade::ResolvedCdl;
    let gpu = GpuContext::request_blocking();
    let mut evaluator = gpu.as_ref().map(|gpu| Evaluator::new(gpu.clone()));
    let encode = |x: f64| {
        if x <= 0.0078125 {
            10.5402377416545 * x + 0.0729055341958355
        } else {
            (x.log2() + 9.72) / 17.52
        }
    };
    let decode = |x: f64| {
        if x <= 0.155251141552511 {
            (x - 0.0729055341958355) / 10.5402377416545
        } else {
            2.0f64.powf(x * 17.52 - 9.72).min(65504.0)
        }
    };
    let mut case = 0u128;
    for rgb in [[0.18f32; 3], [-0.02, 0.5, 3.0], [1.2, 0.04, 0.8]] {
        for alpha in [0.0f32, 0.1, 0.25, 0.5, 1.0] {
            for weight in [0.0f32, 0.25, 0.5, 1.0] {
                for cdl in [
                    ResolvedCdl {
                        slope: [1.0; 3],
                        offset: [0.0; 3],
                        power: [1.0; 3],
                        sat: 1.0,
                    },
                    ResolvedCdl {
                        slope: [1.0; 3],
                        offset: [0.04; 3],
                        power: [1.0; 3],
                        sat: 1.0,
                    },
                    ResolvedCdl {
                        slope: [0.8, 1.2, 0.9],
                        offset: [-0.05, 0.02, 0.1],
                        power: [0.7, 1.3, 1.1],
                        sat: 0.7,
                    },
                    ResolvedCdl {
                        slope: [1.0; 3],
                        offset: [0.0; 3],
                        power: [1.0; 3],
                        sat: 0.0,
                    },
                ] {
                    case += 1;
                    let graph = FrameGraph {
                        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                        nodes: vec![
                            IrNode {
                                op: IrOp::SolidColor {
                                    color: LinearColor {
                                        r: rgb[0] * alpha,
                                        g: rgb[1] * alpha,
                                        b: rgb[2] * alpha,
                                        a: alpha,
                                    },
                                },
                                inputs: vec![],
                                content_hash: ContentHash(case * 3),
                            },
                            IrNode {
                                op: IrOp::GradeMatteConstant { weight },
                                inputs: vec![],
                                content_hash: ContentHash(case * 3 + 1),
                            },
                            IrNode {
                                op: IrOp::NativeMatteCdl { cdl },
                                inputs: vec![
                                    (IrNodeId(0), OutPort::default()),
                                    (IrNodeId(1), OutPort::default()),
                                ],
                                content_hash: ContentHash(case * 3 + 2),
                            },
                        ],
                        output: Some(IrNodeId(2)),
                    };
                    assert!(graph.validate_working_color_domain().is_ok());
                    if case == 1 {
                        let mut bad = graph.clone();
                        bad.working_color_domain = WorkingColorDomain::LegacyLinearRec709;
                        assert!(bad.validate_working_color_domain().is_err());
                        let mut bad = graph.clone();
                        bad.nodes[2].inputs[1].0 = IrNodeId(0);
                        assert!(bad.validate_working_color_domain().is_err());
                        let mut bad = graph.clone();
                        bad.nodes[2].inputs.pop();
                        assert!(bad.validate_working_color_domain().is_err());
                        let mut bad = graph.clone();
                        bad.nodes[2].op = IrOp::NativeMatteCdl {
                            cdl: ResolvedCdl {
                                power: [0.0; 3],
                                ..cdl
                            },
                        };
                        assert!(bad.validate_working_color_domain().is_err());
                    }

                    let mut expected = [0.0; 4];
                    expected[3] = alpha;
                    if alpha > 0.0 {
                        let log = rgb.map(|v| encode(f64::from(v)));
                        let corrected = std::array::from_fn::<_, 3, _>(|c| {
                            let v = log[c] * f64::from(cdl.slope[c]) + f64::from(cdl.offset[c]);
                            if v <= 0.0 {
                                v.max(-65504.0)
                            } else {
                                v.powf(f64::from(cdl.power[c])).min(65504.0)
                            }
                        });
                        let luma =
                            corrected[0] * 0.2126 + corrected[1] * 0.7152 + corrected[2] * 0.0722;
                        for c in 0..3 {
                            let saturated = luma + f64::from(cdl.sat) * (corrected[c] - luma);
                            expected[c] =
                                (decode(log[c] + (saturated - log[c]) * f64::from(weight))
                                    * f64::from(alpha)) as f32;
                        }
                    }
                    let cpu = eval_cpu::evaluate(&graph, (1, 1), &mut EmptyProvider).pixels[0];
                    for c in 0..4 {
                        assert!(
                            (cpu[c] - expected[c]).abs() < 0.00002 * expected[c].abs().max(1.0),
                            "CPU case {case} {cpu:?} {expected:?}"
                        );
                    }
                    if let (Some(gpu), Some(evaluator)) = (&gpu, &mut evaluator) {
                        let (output, source) = evaluator.evaluate_with_tap(
                            &graph,
                            (1, 1),
                            &mut NullFrameSource,
                            Some(IrNodeId(0)),
                        );
                        let actual = read_texture_rgba16f(gpu, &output.unwrap(), 1, 1)[0];
                        if weight == 0.0
                            || (cdl.slope == [1.0; 3]
                                && cdl.offset == [0.0; 3]
                                && cdl.power == [1.0; 3]
                                && cdl.sat == 1.0)
                        {
                            assert_eq!(
                                actual,
                                read_texture_rgba16f(gpu, &source.unwrap().texture, 1, 1)[0],
                                "unmodified pixels must avoid a logarithmic roundtrip"
                            );
                        }
                        for c in 0..4 {
                            assert!(
                                (actual[c] - expected[c]).abs()
                                    < 0.003 * expected[c].abs().max(1.0),
                                "GPU case {case} {actual:?} {expected:?}"
                            );
                        }
                    }
                }
            }
        }
    }
    assert_eq!(case, 240);
}

#[test]
fn native_matte_cdl_extreme_controls_keep_finite_storage_and_coverage() {
    use photonic_render::grade::ResolvedCdl;
    let gpu = GpuContext::request_blocking();
    let mut evaluator = gpu.as_ref().map(|gpu| Evaluator::new(gpu.clone()));
    let mut case = 0u128;
    for rgb in [[-0.5, 0.18, 20.0], [100.0, -20.0, 0.001]] {
        for alpha in [0.0f32, 0.25, 1.0] {
            for weight in [0.0f32, 0.5, 1.0] {
                for cdl in [
                    ResolvedCdl {
                        slope: [8.0; 3],
                        offset: [4.0; 3],
                        power: [10.0; 3],
                        sat: 4.0,
                    },
                    ResolvedCdl {
                        slope: [8.0, 0.0, 8.0],
                        offset: [4.0, -4.0, 2.0],
                        power: [10.0, 0.1, 10.0],
                        sat: 4.0,
                    },
                ] {
                    case += 1;
                    let graph = FrameGraph {
                        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                        nodes: vec![
                            IrNode {
                                op: IrOp::SolidColor {
                                    color: LinearColor {
                                        r: rgb[0] * alpha,
                                        g: rgb[1] * alpha,
                                        b: rgb[2] * alpha,
                                        a: alpha,
                                    },
                                },
                                inputs: vec![],
                                content_hash: ContentHash(case * 3),
                            },
                            IrNode {
                                op: IrOp::GradeMatteConstant { weight },
                                inputs: vec![],
                                content_hash: ContentHash(case * 3 + 1),
                            },
                            IrNode {
                                op: IrOp::NativeMatteCdl { cdl },
                                inputs: vec![
                                    (IrNodeId(0), OutPort::default()),
                                    (IrNodeId(1), OutPort::default()),
                                ],
                                content_hash: ContentHash(case * 3 + 2),
                            },
                        ],
                        output: Some(IrNodeId(2)),
                    };
                    assert!(graph.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&graph, (1, 1), &mut EmptyProvider).pixels[0];
                    assert_eq!(cpu[3], alpha);
                    assert!(cpu.iter().all(|v| v.is_finite() && v.abs() <= 65504.0));
                    if let (Some(gpu), Some(evaluator)) = (&gpu, &mut evaluator) {
                        let output = evaluator
                            .evaluate(&graph, (1, 1), &mut NullFrameSource)
                            .unwrap();
                        let pixel = read_texture_rgba16f(gpu, &output, 1, 1)[0];
                        assert_eq!(pixel[3], alpha);
                        assert!(
                            pixel.iter().all(|v| v.is_finite() && v.abs() <= 65504.0),
                            "case {case}: {pixel:?}"
                        );
                        if alpha == 0.0 {
                            assert_eq!(pixel, [0.0; 4]);
                        }
                    }
                }
            }
        }
    }
    assert_eq!(case, 36);
}

#[test]
fn kelvin_matrices_preserve_signed_color_and_alpha_in_both_working_domains() {
    use photonic_render::grade::{ResolvedGradeOp, ResolvedGradePayload};
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/cct_reference.json")).unwrap();
    let gpu = GpuContext::request_blocking();
    let mut evaluator = gpu.as_ref().map(|g| Evaluator::new(g.clone()));
    let mut case = 0u128;
    for native in [false, true] {
        let fixtures = reference[if native {
            "adaptations"
        } else {
            "legacy_adaptations"
        }]
        .as_array()
        .unwrap();
        for item in fixtures {
            let source = item["source_kelvin"].as_f64().unwrap() as f32;
            let target = item["target_kelvin"].as_f64().unwrap() as f32;
            let rows = photonic_render::white_balance::cct_matrix(source, target, native).unwrap();
            let expected_rows: [[f64; 3]; 3] =
                serde_json::from_value(item["rows"].clone()).unwrap();
            for (a, b) in rows.iter().flatten().zip(expected_rows.iter().flatten()) {
                assert!((f64::from(*a) - b).abs() < 2e-6);
            }
            for rgb in [[-0.03f32, 0.18, 3.0], [0.5, 0.2, 0.8], [1.0; 3]] {
                for alpha in [0.0f32, 0.25, 0.5, 1.0] {
                    case += 1;
                    let op = if native {
                        IrOp::NativeRgbMatrix { rows }
                    } else {
                        IrOp::Grade {
                            ops: vec![ResolvedGradeOp {
                                payload: ResolvedGradePayload::LinearMatrix { rows },
                                mask: None,
                            }],
                        }
                    };
                    let graph = FrameGraph {
                        working_color_domain: if native {
                            WorkingColorDomain::SceneLinearAcescg
                        } else {
                            WorkingColorDomain::LegacyLinearRec709
                        },
                        nodes: vec![
                            IrNode {
                                op: IrOp::SolidColor {
                                    color: LinearColor {
                                        r: rgb[0] * alpha,
                                        g: rgb[1] * alpha,
                                        b: rgb[2] * alpha,
                                        a: alpha,
                                    },
                                },
                                inputs: vec![],
                                content_hash: ContentHash(case * 2),
                            },
                            IrNode {
                                op,
                                inputs: vec![(IrNodeId(0), OutPort::default())],
                                content_hash: ContentHash(case * 2 + 1),
                            },
                        ],
                        output: Some(IrNodeId(1)),
                    };
                    assert!(graph.validate_working_color_domain().is_ok());
                    let expected = expected_rows.map(|r| {
                        (r[0] * f64::from(rgb[0])
                            + r[1] * f64::from(rgb[1])
                            + r[2] * f64::from(rgb[2])) as f32
                            * alpha
                    });
                    let cpu = eval_cpu::evaluate(&graph, (1, 1), &mut EmptyProvider).pixels[0];
                    for c in 0..3 {
                        assert!(
                            (cpu[c] - expected[c]).abs() < 0.00001 * expected[c].abs().max(1.0)
                        );
                    }
                    assert_eq!(cpu[3], alpha);
                    if let (Some(gpu), Some(evaluator)) = (&gpu, &mut evaluator) {
                        let texture = evaluator
                            .evaluate(&graph, (1, 1), &mut NullFrameSource)
                            .unwrap();
                        let pixel = read_texture_rgba16f(gpu, &texture, 1, 1)[0];
                        for c in 0..3 {
                            assert!(
                                (pixel[c] - expected[c]).abs() < 0.003 * expected[c].abs().max(1.0),
                                "case {case} {pixel:?} {expected:?}"
                            );
                        }
                        assert_eq!(pixel[3], alpha);
                    }
                }
            }
        }
    }
    assert_eq!(case, 120);
}

#[test]
fn tonal_exposure_matches_independent_dual_domain_luminance_reference() {
    use photonic_render::{
        grade::{ResolvedGradeOp, ResolvedGradePayload},
        tonal_exposure::ResolvedTonalExposure,
    };
    let gpu = GpuContext::request_blocking();
    let mut evaluator = gpu.as_ref().map(|g| Evaluator::new(g.clone()));
    let mut case = 0u128;
    for native in [false, true] {
        for (stops, boundaries, softness) in [
            ([0.0; 3], [-2.0, 2.0], 1.0),
            ([1.0; 3], [-2.0, 2.0], 1.0),
            ([1.0, -0.5, -1.0], [-2.0, 2.0], 1.0),
            ([-2.0, 0.75, 2.0], [-1.0, 1.0], 3.0),
        ] {
            let tonal = ResolvedTonalExposure {
                stops,
                boundaries,
                softness,
                native_ap1: native,
            };
            for rgb in [
                [-0.03f32, 0.18, 3.0],
                [-0.2; 3],
                [0.0; 3],
                [0.01125; 3],
                [0.045; 3],
                [0.18; 3],
                [0.72; 3],
                [2.88; 3],
            ] {
                for alpha in [0.0f32, 0.25, 0.5, 1.0] {
                    case += 1;
                    let coefficients = if native {
                        [0.2722287167809145, 0.6740817658111483, 0.05368951740793705]
                    } else {
                        [0.2126, 0.7152, 0.0722]
                    };
                    let y: f64 = rgb
                        .iter()
                        .zip(coefficients)
                        .map(|(v, c)| f64::from(*v) * c)
                        .sum();
                    let gate = |boundary: f32| {
                        let t = ((y / 0.18).log2() - f64::from(boundary) + f64::from(softness))
                            / (2.0 * f64::from(softness));
                        let t = t.clamp(0.0, 1.0);
                        t * t * (3.0 - 2.0 * t)
                    };
                    let weights = if y <= 0.0 {
                        [1.0, 0.0, 0.0]
                    } else {
                        let a = gate(boundaries[0]);
                        let b = gate(boundaries[1]);
                        [1.0 - a, a - b, b]
                    };
                    let gain = weights
                        .iter()
                        .zip(stops)
                        .map(|(w, s)| w * f64::from(s))
                        .sum::<f64>()
                        .exp2();
                    let expected = rgb.map(|v| (f64::from(v) * gain * f64::from(alpha)) as f32);
                    let op = if native {
                        IrOp::NativeTonalExposure { tonal }
                    } else {
                        IrOp::Grade {
                            ops: vec![ResolvedGradeOp {
                                payload: ResolvedGradePayload::TonalExposure(tonal),
                                mask: None,
                            }],
                        }
                    };
                    let graph = FrameGraph {
                        working_color_domain: if native {
                            WorkingColorDomain::SceneLinearAcescg
                        } else {
                            WorkingColorDomain::LegacyLinearRec709
                        },
                        nodes: vec![
                            IrNode {
                                op: IrOp::SolidColor {
                                    color: LinearColor {
                                        r: rgb[0] * alpha,
                                        g: rgb[1] * alpha,
                                        b: rgb[2] * alpha,
                                        a: alpha,
                                    },
                                },
                                inputs: vec![],
                                content_hash: ContentHash(case * 2),
                            },
                            IrNode {
                                op,
                                inputs: vec![(IrNodeId(0), OutPort::default())],
                                content_hash: ContentHash(case * 2 + 1),
                            },
                        ],
                        output: Some(IrNodeId(1)),
                    };
                    assert!(graph.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&graph, (1, 1), &mut EmptyProvider).pixels[0];
                    for c in 0..3 {
                        assert!(
                            (cpu[c] - expected[c]).abs() < 0.00001 * expected[c].abs().max(1.0),
                            "case {case} {cpu:?} {expected:?}"
                        );
                    }
                    assert_eq!(cpu[3], alpha);
                    if let (Some(gpu), Some(evaluator)) = (&gpu, &mut evaluator) {
                        let texture = evaluator
                            .evaluate(&graph, (1, 1), &mut NullFrameSource)
                            .unwrap();
                        let pixel = read_texture_rgba16f(gpu, &texture, 1, 1)[0];
                        for c in 0..3 {
                            assert!(
                                (pixel[c] - expected[c]).abs() < 0.004 * expected[c].abs().max(1.0),
                                "case {case} {pixel:?} {expected:?}"
                            );
                        }
                        assert_eq!(pixel[3], alpha);
                    }
                }
            }
        }
    }
    assert_eq!(case, 256);
}

#[test]
fn animated_windows_match_independent_geometry_across_time_alpha_and_canvas() {
    use photonic_core::timeline::{
        GradeMask, GradeOp, GradeOpKind, GradeOpParams, Interp, Keyframe, PropPath, PropValue,
        Tick, WindowShape,
    };
    use photonic_render::grade::ResolvedMask;
    let gpu =
        GpuContext::request_blocking().expect("GPU required for animated window qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut serial = 180000u128;
    for shape in [
        WindowShape::Ellipse,
        WindowShape::Rectangle,
        WindowShape::Gradient,
    ] {
        for invert in [false, true] {
            let mut op = GradeOp::new(
                GradeOpKind::Exposure,
                GradeOpParams::Exposure { stops: 1.0 },
            );
            op.mask = Some(GradeMask::PowerWindow {
                shape,
                center: [0.25, 0.4],
                size: [0.2, 0.3],
                rotation: 0.0,
                softness: 0.2,
                invert,
            });
            for (path, a, b) in [
                ("mask.center[0]", 0.25, 0.75),
                ("mask.center[1]", 0.4, 0.6),
                ("mask.size[0]", 0.2, 0.3),
                ("mask.size[1]", 0.3, 0.2),
                ("mask.rotation", 0.0, 0.6),
                ("mask.softness", 0.2, 0.6),
            ] {
                if op.scalar_property(path).is_none() {
                    continue;
                }
                for (at, v) in [(0, a), (100, b)] {
                    op.params
                        .track_mut(&PropPath::new(path))
                        .insert_keyframe(Keyframe::new(
                            Tick(at),
                            PropValue::Float(v),
                            Interp::Linear,
                        ));
                }
            }
            for tick in [0, 50, 100, 0] {
                let GradeMask::PowerWindow {
                    shape,
                    center,
                    size,
                    rotation,
                    softness,
                    invert,
                } = op.mask_at(Tick(tick)).unwrap().unwrap()
                else {
                    unreachable!()
                };
                let mask = ResolvedMask {
                    shape,
                    center,
                    size,
                    rotation,
                    softness,
                    invert,
                };
                let u = tick as f64 / 100.0;
                for (width, height) in [(5u32, 3u32), (17, 9)] {
                    for alpha in [0.0f32, 0.5] {
                        serial += 3;
                        let mut g = graph(1.0, WorkingColorDomain::SceneLinearAcescg);
                        g.nodes[0].op = IrOp::SolidColor {
                            color: LinearColor {
                                r: -0.25 * alpha,
                                g: 2.0 * alpha,
                                b: 0.5 * alpha,
                                a: alpha,
                            },
                        };
                        g.nodes[0].content_hash = ContentHash(serial);
                        g.nodes[1].content_hash = ContentHash(serial + 1);
                        g.nodes.push(IrNode {
                            op: IrOp::NativeMaskMix { mask },
                            inputs: vec![
                                (IrNodeId(1), OutPort::default()),
                                (IrNodeId(0), OutPort::default()),
                            ],
                            content_hash: ContentHash(serial + 2),
                        });
                        g.output = Some(IrNodeId(2));
                        assert!(g.validate_working_color_domain().is_ok());
                        let cpu = eval_cpu::evaluate(&g, (width, height), &mut EmptyProvider);
                        let texture = evaluator
                            .evaluate(&g, (width, height), &mut NullFrameSource)
                            .unwrap();
                        let pixels = read_texture_rgba16f(&gpu, &texture, width, height);
                        for index in 0..(width * height) as usize {
                            let dx = (index % width as usize) as f64 / width as f64
                                + 0.5 / width as f64
                                - (0.25 + 0.5 * u);
                            let dy = (index / width as usize) as f64 / height as f64
                                + 0.5 / height as f64
                                - (0.4 + 0.2 * u);
                            let (sin, cos) = (0.6 * u).sin_cos();
                            let x = dx * cos + dy * sin;
                            let y = -dx * sin + dy * cos;
                            let sx = 0.2 + 0.1 * u;
                            let sy = 0.3 - 0.1 * u;
                            let feather = 0.2 + 0.4 * u;
                            let smooth = |lo: f64, hi: f64, v: f64| {
                                let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
                                t * t * (3.0 - 2.0 * t)
                            };
                            let weight = match shape {
                                WindowShape::Gradient => 1.0 - smooth(-sy, sy, y),
                                WindowShape::Rectangle => {
                                    1.0 - smooth(
                                        1.0 - feather,
                                        1.0 + feather,
                                        (x / sx).abs().max((y / sy).abs()),
                                    )
                                }
                                WindowShape::Ellipse => {
                                    1.0 - smooth(
                                        1.0 - feather,
                                        1.0 + feather,
                                        (x / sx).hypot(y / sy),
                                    )
                                }
                            };
                            let weight = if invert { 1.0 - weight } else { weight };
                            let expected = [
                                -0.25 * alpha as f64 * (1.0 + weight),
                                2.0 * alpha as f64 * (1.0 + weight),
                                0.5 * alpha as f64 * (1.0 + weight),
                                alpha as f64,
                            ];
                            for c in 0..4 {
                                assert!(
                                    (cpu.pixels[index][c] as f64 - expected[c]).abs() < 1e-5,
                                    "CPU {shape:?} t{tick} {index}: {:?} vs {expected:?}",
                                    cpu.pixels[index]
                                );
                                assert!(
                                    (pixels[index][c] as f64 - expected[c]).abs() < 0.005,
                                    "GPU {shape:?} t{tick} {index}: {:?} vs {expected:?}",
                                    pixels[index]
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn boolean_window_mattes_match_independent_geometry_without_rgb_or_alpha_gating() {
    use photonic_core::timeline::{GradeKeyMixMode, WindowShape};
    use photonic_render::grade::ResolvedMask;
    fn reference(m: &ResolvedMask, x: f64, y: f64) -> f64 {
        let (s, c) = (m.rotation as f64).sin_cos();
        let dx = x - m.center[0] as f64;
        let dy = y - m.center[1] as f64;
        let u = dx * c + dy * s;
        let v = -dx * s + dy * c;
        let smooth = |a: f64, b: f64, x: f64| {
            let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        let w = match m.shape {
            WindowShape::Gradient => 1.0 - smooth(-(m.size[1] as f64), m.size[1] as f64, v),
            WindowShape::Ellipse | WindowShape::Rectangle => {
                let u = u / m.size[0] as f64;
                let v = v / m.size[1] as f64;
                let d = if m.shape == WindowShape::Ellipse {
                    u.hypot(v)
                } else {
                    u.abs().max(v.abs())
                };
                1.0 - smooth(1.0 - m.softness as f64, 1.0 + m.softness as f64, d)
            }
        };
        if m.invert {
            1.0 - w
        } else {
            w
        }
    }
    let gpu =
        GpuContext::request_blocking().expect("GPU required for Boolean window qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut cases = 0u128;
    for domain in [
        WorkingColorDomain::LegacyLinearRec709,
        WorkingColorDomain::SceneLinearAcescg,
    ] {
        for a in [
            WindowShape::Ellipse,
            WindowShape::Rectangle,
            WindowShape::Gradient,
        ] {
            for b in [
                WindowShape::Ellipse,
                WindowShape::Rectangle,
                WindowShape::Gradient,
            ] {
                for mode in [
                    GradeKeyMixMode::Union,
                    GradeKeyMixMode::Intersect,
                    GradeKeyMixMode::Subtract,
                    GradeKeyMixMode::Multiply,
                ] {
                    for alpha in [0.0, 0.5, 1.0] {
                        for (width, height) in [(5u32, 3u32), (17, 9)] {
                            cases += 1;
                            let first = ResolvedMask {
                                shape: a,
                                center: [0.35, 0.42],
                                size: [0.28, 0.35],
                                rotation: 0.37,
                                softness: 0.25,
                                invert: cases % 2 == 0,
                            };
                            let second = ResolvedMask {
                                shape: b,
                                center: [0.65, 0.55],
                                size: [0.33, 0.2],
                                rotation: -0.21,
                                softness: 0.4,
                                invert: cases % 3 == 0,
                            };
                            let base = 200000 + cases * 10;
                            let nodes = vec![
                                IrNode {
                                    op: IrOp::SolidColor {
                                        color: LinearColor {
                                            r: -2.0 * alpha,
                                            g: 8.0 * alpha,
                                            b: 0.3 * alpha,
                                            a: alpha,
                                        },
                                    },
                                    inputs: vec![],
                                    content_hash: ContentHash(base),
                                },
                                IrNode {
                                    op: IrOp::WindowMatte { mask: first },
                                    inputs: vec![(IrNodeId(0), OutPort::default())],
                                    content_hash: ContentHash(base + 1),
                                },
                                IrNode {
                                    op: IrOp::WindowMatte { mask: second },
                                    inputs: vec![(IrNodeId(0), OutPort::default())],
                                    content_hash: ContentHash(base + 2),
                                },
                                IrNode {
                                    op: IrOp::GradeKeyMix { mode },
                                    inputs: vec![
                                        (IrNodeId(1), OutPort::default()),
                                        (IrNodeId(2), OutPort::default()),
                                    ],
                                    content_hash: ContentHash(base + 3),
                                },
                            ];
                            let mut g = FrameGraph {
                                working_color_domain: domain,
                                nodes,
                                output: Some(IrNodeId(0)),
                            };
                            assert_eq!(g.validate_working_color_domain(), Ok(()));
                            let cpu = eval_cpu::evaluate_at(
                                &g,
                                (width, height),
                                &mut EmptyProvider,
                                Some(IrNodeId(3)),
                            );
                            let texture = evaluator
                                .evaluate_with_tap(
                                    &g,
                                    (width, height),
                                    &mut NullFrameSource,
                                    Some(IrNodeId(3)),
                                )
                                .1
                                .unwrap()
                                .texture;
                            let pixels = read_texture_rgba16f(&gpu, &texture, width, height);
                            g.nodes.push(IrNode {
                                op: IrOp::SolidColor {
                                    color: LinearColor {
                                        r: -4.0 * alpha,
                                        g: 16.0 * alpha,
                                        b: 0.6 * alpha,
                                        a: alpha,
                                    },
                                },
                                inputs: vec![],
                                content_hash: ContentHash(base + 4),
                            });
                            g.nodes.push(IrNode {
                                op: IrOp::GradeMatteApply,
                                inputs: vec![
                                    (IrNodeId(4), OutPort::default()),
                                    (IrNodeId(0), OutPort::default()),
                                    (IrNodeId(3), OutPort::default()),
                                ],
                                content_hash: ContentHash(base + 5),
                            });
                            g.output = Some(IrNodeId(5));
                            assert_eq!(g.validate_working_color_domain(), Ok(()));
                            let mixed_cpu =
                                eval_cpu::evaluate(&g, (width, height), &mut EmptyProvider);
                            let mixed_texture = evaluator
                                .evaluate(&g, (width, height), &mut NullFrameSource)
                                .unwrap();
                            let mixed_pixels =
                                read_texture_rgba16f(&gpu, &mixed_texture, width, height);
                            for index in 0..(width * height) as usize {
                                let x = (index % width as usize) as f64 / width as f64
                                    + 0.5 / width as f64;
                                let y = (index / width as usize) as f64 / height as f64
                                    + 0.5 / height as f64;
                                let a = reference(&first, x, y);
                                let b = reference(&second, x, y);
                                let expected = match mode {
                                    GradeKeyMixMode::Union => a.max(b),
                                    GradeKeyMixMode::Intersect => a.min(b),
                                    GradeKeyMixMode::Subtract => (a - b).max(0.0),
                                    GradeKeyMixMode::Multiply => a * b,
                                } as f32;
                                for c in 0..3 {
                                    assert!(
                                        (cpu.pixels[index][c] - expected).abs() < 1e-5,
                                        "CPU case {cases} {index}"
                                    );
                                    assert!(
                                        (pixels[index][c] - expected).abs() < 0.003,
                                        "GPU case {cases} {index}: {} vs {expected}",
                                        pixels[index][c]
                                    );
                                }
                                assert_eq!(cpu.pixels[index][3], 1.0);
                                assert_eq!(pixels[index][3], 1.0);
                                for (c, source) in [-2.0, 8.0, 0.3].into_iter().enumerate() {
                                    let target = source * alpha * (1.0 + expected);
                                    assert!((mixed_cpu.pixels[index][c] - target).abs() < 0.0001);
                                    assert!(
                                        (mixed_pixels[index][c] - target).abs() < 0.03,
                                        "matte apply case {cases} {index}"
                                    );
                                }
                                assert_eq!(mixed_cpu.pixels[index][3], alpha);
                                assert_eq!(mixed_pixels[index][3], alpha);
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(cases, 432);
}

#[test]
fn native_resize_preserves_signed_hdr_alpha_and_matches_independent_fit_reference() {
    use photonic_video::graph::ir::FitMode;
    let gpu = GpuContext::request_blocking().expect("GPU required for native resize qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut serial = 71000u128;
    for (iw, ih, ow, oh) in [
        (7u32, 5u32, 12u32, 10u32),
        (8, 4, 4, 8),
        (9, 7, 3, 2),
        (1, 1, 5, 3),
    ] {
        for fit in [FitMode::Fit, FitMode::Fill, FitMode::Stretch] {
            serial += 4;
            let graph = FrameGraph {
                working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                nodes: vec![
                    IrNode {
                        op: IrOp::SolidColor {
                            color: LinearColor {
                                r: -0.25,
                                g: 1.5,
                                b: 0.125,
                                a: 0.5,
                            },
                        },
                        inputs: vec![],
                        content_hash: ContentHash(serial),
                    },
                    IrNode {
                        op: IrOp::Crop {
                            left: 0.25,
                            top: 0.0,
                            right: 0.0,
                            bottom: 0.0,
                        },
                        inputs: vec![(IrNodeId(0), OutPort::default())],
                        content_hash: ContentHash(serial + 1),
                    },
                    IrNode {
                        op: IrOp::Resize { w: ow, h: oh, fit },
                        inputs: vec![(IrNodeId(1), OutPort::default())],
                        content_hash: ContentHash(serial + 2),
                    },
                ],
                output: Some(IrNodeId(2)),
            };
            assert!(graph.validate_working_color_domain().is_ok());
            let cpu = eval_cpu::evaluate(&graph, (iw, ih), &mut EmptyProvider);
            assert_eq!((cpu.width, cpu.height), (ow, oh));
            let texture = evaluator
                .evaluate(&graph, (iw, ih), &mut NullFrameSource)
                .unwrap();
            let pixels = read_texture_rgba16f(&gpu, &texture, ow, oh);
            let (iwf, ihf, owf, ohf) = (iw as f64, ih as f64, ow as f64, oh as f64);
            let (sx, sy) = match fit {
                FitMode::Stretch => (owf / iwf, ohf / ihf),
                FitMode::Fit => {
                    let s = (owf / iwf).min(ohf / ihf);
                    (s, s)
                }
                FitMode::Fill => {
                    let s = (owf / iwf).max(ohf / ihf);
                    (s, s)
                }
            };
            let (ox, oy) = ((owf - iwf * sx) / 2.0, (ohf - ihf * sy) / 2.0);
            for y in 0..oh {
                for x in 0..ow {
                    let (dx, dy) = (x as f64 + 0.5, y as f64 + 0.5);
                    let boxed = matches!(fit, FitMode::Fit)
                        && (dx < ox || dx >= owf - ox || dy < oy || dy >= ohf - oy);
                    let sample_x = ((dx - ox) / sx - 0.5).clamp(0.0, iwf - 1.0);
                    let lo = sample_x.floor();
                    let hi = (lo + 1.0).min(iwf - 1.0);
                    let t = sample_x - lo;
                    let covered = |p: f64| if (p + 0.5) / iwf >= 0.25 { 1.0 } else { 0.0 };
                    let coverage = if boxed {
                        0.0
                    } else {
                        (1.0 - t) * covered(lo) + t * covered(hi)
                    };
                    let idx = (y * ow + x) as usize;
                    for (c, value) in [-0.25, 1.5, 0.125, 0.5].iter().enumerate() {
                        let expected = value * coverage;
                        assert!(
                            (cpu.pixels[idx][c] as f64 - expected).abs() < 2e-5,
                            "CPU {fit:?} {iw}x{ih}->{ow}x{oh} ({x},{y}): {:?} expected {expected}",
                            cpu.pixels[idx]
                        );
                        assert!(
                            (pixels[idx][c] as f64 - expected).abs() < 0.004,
                            "GPU {fit:?} {iw}x{ih}->{ow}x{oh} ({x},{y}): {:?} expected {expected}",
                            pixels[idx]
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn native_resize_draft_uses_scaled_intermediates_and_isolates_cached_sizes() {
    use photonic_video::graph::ir::FitMode;
    let gpu = GpuContext::request_blocking().expect("GPU required for resize Draft qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let graph = FrameGraph {
        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
        nodes: vec![
            IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: -0.25,
                        g: 1.5,
                        b: 0.125,
                        a: 0.5,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(72001),
            },
            IrNode {
                op: IrOp::Resize {
                    w: 4,
                    h: 2,
                    fit: FitMode::Stretch,
                },
                inputs: vec![(IrNodeId(0), OutPort::default())],
                content_hash: ContentHash(72002),
            },
            IrNode {
                op: IrOp::Resize {
                    w: 8,
                    h: 4,
                    fit: FitMode::Stretch,
                },
                inputs: vec![(IrNodeId(1), OutPort::default())],
                content_hash: ContentHash(72003),
            },
            IrNode {
                op: IrOp::NativeCanvas { w: 8, h: 4 },
                inputs: vec![(IrNodeId(2), OutPort::default())],
                content_hash: ContentHash(72004),
            },
        ],
        output: Some(IrNodeId(3)),
    };
    for canvas in [(4, 2), (8, 4), (4, 2)] {
        let resized = eval_cpu::evaluate_at(&graph, canvas, &mut EmptyProvider, Some(IrNodeId(1)));
        assert_eq!(
            (resized.width, resized.height),
            (canvas.0 / 2, canvas.1 / 2)
        );
        let cpu = eval_cpu::evaluate(&graph, canvas, &mut EmptyProvider);
        assert_eq!((cpu.width, cpu.height), canvas);
        let texture = evaluator
            .evaluate(&graph, canvas, &mut NullFrameSource)
            .unwrap();
        for pixel in read_texture_rgba16f(&gpu, &texture, canvas.0, canvas.1) {
            for (value, expected) in pixel.iter().zip([-0.25, 1.5, 0.125, 0.5]) {
                assert!((value - expected).abs() < 0.002, "{canvas:?}: {pixel:?}");
            }
        }
    }
}

#[test]
fn native_vignette_matches_independent_radial_gain_and_preserves_alpha() {
    let gpu = GpuContext::request_blocking().expect("GPU required for AP1 vignette qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut serial = 74000u128;
    for (w, h) in [(1u32, 1u32), (7, 5), (8, 4), (3, 9)] {
        for amount in [-1.0f32, -0.5, 0.0, 0.5, 1.0] {
            for feather in [0.0f32, 0.1, 0.5, 1.0] {
                for rgba in [[-0.25f32, 1.5, 0.125, 0.5], [2.0, 0.3, 4.0, 1.0], [0.0; 4]] {
                    serial += 2;
                    let graph = FrameGraph {
                        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                        nodes: vec![
                            IrNode {
                                op: IrOp::SolidColor {
                                    color: LinearColor {
                                        r: rgba[0],
                                        g: rgba[1],
                                        b: rgba[2],
                                        a: rgba[3],
                                    },
                                },
                                inputs: vec![],
                                content_hash: ContentHash(serial),
                            },
                            IrNode {
                                op: IrOp::NativeVignette { amount, feather },
                                inputs: vec![(IrNodeId(0), OutPort::default())],
                                content_hash: ContentHash(serial + 1),
                            },
                        ],
                        output: Some(IrNodeId(1)),
                    };
                    assert!(graph.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
                    let tex = evaluator
                        .evaluate(&graph, (w, h), &mut NullFrameSource)
                        .unwrap();
                    let pixels = read_texture_rgba16f(&gpu, &tex, w, h);
                    let (cx, cy) = ((w - 1) as f64 / 2.0, (h - 1) as f64 / 2.0);
                    let radius = cx.hypot(cy).max(0.0001);
                    let width = (feather as f64).max(0.0001);
                    for y in 0..h {
                        for x in 0..w {
                            let distance = (x as f64 - cx).hypot(y as f64 - cy) / radius;
                            let t = ((distance - (1.0 - width)) / width).clamp(0.0, 1.0);
                            let gain = 1.0 + amount as f64 * (3.0 * t * t - 2.0 * t * t * t);
                            let idx = (y * w + x) as usize;
                            for c in 0..4 {
                                let expected = rgba[c] as f64 * if c == 3 { 1.0 } else { gain };
                                assert!((cpu.pixels[idx][c] as f64-expected).abs()<0.0001,"CPU {w}x{h} {amount}/{feather} {x},{y}: {:?} expected {expected}",cpu.pixels[idx]);
                                assert!((pixels[idx][c] as f64-expected).abs()<0.006,"GPU {w}x{h} {amount}/{feather} {x},{y}: {:?} expected {expected}",pixels[idx]);
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn native_grain_matches_integer_reference_preserves_alpha_and_draft_identity() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/native_grain_reference.json")).unwrap();
    let gpu = GpuContext::request_blocking().expect("GPU required for native deterministic grain");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut serial = 76000u128;
    for case in fixture["cases"].as_array().unwrap() {
        let w = case["width"].as_u64().unwrap() as u32;
        let h = case["height"].as_u64().unwrap() as u32;
        let seed = case["seed"].as_u64().unwrap() as u32;
        for mono in [true, false] {
            for amount in [0.0f32, 0.125, 1.0] {
                for rgba in [[-0.25f32, 1.5, 0.125, 0.5], [2.0, 0.3, 4.0, 1.0], [0.0; 4]] {
                    serial += 3;
                    let graph = FrameGraph {
                        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                        nodes: vec![
                            IrNode {
                                op: IrOp::SolidColor {
                                    color: LinearColor {
                                        r: rgba[0],
                                        g: rgba[1],
                                        b: rgba[2],
                                        a: rgba[3],
                                    },
                                },
                                inputs: vec![],
                                content_hash: ContentHash(serial),
                            },
                            IrNode {
                                op: IrOp::NativeGrain {
                                    amount,
                                    monochrome: mono,
                                    seed,
                                    pixel_scale: [1.0; 2],
                                },
                                inputs: vec![(IrNodeId(0), OutPort::default())],
                                content_hash: ContentHash(serial + 1),
                            },
                            IrNode {
                                op: IrOp::NativeCanvas { w, h },
                                inputs: vec![(IrNodeId(1), OutPort::default())],
                                content_hash: ContentHash(serial + 2),
                            },
                        ],
                        output: Some(IrNodeId(2)),
                    };
                    assert!(graph.validate_working_color_domain().is_ok());
                    let canvases = if (w, h) == (8, 4) {
                        vec![(4, 2), (8, 4), (4, 2)]
                    } else {
                        vec![(w, h)]
                    };
                    for (cw, ch) in canvases {
                        let cpu = eval_cpu::evaluate(&graph, (cw, ch), &mut EmptyProvider);
                        let tex = evaluator
                            .evaluate(&graph, (cw, ch), &mut NullFrameSource)
                            .unwrap();
                        let pixels = read_texture_rgba16f(&gpu, &tex, cw, ch);
                        for y in 0..ch {
                            for x in 0..cw {
                                let ax = ((x as f64 + 0.5) * w as f64 / cw as f64).floor() as u32;
                                let ay = ((y as f64 + 0.5) * h as f64 / ch as f64).floor() as u32;
                                for c in 0..4 {
                                    let noise = if c == 3 {
                                        0.0
                                    } else {
                                        case["noise"][(ay * w + ax) as usize]
                                            [if mono { 0 } else { c }]
                                        .as_f64()
                                        .unwrap()
                                    };
                                    let expected =
                                        rgba[c] as f64 + noise * amount as f64 * rgba[3] as f64;
                                    let idx = (y * cw + x) as usize;
                                    assert!((cpu.pixels[idx][c] as f64-expected).abs()<1e-5,"CPU seed{seed} {mono}/{amount} {cw}x{ch}: {:?} vs {expected}",cpu.pixels[idx]);
                                    assert!((pixels[idx][c] as f64-expected).abs()<0.006,"GPU seed{seed} {mono}/{amount} {cw}x{ch}: {:?} vs {expected}",pixels[idx]);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn native_denoise_matches_independent_bilateral_reference_and_preserves_coverage() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/native_grain_reference.json")).unwrap();
    let gpu =
        GpuContext::request_blocking().expect("GPU required for native bilateral qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut serial = 80000u128;
    for case in fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["seed"] == 1)
    {
        let w = case["width"].as_u64().unwrap() as u32;
        let h = case["height"].as_u64().unwrap() as u32;
        for strength in [0.0f32, 0.25, 0.5, 1.0] {
            for alpha in [0.25f32, 0.5, 1.0] {
                for edge in [false, true] {
                    serial += 9;
                    let solid = |rgba: [f32; 4], hash| IrNode {
                        op: IrOp::SolidColor {
                            color: LinearColor {
                                r: rgba[0],
                                g: rgba[1],
                                b: rgba[2],
                                a: rgba[3],
                            },
                        },
                        inputs: vec![],
                        content_hash: ContentHash(hash),
                    };
                    let graph = FrameGraph {
                        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                        nodes: vec![
                            solid([-0.5 * alpha, 3.0 * alpha, 0.25 * alpha, alpha], serial),
                            IrNode {
                                op: IrOp::NativeGrain {
                                    amount: 0.1,
                                    monochrome: false,
                                    seed: 1,
                                    pixel_scale: [1.0; 2],
                                },
                                inputs: vec![(IrNodeId(0), OutPort::default())],
                                content_hash: ContentHash(serial + 1),
                            },
                            IrNode {
                                op: IrOp::Crop {
                                    left: if edge { 0.5 } else { 0.0 },
                                    top: 0.0,
                                    right: 0.0,
                                    bottom: 0.0,
                                },
                                inputs: vec![(IrNodeId(1), OutPort::default())],
                                content_hash: ContentHash(serial + 2),
                            },
                            solid(
                                if edge {
                                    [1.5, -0.1875, 0.375, 0.75]
                                } else {
                                    [0.0; 4]
                                },
                                serial + 3,
                            ),
                            IrNode {
                                op: IrOp::Merge {
                                    mode: BlendMode::Normal,
                                    opacity: 1.0,
                                },
                                inputs: vec![
                                    (IrNodeId(2), OutPort::default()),
                                    (IrNodeId(3), OutPort::default()),
                                ],
                                content_hash: ContentHash(serial + 4),
                            },
                            IrNode {
                                op: IrOp::Crop {
                                    left: 0.15,
                                    top: 0.0,
                                    right: 0.0,
                                    bottom: 0.0,
                                },
                                inputs: vec![(IrNodeId(4), OutPort::default())],
                                content_hash: ContentHash(serial + 5),
                            },
                            IrNode {
                                op: IrOp::NativeDenoise {
                                    strength,
                                    spatial_scale: 1.0,
                                },
                                inputs: vec![(IrNodeId(5), OutPort::default())],
                                content_hash: ContentHash(serial + 6),
                            },
                            IrNode {
                                op: IrOp::NativeCanvas { w, h },
                                inputs: vec![(IrNodeId(6), OutPort::default())],
                                content_hash: ContentHash(serial + 7),
                            },
                        ],
                        output: Some(IrNodeId(7)),
                    };
                    let canvases = if (w, h) == (8, 4) {
                        vec![(4, 2), (8, 4), (4, 2)]
                    } else {
                        vec![(w, h)]
                    };
                    for (cw, ch) in canvases {
                        let mut source = Vec::new();
                        for y in 0..ch {
                            for x in 0..cw {
                                let ax = ((x as f64 + 0.5) * w as f64 / cw as f64).floor() as u32;
                                let ay = ((y as f64 + 0.5) * h as f64 / ch as f64).floor() as u32;
                                let mut p = [0.0f64; 4];
                                if !edge || (x as f64 + 0.5) / cw as f64 >= 0.5 {
                                    p[3] = alpha as f64;
                                    for c in 0..3 {
                                        p[c] = ([-0.5, 3.0, 0.25][c]
                                            + case["noise"][(ay * w + ax) as usize][c]
                                                .as_f64()
                                                .unwrap()
                                                * 0.1)
                                            * alpha as f64;
                                    }
                                }
                                if edge {
                                    for c in 0..3 {
                                        p[c] += [1.5, -0.1875, 0.375][c] * (1.0 - p[3]);
                                    }
                                    p[3] += 0.75 * (1.0 - p[3]);
                                }
                                if (x as f64 + 0.5) / (cw as f64) < 0.15 {
                                    p = [0.0; 4];
                                }
                                source.push(p);
                            }
                        }
                        let scale = (cw as f64 / w as f64).min(ch as f64 / h as f64);
                        let radius = (2.0 * scale).ceil().clamp(1.0, 8.0) as i32;
                        let ss = ((1.5 + 1.5 * strength as f64) * scale).max(0.0001);
                        let sr = (10.0 + 70.0 * strength as f64) / 255.0;
                        let cpu = eval_cpu::evaluate(&graph, (cw, ch), &mut EmptyProvider);
                        let tex = evaluator
                            .evaluate(&graph, (cw, ch), &mut NullFrameSource)
                            .unwrap();
                        let pixels = read_texture_rgba16f(&gpu, &tex, cw, ch);
                        for y in 0..ch {
                            for x in 0..cw {
                                let idx = (y * cw + x) as usize;
                                let center = source[idx];
                                let mut expected = center;
                                if strength > 0.0 && center[3] > 0.0 {
                                    let mut sum = [0.0; 3];
                                    let mut coverage = 0.0;
                                    for dy in -radius..=radius {
                                        for dx in -radius..=radius {
                                            let nx = (x as i32 + dx).clamp(0, cw as i32 - 1) as u32;
                                            let ny = (y as i32 + dy).clamp(0, ch as i32 - 1) as u32;
                                            let q = source[(ny * cw + nx) as usize];
                                            if q[3] == 0.0 {
                                                continue;
                                            }
                                            let dist = (0..3)
                                                .map(|c| {
                                                    (q[c] / q[3] - center[c] / center[3]).powi(2)
                                                })
                                                .sum::<f64>();
                                            let weight = (-f64::from(dx * dx + dy * dy)
                                                / (2.0 * ss * ss)
                                                - dist / (2.0 * sr * sr))
                                                .exp();
                                            for c in 0..3 {
                                                sum[c] += q[c] * weight;
                                            }
                                            coverage += q[3] * weight;
                                        }
                                    }
                                    for c in 0..3 {
                                        expected[c] = sum[c] / coverage * center[3];
                                    }
                                }
                                for c in 0..4 {
                                    assert!((cpu.pixels[idx][c] as f64-expected[c]).abs()<2e-5,"CPU strength{strength} alpha{alpha} edge{edge} {cw}x{ch}: {:?} expected {expected:?}",cpu.pixels[idx]);
                                    assert!((pixels[idx][c] as f64-expected[c]).abs()<0.007,"GPU strength{strength} alpha{alpha} edge{edge} {cw}x{ch}: {:?} expected {expected:?}",pixels[idx]);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn native_denoise_reduces_flat_region_variance_without_changing_alpha() {
    let gpu = GpuContext::request_blocking().expect("GPU required for noise reduction behavior");
    let graph = FrameGraph {
        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
        nodes: vec![
            IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: -0.25,
                        g: 1.5,
                        b: 0.125,
                        a: 0.5,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(90001),
            },
            IrNode {
                op: IrOp::NativeGrain {
                    amount: 0.05,
                    monochrome: false,
                    seed: 7,
                    pixel_scale: [1.0; 2],
                },
                inputs: vec![(IrNodeId(0), OutPort::default())],
                content_hash: ContentHash(90002),
            },
            IrNode {
                op: IrOp::NativeDenoise {
                    strength: 0.8,
                    spatial_scale: 1.0,
                },
                inputs: vec![(IrNodeId(1), OutPort::default())],
                content_hash: ContentHash(90003),
            },
        ],
        output: Some(IrNodeId(2)),
    };
    let before = eval_cpu::evaluate_at(&graph, (32, 16), &mut EmptyProvider, Some(IrNodeId(1)));
    let cpu = eval_cpu::evaluate(&graph, (32, 16), &mut EmptyProvider);
    let tex = Evaluator::new(gpu.clone())
        .evaluate(&graph, (32, 16), &mut NullFrameSource)
        .unwrap();
    let after = read_texture_rgba16f(&gpu, &tex, 32, 16);
    let variance = |pixels: &[[f32; 4]], c: usize| {
        let mean = pixels.iter().map(|p| p[c] as f64).sum::<f64>() / pixels.len() as f64;
        pixels
            .iter()
            .map(|p| (p[c] as f64 - mean).powi(2))
            .sum::<f64>()
            / pixels.len() as f64
    };
    for c in 0..3 {
        assert!(variance(&cpu.pixels, c) < variance(&before.pixels, c) * 0.5);
        assert!(variance(&after, c) < variance(&before.pixels, c) * 0.5);
    }
    assert!(after
        .iter()
        .all(|p| p[3] == 0.5 && p[0] < 0.0 && p[1] > 1.0));
}

#[test]
fn native_title_raster_converts_srgb_glyph_color_to_ap1_without_losing_coverage() {
    use photonic_render::caption::CaptionWordRun;
    use photonic_video::contract::{CaptionCueRun, ResolvedTextBlock};
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/native_title_reference.json")).unwrap();
    let gpu = GpuContext::request_blocking().expect("GPU required for managed title qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 95000u128;
    for case in reference["cases"].as_array().unwrap() {
        hash += 3;
        let color = std::array::from_fn(|i| case["rgba8"][i].as_u64().unwrap() as u8);
        let block = ResolvedTextBlock {
            cue: Some(CaptionCueRun {
                direction: photonic_core::timeline::TextDirection::Auto,
                words: vec![CaptionWordRun {
                    metrics: None,
                    text: "HI".into(),
                    font_family: "sans-serif".into(),
                    font_weight: 700,
                    color,
                }],
                font_size: 40.0,
                line_height_mul: 1.2,
                anchor: [0.5, 0.2],
                max_width: 0.9,
            }),
        };
        let graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![
                IrNode {
                    op: IrOp::NativeTextGen { block },
                    inputs: vec![],
                    content_hash: ContentHash(hash),
                },
                IrNode {
                    op: IrOp::NativeCanvas { w: 190, h: 84 },
                    inputs: vec![(IrNodeId(0), OutPort::default())],
                    content_hash: ContentHash(hash + 1),
                },
            ],
            output: Some(IrNodeId(1)),
        };
        assert!(graph.validate_working_color_domain().is_ok());
        for (w, h) in [(95, 42), (190, 84), (95, 42)] {
            let texture = evaluator
                .evaluate(&graph, (w, h), &mut NullFrameSource)
                .unwrap();
            let pixels = read_texture_rgba16f(&gpu, &texture, w, h);
            let lit: Vec<_> = pixels.iter().filter(|p| p[3] > 0.03).collect();
            assert!(
                lit.len() > 10,
                "managed title must contain actual glyph coverage"
            );
            for p in lit {
                assert!(p[3] <= color[3] as f32 / 255.0 + 0.003);
                for c in 0..3 {
                    let expected = case["straight_ap1"][c].as_f64().unwrap() * p[3] as f64;
                    assert!(
                        (p[c] as f64 - expected).abs() < 0.003,
                        "{color:?}: {p:?}, channel{c} expected {expected}"
                    );
                }
            }
            let padded = read_texture_rgba16f(&gpu, &texture, texture.width(), texture.height());
            for y in 0..texture.height() {
                for x in 0..texture.width() {
                    if x >= w || y >= h {
                        assert_eq!(padded[(y * texture.width() + x) as usize], [0.0; 4]);
                    }
                }
            }
        }
    }
}

#[test]
fn native_title_checked_compositor_rejects_invalid_targets() {
    let gpu = GpuContext::request_blocking().expect("GPU required for checked title compositor");
    let compositor = photonic_render::caption::CaptionCompositor::new(
        gpu.device(),
        gpu.queue(),
        wgpu::TextureFormat::Rgba16Float,
    );
    for format in [
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Rgba16Float,
    ] {
        let target = gpu.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("title-invalid-target"),
            size: wgpu::Extent3d {
                width: 8,
                height: 8,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        assert!(compositor
            .composite_checked(gpu.device(), gpu.queue(), &target, &[], 9, 8)
            .is_err());
        assert!(compositor
            .composite_checked(gpu.device(), gpu.queue(), &target, &[], 0, 8)
            .is_err());
        assert_eq!(
            compositor
                .composite_checked(gpu.device(), gpu.queue(), &target, &[], 8, 8)
                .is_ok(),
            format == wgpu::TextureFormat::Rgba16Float
        );
    }
}

#[test]
fn native_captions_are_display_authored_and_draft_cache_safe() {
    use photonic_render::caption::CaptionWordRun;
    use photonic_video::contract::{CaptionBatch, CaptionCueRun};
    let gpu = GpuContext::request_blocking().expect("GPU required for caption qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    for (i, color) in [
        [255, 255, 255, 255],
        [255, 0, 0, 128],
        [40, 180, 220, 160],
        [1, 10, 128, 255],
    ]
    .into_iter()
    .enumerate()
    {
        for video in [false, true] {
            let batch = CaptionBatch {
                cues: vec![CaptionCueRun {
                    direction: photonic_core::timeline::TextDirection::Auto,
                    words: vec![CaptionWordRun {
                        metrics: None,
                        text: "HI".into(),
                        font_family: "sans-serif".into(),
                        font_weight: 700,
                        color,
                    }],
                    font_size: 40.0,
                    line_height_mul: 1.2,
                    anchor: [0.5, 0.2],
                    max_width: 0.9,
                }],
            };
            let mut hash = 100000 + i as u128 * 100 + video as u128 * 10;
            let mut node = |op, input: Option<usize>| {
                hash += 1;
                IrNode {
                    op,
                    inputs: input
                        .map(|i| vec![(IrNodeId(i as u32), OutPort::default())])
                        .unwrap_or_default(),
                    content_hash: ContentHash(hash),
                }
            };
            let graph = FrameGraph {
                working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                nodes: vec![
                    node(
                        IrOp::SolidColor {
                            color: LinearColor {
                                r: 0.0,
                                g: 0.0,
                                b: 0.0,
                                a: 0.0,
                            },
                        },
                        None,
                    ),
                    node(IrOp::NativeCanvas { w: 190, h: 84 }, Some(0)),
                    node(
                        if video {
                            IrOp::NativeSdrVideoOutput
                        } else {
                            IrOp::NativeSdrOutput
                        },
                        Some(1),
                    ),
                    node(
                        IrOp::NativeCaptionOutput {
                            cue_batch: batch,
                            video,
                            paints: vec![],
                        },
                        Some(2),
                    ),
                ],
                output: Some(IrNodeId(3)),
            };
            assert!(graph.validate_working_color_domain().is_ok());
            assert_eq!(
                graph.output_color_encoding().unwrap(),
                if video {
                    FrameColorEncoding::Bt709Video
                } else {
                    FrameColorEncoding::SrgbDisplay
                }
            );
            let mut draft = None;
            for (w, h) in [(95, 42), (190, 84), (95, 42)] {
                let texture = evaluator
                    .evaluate(&graph, (w, h), &mut NullFrameSource)
                    .unwrap();
                let pixels = read_texture_rgba16f(&gpu, &texture, w, h);
                let covered: Vec<_> = pixels.iter().filter(|p| p[3] > 0.03).collect();
                assert!(covered.len() > 10);
                for p in covered {
                    assert!(p[3] <= color[3] as f32 / 255.0 + 0.003);
                    for c in 0..3 {
                        let s = color[c] as f64 / 255.0;
                        let l = if s <= 0.04045 {
                            s / 12.92
                        } else {
                            ((s + 0.055) / 1.055).powf(2.4)
                        };
                        let expected = if video {
                            if l < 0.018 {
                                4.5 * l
                            } else {
                                1.099 * l.powf(0.45) - 0.099
                            }
                        } else {
                            s
                        };
                        assert!(
                            (p[c] as f64 - expected * p[3] as f64).abs() < 0.003,
                            "color={color:?},video={video},pixel={p:?},expected={expected}"
                        );
                    }
                }
                if w == 95 {
                    if let Some(previous) = &draft {
                        assert_eq!(previous, &pixels);
                    } else {
                        draft = Some(pixels.clone());
                    }
                }
                let padded =
                    read_texture_rgba16f(&gpu, &texture, texture.width(), texture.height());
                for y in 0..texture.height() {
                    for x in 0..texture.width() {
                        if x >= w || y >= h {
                            assert_eq!(padded[(y * texture.width() + x) as usize], [0.0; 4]);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn native_caption_transfer_matches_independent_binary64_piecewise_reference() {
    use photonic_render::native_caption::NativeCaptionTransferPass;
    let gpu =
        GpuContext::request_blocking().expect("GPU required for caption transfer qualification");
    let pass = NativeCaptionTransferPass::new(gpu.device());
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 110000;
    for rgb in [
        [-0.05, 0.003, 0.5],
        [0.018, 0.081, 1.2],
        [0.0031308, 0.04045, 0.9],
        [0.0, 1.0, 0.2],
    ] {
        for alpha in [0.0, 0.125, 1.0] {
            hash += 1;
            let graph = FrameGraph {
                working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                nodes: vec![IrNode {
                    op: IrOp::SolidColor {
                        color: LinearColor {
                            r: rgb[0] * alpha,
                            g: rgb[1] * alpha,
                            b: rgb[2] * alpha,
                            a: alpha,
                        },
                    },
                    inputs: vec![],
                    content_hash: ContentHash(hash),
                }],
                output: Some(IrNodeId(0)),
            };
            let src = evaluator
                .evaluate(&graph, (3, 1), &mut NullFrameSource)
                .unwrap();
            let actual_input = read_texture_rgba16f(&gpu, &src, 3, 1);
            let target = gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: src.size(),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            for video in [false, true] {
                for encode in [false, true] {
                    pass.apply_into(gpu.device(), gpu.queue(), &src, &target, video, encode)
                        .unwrap();
                    let pixels = read_texture_rgba16f(&gpu, &target, 3, 1);
                    for (p, q) in actual_input.iter().zip(pixels) {
                        assert_eq!(q[3], p[3]);
                        if p[3] == 0.0 {
                            assert_eq!(q, [0.0; 4]);
                            continue;
                        }
                        for c in 0..3 {
                            let x = p[c] as f64 / p[3] as f64;
                            let y = match (video, encode) {
                                (false, false) => {
                                    if x <= 0.04045 {
                                        x / 12.92
                                    } else {
                                        ((x + 0.055) / 1.055).powf(2.4)
                                    }
                                }
                                (false, true) => {
                                    if x <= 0.0031308 {
                                        x * 12.92
                                    } else {
                                        1.055 * x.powf(1.0 / 2.4) - 0.055
                                    }
                                }
                                (true, false) => {
                                    if x < 0.081 {
                                        x / 4.5
                                    } else {
                                        ((x + 0.099) / 1.099).powf(1.0 / 0.45)
                                    }
                                }
                                (true, true) => {
                                    if x < 0.018 {
                                        x * 4.5
                                    } else {
                                        1.099 * x.powf(0.45) - 0.099
                                    }
                                }
                            } * p[3] as f64;
                            assert!(
                                (q[c] as f64 - y).abs() < 0.002,
                                "video={video},encode={encode},{p:?}->{q:?},expected={y}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn native_caption_alpha_over_uses_display_linear_light_and_keeps_program_tap() {
    use photonic_render::caption::CaptionWordRun;
    use photonic_video::contract::{CaptionBatch, CaptionCueRun};
    let gpu =
        GpuContext::request_blocking().expect("GPU required for caption alpha-over qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let decode = |x: f64, video| {
        if video {
            if x < 0.081 {
                x / 4.5
            } else {
                ((x + 0.099) / 1.099).powf(1.0 / 0.45)
            }
        } else if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };
    let encode = |x: f64, video| {
        if video {
            if x < 0.018 {
                x * 4.5
            } else {
                1.099 * x.powf(0.45) - 0.099
            }
        } else if x <= 0.0031308 {
            x * 12.92
        } else {
            1.055 * x.powf(1.0 / 2.4) - 0.055
        }
    };
    for video in [false, true] {
        let color = [20, 100, 240, 160];
        let cue_batch = CaptionBatch {
            cues: vec![CaptionCueRun {
                direction: photonic_core::timeline::TextDirection::Auto,
                words: vec![CaptionWordRun {
                    metrics: None,
                    text: "HI".into(),
                    font_family: "sans-serif".into(),
                    font_weight: 700,
                    color,
                }],
                font_size: 40.0,
                line_height_mul: 1.2,
                anchor: [0.5, 0.2],
                max_width: 0.9,
            }],
        };
        let mut hash = 120000 + video as u128 * 100;
        let mut node = |op, input: Option<u32>| {
            hash += 1;
            IrNode {
                op,
                inputs: input
                    .map(|i| vec![(IrNodeId(i), OutPort::default())])
                    .unwrap_or_default(),
                content_hash: ContentHash(hash),
            }
        };
        let graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![
                node(
                    IrOp::SolidColor {
                        color: LinearColor {
                            r: 0.4,
                            g: 0.025,
                            b: 0.1,
                            a: 0.5,
                        },
                    },
                    None,
                ),
                node(IrOp::NativeCanvas { w: 190, h: 84 }, Some(0)),
                node(
                    if video {
                        IrOp::NativeSdrVideoOutput
                    } else {
                        IrOp::NativeSdrOutput
                    },
                    Some(1),
                ),
                node(
                    IrOp::NativeCaptionOutput {
                        cue_batch,
                        video,
                        paints: vec![],
                    },
                    Some(2),
                ),
            ],
            output: Some(IrNodeId(3)),
        };
        let (out, tap) = evaluator.evaluate_with_tap_frame(
            &graph,
            (190, 84),
            &mut NullFrameSource,
            Some(IrNodeId(2)),
        );
        let out = read_texture_rgba16f(&gpu, &out.unwrap().texture, 190, 84);
        let tap = read_texture_rgba16f(&gpu, &tap.unwrap().texture, 190, 84);
        assert!(tap.iter().all(|p| p == &tap[0]));
        let mut hits = 0;
        for (p, bg) in out.iter().zip(tap) {
            let a = p[3] as f64;
            let b = bg[3] as f64;
            let coverage = (a - b) / (1.0 - b);
            if coverage > 0.05 {
                hits += 1;
            }
            for c in 0..3 {
                let fg = decode(color[c] as f64 / 255.0, false);
                let back = decode(bg[c] as f64 / b, video);
                let expected = encode((fg * coverage + back * b * (1.0 - coverage)) / a, video) * a;
                assert!(
                    (p[c] as f64 - expected).abs() < 0.003,
                    "{p:?}, channel{c}, expected={expected}"
                );
            }
        }
        assert!(hits > 20);
    }
}

#[test]
fn managed_caption_decorations_match_independent_outer_mask_and_rounded_box_reference() {
    use photonic_render::caption_decoration::{CaptionDecoration, CaptionDecorationPass};
    let gpu =
        GpuContext::request_blocking().expect("GPU required for managed decoration qualification");
    let pass = CaptionDecorationPass::new(gpu.device());
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 130000;
    for (w, h) in [(13u32, 9u32), (1, 1)] {
        let mut source = |op| {
            hash += 1;
            FrameGraph {
                working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                nodes: vec![IrNode {
                    op,
                    inputs: vec![],
                    content_hash: ContentHash(hash),
                }],
                output: Some(IrNodeId(0)),
            }
        };
        let mask = evaluator
            .evaluate(
                &source(IrOp::NativeMaskShape {
                    rect: true,
                    center: [0.5, 0.5],
                    size: [0.35, 0.45],
                    rotation: 0.0,
                    feather: 0.0,
                }),
                (w, h),
                &mut NullFrameSource,
            )
            .unwrap();
        let mask_pixels = read_texture_rgba16f(&gpu, &mask, w, h);
        let base = evaluator
            .evaluate(
                &source(IrOp::SolidColor {
                    color: LinearColor {
                        r: 0.1,
                        g: 0.02,
                        b: 0.03,
                        a: 0.5,
                    },
                }),
                (w, h),
                &mut NullFrameSource,
            )
            .unwrap();
        let base_pixels = read_texture_rgba16f(&gpu, &base, w, h);
        let base_padded = read_texture_rgba16f(&gpu, &base, base.width(), base.height());
        for width in [0.0f32, 0.5, 2.0, 8.0] {
            for boxed in [false, true] {
                for alpha in [0u8, 128, 255] {
                    let style = CaptionDecoration {
                        stroke: Some(([200, 80, 20, alpha], width)),
                        background: boxed.then_some(([20, 50, 180, 128], 1.25, 2.5)),
                    };
                    let bounds = [2.0, 1.0, 10.0, 7.0];
                    let target = gpu.device().create_texture(&wgpu::TextureDescriptor {
                        label: None,
                        size: base.size(),
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba16Float,
                        usage: wgpu::TextureUsages::COPY_DST
                            | wgpu::TextureUsages::COPY_SRC
                            | wgpu::TextureUsages::RENDER_ATTACHMENT
                            | wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    });
                    let mut encoder = gpu.device().create_command_encoder(&Default::default());
                    encoder.copy_texture_to_texture(
                        base.as_image_copy(),
                        target.as_image_copy(),
                        base.size(),
                    );
                    gpu.queue().submit([encoder.finish()]);
                    pass.composite(
                        gpu.device(),
                        gpu.queue(),
                        &mask,
                        &target,
                        (w, h),
                        bounds,
                        &style,
                    )
                    .unwrap();
                    let pixels = read_texture_rgba16f(&gpu, &target, w, h);
                    let sample = |x: i32, y: i32| -> f64 {
                        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                            0.0
                        } else {
                            mask_pixels[(y as u32 * w + x as u32) as usize][3] as f64
                        }
                    };
                    let linear = |v: u8| {
                        let v = v as f64 / 255.0;
                        if v <= 0.04045 {
                            v / 12.92
                        } else {
                            ((v + 0.055) / 1.055).powf(2.4)
                        }
                    };
                    for y in 0..h {
                        for x in 0..w {
                            let original = sample(x as i32, y as i32);
                            let mut expanded = original;
                            if width > 0.0 {
                                let radius = (width as f64 + 0.5).ceil() as i32;
                                for dy in -radius..=radius {
                                    for dx in -radius..=radius {
                                        let weight = (width as f64 + 0.5
                                            - ((dx * dx + dy * dy) as f64).sqrt())
                                        .clamp(0.0, 1.0);
                                        expanded = expanded
                                            .max(sample(x as i32 + dx, y as i32 + dy) * weight);
                                    }
                                }
                            }
                            let sa = (expanded - original).max(0.0) * alpha as f64 / 255.0;
                            let ba = if boxed {
                                let center = [6.0, 4.0];
                                let half = [5.25f64, 4.25];
                                let r = 2.5f64.min(half[0]).min(half[1]);
                                let q = [
                                    (x as f64 + 0.5 - center[0]).abs() - half[0] + r,
                                    (y as f64 + 0.5 - center[1]).abs() - half[1] + r,
                                ];
                                let d = q[0].max(0.0).hypot(q[1].max(0.0))
                                    + q[0].max(q[1]).min(0.0)
                                    - r;
                                (0.5 - d).clamp(0.0, 1.0) * 128.0 / 255.0
                            } else {
                                0.0
                            };
                            let combined = sa + ba * (1.0 - sa);
                            let i = (y * w + x) as usize;
                            let bg = base_pixels[i];
                            for c in 0..4 {
                                let expected = if c == 3 {
                                    combined + bg[3] as f64 * (1.0 - combined)
                                } else {
                                    linear([200, 80, 20][c]) * sa
                                        + linear([20, 50, 180][c]) * ba * (1.0 - sa)
                                        + bg[c] as f64 * (1.0 - combined)
                                };
                                assert!((pixels[i][c]as f64-expected).abs()<0.002,"{w}x{h} width{width} box{boxed} alpha{alpha} ({x},{y}) channel{c}: {:?}, expected{expected}",pixels[i]);
                            }
                        }
                    }
                    let padded =
                        read_texture_rgba16f(&gpu, &target, target.width(), target.height());
                    for y in 0..target.height() {
                        for x in 0..target.width() {
                            if x >= w || y >= h {
                                let i = (y * target.width() + x) as usize;
                                assert_eq!(
                                    padded[i], base_padded[i],
                                    "decoration must leave target padding unchanged"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    for width in [f32::NAN, -1.0, 8.1] {
        assert!(CaptionDecoration {
            stroke: Some(([255; 4], width)),
            background: None
        }
        .validate()
        .is_err());
    }
    assert!(CaptionDecoration {
        stroke: None,
        background: Some(([255; 4], f32::INFINITY, 0.0))
    }
    .validate()
    .is_err());
}

#[test]
fn managed_decorated_titles_preserve_transparent_fill_and_match_linear_ap1_compositing() {
    use photonic_render::{caption::CaptionWordRun, caption_decoration::CaptionDecoration};
    use photonic_video::contract::{CaptionCueRun, ResolvedTextBlock};
    let gpu = GpuContext::request_blocking().expect("GPU required for styled native text");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 140000;
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/native_title_reference.json")).unwrap();
    let matrix: [[f64; 3]; 3] = std::array::from_fn(|c| {
        std::array::from_fn(|k| reference["matrix"][c][k].as_f64().unwrap())
    });
    let caption = photonic_render::caption::CaptionCompositor::new(
        gpu.device(),
        gpu.queue(),
        wgpu::TextureFormat::Rgba16Float,
    );
    let base = CaptionCueRun {
        direction: photonic_core::timeline::TextDirection::Auto,
        words: vec![CaptionWordRun {
            metrics: None,
            text: "HI".into(),
            font_family: "sans-serif".into(),
            font_weight: 700,
            color: [255; 4],
        }],
        font_size: 40.0,
        line_height_mul: 1.2,
        anchor: [0.5, 0.2],
        max_width: 0.8,
    };
    let mut make = |op| {
        hash += 2;
        FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![
                IrNode {
                    op,
                    inputs: vec![],
                    content_hash: ContentHash(hash),
                },
                IrNode {
                    op: IrOp::NativeCanvas { w: 190, h: 84 },
                    inputs: vec![(IrNodeId(0), OutPort::default())],
                    content_hash: ContentHash(hash + 1),
                },
            ],
            output: Some(IrNodeId(1)),
        }
    };
    let mask_graph = make(IrOp::NativeTextGen {
        block: ResolvedTextBlock {
            cue: Some(base.clone()),
        },
    });
    for (stroke, boxed, fill_alpha) in [(true, false, 0u8), (false, true, 0), (true, true, 128)] {
        let mut fill = base.clone();
        fill.words[0].color = [255, 0, 0, fill_alpha];
        let decoration = CaptionDecoration {
            stroke: stroke.then_some(([0, 255, 0, 255], 2.0)),
            background: boxed.then_some(([0, 0, 255, 128], 4.0, 6.0)),
        };
        let graph = make(IrOp::NativeDecoratedTextGen {
            block: ResolvedTextBlock { cue: Some(fill) },
            decoration,
        });
        let mut draft = None;
        for (w, h) in [(95u32, 42u32), (190, 84), (95, 42)] {
            let scale = h as f64 / 84.0;
            let mask = evaluator
                .evaluate(&mask_graph, (w, h), &mut NullFrameSource)
                .unwrap();
            let mask = read_texture_rgba16f(&gpu, &mask, w, h);
            let texture = evaluator
                .evaluate(&graph, (w, h), &mut NullFrameSource)
                .expect("styled native title should render");
            let actual = read_texture_rgba16f(&gpu, &texture, w, h);
            let mut layout = base.clone();
            layout.font_size *= scale as f32;
            let bounds = caption
                .layout_bounds_checked(&layout, w, h)
                .unwrap()
                .unwrap()
                .map(f64::from);
            let padding = 4.0 * scale;
            let lo = [bounds[0] - padding, bounds[1] - padding];
            let hi = [bounds[2] + padding, bounds[3] + padding];
            let center = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5];
            let half = [(hi[0] - lo[0]) * 0.5, (hi[1] - lo[1]) * 0.5];
            let radius = (6.0 * scale).min(half[0]).min(half[1]);
            let sample = |x: i32, y: i32| -> f64 {
                if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                    0.0
                } else {
                    mask[(y as u32 * w + x as u32) as usize][3] as f64
                }
            };
            let mut hollow = 0;
            let mut ring = 0;
            for y in 0..h {
                for x in 0..w {
                    let original = sample(x as i32, y as i32);
                    let mut expanded = original;
                    if stroke {
                        let width = 2.0 * scale;
                        let extent = (width + 0.5).ceil() as i32;
                        for dy in -extent..=extent {
                            for dx in -extent..=extent {
                                expanded = expanded.max(
                                    sample(x as i32 + dx, y as i32 + dy)
                                        * (width + 0.5 - ((dx * dx + dy * dy) as f64).sqrt())
                                            .clamp(0.0, 1.0),
                                );
                            }
                        }
                    }
                    let sa = if stroke {
                        (expanded - original).max(0.0)
                    } else {
                        0.0
                    };
                    let q = [
                        (x as f64 + 0.5 - center[0]).abs() - half[0] + radius,
                        (y as f64 + 0.5 - center[1]).abs() - half[1] + radius,
                    ];
                    let d = q[0].max(0.0).hypot(q[1].max(0.0)) + q[0].max(q[1]).min(0.0) - radius;
                    let ba = if boxed {
                        (0.5 - d).clamp(0.0, 1.0) * 128.0 / 255.0
                    } else {
                        0.0
                    };
                    let fa = original * fill_alpha as f64 / 255.0;
                    let linear = [fa, sa * (1.0 - fa), ba * (1.0 - sa) * (1.0 - fa)];
                    let alpha = fa + (sa + ba * (1.0 - sa)) * (1.0 - fa);
                    let i = (y * w + x) as usize;
                    assert!(
                        (actual[i][3] as f64 - alpha).abs() < 0.003,
                        "alpha {:?} at {x},{y}",
                        actual[i]
                    );
                    for c in 0..3 {
                        let expected = (0..3).map(|k| matrix[c][k] * linear[k]).sum::<f64>();
                        assert!(
                            (actual[i][c] as f64 - expected).abs() < 0.004,
                            "stroke{stroke} box{boxed} ({x},{y}) {:?}, expected{expected}",
                            actual[i]
                        );
                    }
                    if stroke && !boxed && fill_alpha == 0 {
                        if original == 1.0 {
                            assert!(actual[i][3] < 0.002);
                            hollow += 1;
                        }
                        if original == 0.0 && actual[i][3] > 0.1 {
                            ring += 1;
                        }
                    }
                }
            }
            if stroke && !boxed && fill_alpha == 0 {
                assert!(hollow > 5 && ring > 10);
            }
            if w == 95 {
                if let Some(previous) = &draft {
                    assert_eq!(previous, &actual);
                } else {
                    draft = Some(actual.clone());
                }
            }
            let padded = read_texture_rgba16f(&gpu, &texture, texture.width(), texture.height());
            for y in 0..texture.height() {
                for x in 0..texture.width() {
                    if x >= w || y >= h {
                        assert_eq!(padded[(y * texture.width() + x) as usize], [0.0; 4]);
                    }
                }
            }
        }
    }
}

#[test]
fn managed_caption_group_fade_matches_independent_premultiplied_over() {
    let gpu = GpuContext::request_blocking().expect("GPU required for managed group fade");
    let pass = photonic_render::caption_decoration::CaptionDecorationPass::new(gpu.device());
    let mut eval = Evaluator::new(gpu.clone());
    let mut hash = 150000;
    let mut solid = |color| {
        hash += 1;
        FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![IrNode {
                op: IrOp::SolidColor { color },
                inputs: vec![],
                content_hash: ContentHash(hash),
            }],
            output: Some(IrNodeId(0)),
        }
    };
    let base = eval
        .evaluate(
            &solid(LinearColor {
                r: 0.1,
                g: 0.2,
                b: 0.3,
                a: 0.5,
            }),
            (3, 1),
            &mut NullFrameSource,
        )
        .unwrap();
    let bg = read_texture_rgba16f(&gpu, &base, 3, 1);
    for alpha in [0.0, 0.25, 1.0] {
        let src = eval
            .evaluate(
                &solid(LinearColor {
                    r: -0.05 * alpha,
                    g: 0.8 * alpha,
                    b: 1.2 * alpha,
                    a: alpha,
                }),
                (3, 1),
                &mut NullFrameSource,
            )
            .unwrap();
        let source = read_texture_rgba16f(&gpu, &src, 3, 1);
        for opacity in [0.0, 0.25, 0.5, 1.0] {
            let out = gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: base.size(),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let mut e = gpu.device().create_command_encoder(&Default::default());
            e.copy_texture_to_texture(base.as_image_copy(), out.as_image_copy(), base.size());
            gpu.queue().submit([e.finish()]);
            pass.composite_group(gpu.device(), gpu.queue(), &src, &out, (3, 1), opacity)
                .unwrap();
            let got = read_texture_rgba16f(&gpu, &out, 3, 1);
            for i in 0..3 {
                for c in 0..4 {
                    let expected = source[i][c] as f64 * opacity as f64
                        + bg[i][c] as f64 * (1.0 - source[i][3] as f64 * opacity as f64);
                    assert!((got[i][c] as f64 - expected).abs() < 0.002);
                }
            }
            for bad in [f32::NAN, -0.1, 1.1] {
                assert!(pass
                    .composite_group(gpu.device(), gpu.queue(), &src, &out, (3, 1), bad)
                    .is_err());
            }
        }
    }
}

#[test]
fn native_polygon_masks_match_geometry_reference_quality_and_padding() {
    let gpu = GpuContext::request_blocking().expect("GPU required for polygon qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let shapes = [
        vec![[0., 0.], [1., 0.], [1., 1.], [0., 1.]],
        vec![
            [0., 0.],
            [1., 0.],
            [1., 0.5],
            [0.5, 0.5],
            [0.5, 1.],
            [0., 1.],
        ],
        vec![[0., 0.], [1., 0.], [0.5, 1.]],
        vec![[0., 0.], [1., 1.], [0., 1.], [1., 0.]],
        vec![[0., 0.], [0., 0.], [1., 0.], [1., 1.], [0., 1.]],
        vec![[0.5, 0.5]; 3],
        (0..128)
            .map(|i| {
                let angle = i as f64 * std::f64::consts::TAU / 128.0;
                [
                    (0.5 + 0.5 * angle.cos()) as f32,
                    (0.5 + 0.5 * angle.sin()) as f32,
                ]
            })
            .collect(),
    ];
    let mut id = 99000;
    for (shape_index, vertices) in shapes.iter().enumerate() {
        for rotation in [0.0, 0.47] {
            for feather in [0.0, 0.2, 1.0] {
                for size in [[0.0, 0.3], [0.4, 0.3]] {
                    id += 1;
                    let graph = FrameGraph {
                        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                        nodes: vec![IrNode {
                            op: IrOp::NativePolygonMask {
                                vertices: vertices.clone(),
                                center: [0.5, 0.5],
                                size,
                                rotation,
                                feather,
                            },
                            inputs: vec![],
                            content_hash: ContentHash(id),
                        }],
                        output: Some(IrNodeId(0)),
                    };
                    assert!(graph.validate_working_color_domain().is_ok());
                    for (w, h) in [(34, 18), (17, 9), (34, 18)] {
                        let reference = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
                        let frame = evaluator
                            .evaluate(&graph, (w, h), &mut NullFrameSource)
                            .unwrap();
                        let pixels = read_texture_rgba16f(&gpu, &frame, w, h);
                        for (index, (cpu, gpu)) in reference.pixels.iter().zip(&pixels).enumerate()
                        {
                            for c in 0..4 {
                                assert!((cpu[c]-gpu[c]).abs()<0.003,"shape{shape_index}, rot{rotation}, feather{feather}, size{size:?}, {w}x{h} pixel{index}: {cpu:?} vs {gpu:?}");
                            }
                            assert!(gpu.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
                            if shape_index == 0 || shape_index == 4 {
                                let rect = photonic_video::graph::ops::native_mask_shape(
                                    w,
                                    h,
                                    true,
                                    [0.5, 0.5],
                                    size,
                                    rotation,
                                    feather,
                                );
                                assert!(
                                    (cpu[3] - rect.pixels[index][3]).abs() < 0.001,
                                    "independent rectangle distance oracle"
                                );
                            }
                            if shape_index == 5 || size[0] == 0.0 {
                                assert_eq!(gpu, &[0.0; 4]);
                            }
                            if shape_index == 1
                                && rotation == 0.0
                                && feather == 0.0
                                && size[0] > 0.0
                            {
                                let x = ((index % w as usize) as f64 + 0.5) / w as f64;
                                let y = ((index / w as usize) as f64 + 0.5) / h as f64;
                                let inside = (0.1..0.9).contains(&x)
                                    && (0.2..0.8).contains(&y)
                                    && (x < 0.5 || y < 0.5);
                                assert_eq!(
                                    cpu[3],
                                    if inside { 1.0 } else { 0.0 },
                                    "concave L geometry"
                                );
                            }
                        }
                        let all = read_texture_rgba16f(&gpu, &frame, frame.width(), frame.height());
                        for y in 0..frame.height() {
                            for x in 0..frame.width() {
                                if x >= w || y >= h {
                                    assert_eq!(
                                        all[(y * frame.width() + x) as usize],
                                        [0.0; 4],
                                        "pool padding"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn native_polygon_invalid_payloads_are_rejected_before_evaluation() {
    for vertices in [
        vec![],
        vec![[0.0, 0.0]; 2],
        vec![[0.5, 0.5]; 129],
        vec![[f32::NAN, 0.0]; 3],
        vec![[1.01, 0.0]; 3],
    ] {
        let graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![IrNode {
                op: IrOp::NativePolygonMask {
                    vertices,
                    center: [0.5; 2],
                    size: [0.5; 2],
                    rotation: 0.0,
                    feather: 0.0,
                },
                inputs: vec![],
                content_hash: ContentHash(99990),
            }],
            output: Some(IrNodeId(0)),
        };
        assert!(graph.validate_working_color_domain().is_err());
    }
}

#[test]
fn native_caption_word_bounds_follow_shaping_wrapping_and_exclude_separators() {
    use photonic_render::caption::{CaptionCompositor, CaptionCueRun, CaptionWordRun};
    let gpu = GpuContext::request_blocking().expect("GPU required for caption layout");
    let compositor =
        CaptionCompositor::new(gpu.device(), gpu.queue(), wgpu::TextureFormat::Rgba16Float);
    let cue = CaptionCueRun {
        direction: photonic_core::timeline::TextDirection::Auto,
        words: ["Wide", "i", "OK"]
            .into_iter()
            .map(|s| CaptionWordRun {
                metrics: None,
                text: s.into(),
                font_family: "sans-serif".into(),
                font_weight: 400,
                color: [0; 4],
            })
            .collect(),
        font_size: 24.0,
        line_height_mul: 1.2,
        anchor: [0.5, 0.1],
        max_width: 0.95,
    };
    let before = cue.clone();
    let spans = compositor.word_bounds_checked(&cue, 300, 150).unwrap();
    assert_eq!(spans.len(), 3);
    for (index, span) in spans.iter().enumerate() {
        assert_eq!(span.word, index);
        let mut single = cue.clone();
        single.words = vec![cue.words[index].clone()];
        let bounds = compositor
            .layout_bounds_checked(&single, 300, 150)
            .unwrap()
            .unwrap();
        assert!(
            ((span.bounds[2] - span.bounds[0]) - (bounds[2] - bounds[0])).abs() < 0.01,
            "separator must not enter word bounds"
        );
    }
    assert!(spans[0].bounds[2] - spans[0].bounds[0] > spans[1].bounds[2] - spans[1].bounds[0]);
    assert!(spans[1].bounds[0] > spans[0].bounds[2]);
    assert_eq!(cue, before);
    let mut wrapped = cue;
    wrapped.max_width = 0.2;
    let rows = compositor.word_bounds_checked(&wrapped, 300, 150).unwrap();
    assert!(
        rows.iter()
            .map(|s| s.bounds[1].to_bits())
            .collect::<std::collections::HashSet<_>>()
            .len()
            > 1
    );
    assert!(rows
        .iter()
        .all(|s| s.bounds.iter().all(|v| v.is_finite()) && s.bounds[2] > s.bounds[0]));
}

#[test]
fn native_caption_underline_pixels_follow_word_geometry_alpha_and_draft_cache() {
    use photonic_render::caption::{CaptionCompositor, CaptionCueRun, CaptionWordRun};
    use photonic_render::caption_decoration::{CaptionPaint, CaptionUnderline};
    use photonic_video::contract::CaptionBatch;
    let gpu = GpuContext::request_blocking().expect("GPU required for underlines");
    let compositor =
        CaptionCompositor::new(gpu.device(), gpu.queue(), wgpu::TextureFormat::Rgba16Float);
    let mut evaluator = Evaluator::new(gpu.clone());
    let cue = CaptionCueRun {
        direction: photonic_core::timeline::TextDirection::Auto,
        words: ["HI", "OK"]
            .into_iter()
            .map(|s| CaptionWordRun {
                metrics: None,
                text: s.into(),
                font_family: "sans-serif".into(),
                font_weight: 400,
                color: [0; 4],
            })
            .collect(),
        font_size: 32.0,
        line_height_mul: 1.2,
        anchor: [0.5, 0.1],
        max_width: 0.9,
    };
    let mut hash = 991000;
    for max_width in [0.9, 0.25] {
        let mut cue = cue.clone();
        cue.max_width = max_width;
        for word in [0, 1] {
            for alpha in [0_u8, 128, 255] {
                for width in [0.0_f32, 1.0, 4.0] {
                    for opacity in [0.0_f32, 0.5, 1.0] {
                        hash += 10;
                        let graph = FrameGraph {
                            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                            nodes: vec![
                                IrNode {
                                    op: IrOp::SolidColor {
                                        color: LinearColor {
                                            r: 0.,
                                            g: 0.,
                                            b: 0.,
                                            a: 0.,
                                        },
                                    },
                                    inputs: vec![],
                                    content_hash: ContentHash(hash),
                                },
                                IrNode {
                                    op: IrOp::NativeCanvas { w: 200, h: 100 },
                                    inputs: vec![(IrNodeId(0), OutPort::default())],
                                    content_hash: ContentHash(hash + 1),
                                },
                                IrNode {
                                    op: IrOp::NativeSdrOutput,
                                    inputs: vec![(IrNodeId(1), OutPort::default())],
                                    content_hash: ContentHash(hash + 2),
                                },
                                IrNode {
                                    op: IrOp::NativeCaptionOutput {
                                        cue_batch: CaptionBatch {
                                            cues: vec![cue.clone()],
                                        },
                                        video: false,
                                        paints: vec![CaptionPaint {
                                            opacity,
                                            underlines: vec![CaptionUnderline {
                                                word,
                                                color: [255, 0, 0, alpha],
                                                width,
                                            }],
                                            ..Default::default()
                                        }],
                                    },
                                    inputs: vec![(IrNodeId(2), OutPort::default())],
                                    content_hash: ContentHash(hash + 3),
                                },
                            ],
                            output: Some(IrNodeId(3)),
                        };
                        assert!(graph.validate_working_color_domain().is_ok());
                        for (w, h) in [(200, 100), (100, 50), (200, 100)] {
                            let scale = w as f32 / 200.0;
                            let mut scaled = cue.clone();
                            scaled.font_size *= scale;
                            let spans: Vec<_> = compositor
                                .word_bounds_checked(&scaled, w, h)
                                .unwrap()
                                .into_iter()
                                .filter(|s| s.word == word)
                                .collect();
                            assert!(!spans.is_empty());
                            let texture = evaluator
                                .evaluate(&graph, (w, h), &mut NullFrameSource)
                                .unwrap();
                            let pixels = read_texture_rgba16f(&gpu, &texture, w, h);
                            let mut covered = 0;
                            for (index, p) in pixels.iter().enumerate() {
                                let x = (index % w as usize) as f64 + 0.5;
                                let y = (index / w as usize) as f64 + 0.5;
                                let mut coverage = 0.0_f64;
                                for span in &spans {
                                    let [left, _, right, bottom] = span.bounds.map(f64::from);
                                    let top = bottom - f64::from(width * scale);
                                    let qx =
                                        (x - (left + right) * 0.5).abs() - (right - left) * 0.5;
                                    let qy =
                                        (y - (top + bottom) * 0.5).abs() - (bottom - top) * 0.5;
                                    let distance =
                                        qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0);
                                    let a = if width == 0.0 {
                                        0.0
                                    } else {
                                        (0.5 - distance).clamp(0.0, 1.0) * f64::from(alpha) / 255.0
                                    };
                                    coverage = a + coverage * (1.0 - a);
                                }
                                let a = coverage * f64::from(opacity);
                                assert!((f64::from(p[3])-a).abs()<0.003,"word{word} alpha{alpha} width{width} {w}x{h} ({x},{y}) {p:?} expected alpha{a}");
                                assert!(
                                    (p[0] - p[3]).abs() < 0.003
                                        && p[1].abs() < 0.001
                                        && p[2].abs() < 0.001
                                );
                                if p[3] > 0.01 {
                                    covered += 1;
                                }
                            }
                            if width > 0.0 && alpha > 0 && opacity > 0.0 {
                                assert!(covered > 4, "underline must render");
                            } else {
                                assert_eq!(covered, 0, "disabled paint must be transparent");
                            }
                        }
                    }
                }
            }
        }
    }
    let invalid = CaptionPaint {
        underlines: vec![CaptionUnderline {
            word: 2,
            color: [255; 4],
            width: 1.0,
        }],
        ..Default::default()
    };
    assert!(invalid.validate(2).is_err());
}

#[test]
fn native_caption_underline_paint_refuses_invalid_widths_indexes_and_duplicates() {
    use photonic_render::caption_decoration::{CaptionPaint, CaptionUnderline};
    for underlines in [
        vec![CaptionUnderline {
            word: 0,
            color: [255; 4],
            width: f32::NAN,
        }],
        vec![CaptionUnderline {
            word: 0,
            color: [255; 4],
            width: -1.0,
        }],
        vec![CaptionUnderline {
            word: 0,
            color: [255; 4],
            width: 8.01,
        }],
        vec![CaptionUnderline {
            word: 2,
            color: [255; 4],
            width: 2.0,
        }],
        vec![
            CaptionUnderline {
                word: 0,
                color: [255; 4],
                width: 2.0
            };
            2
        ],
    ] {
        assert!(CaptionPaint {
            underlines,
            ..Default::default()
        }
        .validate(2)
        .is_err());
    }
}

#[test]
fn explicit_caption_direction_keeps_the_authored_anchor_and_word_geometry() {
    use photonic_core::timeline::TextDirection;
    use photonic_render::caption::{CaptionCompositor, CaptionCueRun, CaptionWordRun};
    let gpu = GpuContext::request_blocking().expect("GPU required for direction layout");
    let compositor =
        CaptionCompositor::new(gpu.device(), gpu.queue(), wgpu::TextureFormat::Rgba16Float);
    for direction in [TextDirection::Ltr, TextDirection::Rtl] {
        let cue = CaptionCueRun {
            direction,
            words: ["Brand", "שלום"]
                .into_iter()
                .map(|text| CaptionWordRun {
                    metrics: None,
                    text: text.into(),
                    font_family: "sans-serif".into(),
                    font_weight: 400,
                    color: [255; 4],
                })
                .collect(),
            font_size: 24.0,
            line_height_mul: 1.2,
            anchor: [0.5, 0.2],
            max_width: 0.9,
        };
        let spans = compositor.word_bounds_checked(&cue, 320, 100).unwrap();
        assert_eq!(spans.len(), 2);
        let left = spans
            .iter()
            .map(|s| s.bounds[0])
            .fold(f32::INFINITY, f32::min);
        let right = spans
            .iter()
            .map(|s| s.bounds[2])
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (0.0..=320.0).contains(&left) && (0.0..=320.0).contains(&right),
            "{direction:?}: {left}..{right}"
        );
        assert!(
            ((left + right) * 0.5 - 160.0).abs() < 1.0,
            "{direction:?}: anchor moved to {}",
            (left + right) * 0.5
        );
    }
}

#[test]
fn explicit_caption_direction_changes_visual_order_without_changing_auto_pixels() {
    use photonic_core::timeline::TextDirection;
    use photonic_render::caption::{CaptionCompositor, CaptionCueRun, CaptionWordRun};
    let gpu = GpuContext::request_blocking().expect("GPU required for direction render");
    let compositor =
        CaptionCompositor::new(gpu.device(), gpu.queue(), wgpu::TextureFormat::Rgba16Float);
    let target = gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("direction caption"),
        size: wgpu::Extent3d {
            width: 320,
            height: 100,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let mut baseline = None;
    for direction in [
        TextDirection::Auto,
        TextDirection::Ltr,
        TextDirection::Rtl,
        TextDirection::Auto,
    ] {
        gpu.queue().write_texture(
            target.as_image_copy(),
            &vec![0; 320 * 100 * 8],
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(320 * 8),
                rows_per_image: Some(100),
            },
            target.size(),
        );
        let cue = CaptionCueRun {
            direction,
            words: vec![
                CaptionWordRun {
                    metrics: None,
                    text: "Brand".into(),
                    font_family: "sans-serif".into(),
                    font_weight: 400,
                    color: [255, 0, 0, 255],
                },
                CaptionWordRun {
                    metrics: None,
                    text: "שלום".into(),
                    font_family: "sans-serif".into(),
                    font_weight: 400,
                    color: [0, 0, 255, 255],
                },
            ],
            font_size: 24.0,
            line_height_mul: 1.2,
            anchor: [0.5, 0.2],
            max_width: 0.9,
        };
        compositor
            .composite_checked(gpu.device(), gpu.queue(), &target, &[cue], 320, 100)
            .unwrap();
        let pixels = read_texture_rgba16f(&gpu, &target, 320, 100);
        let centroid = |channel: usize| {
            let mut count = 0;
            let mut sum = 0.;
            for (i, p) in pixels.iter().enumerate() {
                if p[channel] > 0.2 {
                    count += 1;
                    sum += (i % 320) as f64;
                }
            }
            assert!(count > 25);
            sum / count as f64
        };
        let red = centroid(0);
        let blue = centroid(2);
        if direction == TextDirection::Rtl {
            assert!(
                red > blue,
                "RTL should place the Latin brand to the right: {red} {blue}"
            );
        } else {
            assert!(red < blue);
            if let Some(ref baseline) = baseline {
                assert_eq!(
                    &pixels, baseline,
                    "Auto/LTR preserves the existing Latin-first raster"
                );
            } else {
                baseline = Some(pixels.clone());
            }
        }
    }
}

#[test]
fn spatial_caption_sweep_matches_independent_linear_alpha_reference() {
    use photonic_render::caption_decoration::{CaptionDecorationPass, CaptionSweep};
    let gpu = GpuContext::request_blocking().expect("GPU required for spatial sweep");
    let pass = CaptionDecorationPass::new(gpu.device());
    let mut eval = Evaluator::new(gpu.clone());
    let mut hash = 900_000;
    let linear = |v: u8| {
        let c = v as f64 / 255.;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    for coverage in [0., 0.25, 1.] {
        hash += 1;
        let g = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: coverage,
                        g: coverage,
                        b: coverage,
                        a: coverage,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(hash),
            }],
            output: Some(IrNodeId(0)),
        };
        let mask = eval.evaluate(&g, (24, 12), &mut NullFrameSource).unwrap();
        let target = gpu.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("sweep reference"),
            size: mask.size(),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for spans in [
            vec![[4., 1., 20., 9.]],
            vec![[2., 0., 10., 4.], [5., 6., 21., 12.]],
        ] {
            for fraction in [0., 0.01, 0.25, 0.5, 0.75, 0.99, 1.] {
                for alpha in [0, 64, 255] {
                    gpu.queue().write_texture(
                        target.as_image_copy(),
                        &vec![0; (target.width() * target.height() * 8) as usize],
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(target.width() * 8),
                            rows_per_image: Some(target.height()),
                        },
                        target.size(),
                    );
                    let sweep = CaptionSweep {
                        word: 0,
                        fraction,
                        active: [220, 50, 10, alpha],
                        inactive: [15, 100, 240, 128],
                    };
                    pass.composite_sweep(
                        gpu.device(),
                        gpu.queue(),
                        &mask,
                        &target,
                        (24, 12),
                        &spans,
                        &sweep,
                    )
                    .unwrap();
                    let pixels = read_texture_rgba16f(&gpu, &target, 24, 12);
                    for y in 0..12 {
                        for x in 0..24 {
                            let row = if spans.len() == 2 && y >= 6 { 1 } else { 0 };
                            let total: f64 = spans.iter().map(|b| (b[2] - b[0]) as f64).sum();
                            let spent: f64 =
                                spans[..row].iter().map(|b| (b[2] - b[0]) as f64).sum();
                            let remaining = total * fraction as f64 - spent;
                            let span = spans[row];
                            let width = (span[2] - span[0]) as f64;
                            let weight = if remaining <= 0. {
                                0.
                            } else if remaining >= width {
                                1.
                            } else {
                                (span[0] as f64 + remaining - x as f64).clamp(0., 1.)
                            };
                            let a = alpha as f64 / 255. * weight;
                            let b = 128. / 255. * (1. - weight);
                            let mut expected = [0.; 4];
                            for c in 0..3 {
                                expected[c] = (linear(sweep.active[c]) * a
                                    + linear(sweep.inactive[c]) * b)
                                    * coverage as f64;
                            }
                            expected[3] = (a + b) * coverage as f64;
                            for (actual, want) in pixels[(y * 24 + x) as usize].iter().zip(expected)
                            {
                                assert!((*actual as f64-want).abs()<0.0015,"xy{x}/{y} fraction{fraction} spans{spans:?} actual{actual} expected{want}");
                            }
                        }
                    }
                }
            }
        }
        for fraction in [f32::NAN, -0.01, 1.01] {
            assert!(pass
                .composite_sweep(
                    gpu.device(),
                    gpu.queue(),
                    &mask,
                    &target,
                    (24, 12),
                    &[[0., 0., 8., 8.]],
                    &CaptionSweep {
                        word: 0,
                        fraction,
                        active: [255; 4],
                        inactive: [0; 4]
                    }
                )
                .is_err());
        }
    }
}

#[test]
fn caption_sweep_payload_refuses_invalid_indexes_duplicates_and_fractions() {
    use photonic_render::caption_decoration::{CaptionPaint, CaptionSweep};
    let good = CaptionSweep {
        word: 0,
        fraction: 0.5,
        active: [255; 4],
        inactive: [0; 4],
    };
    assert!(CaptionPaint {
        sweeps: vec![good.clone()],
        ..Default::default()
    }
    .validate(1)
    .is_ok());
    for sweeps in [
        vec![CaptionSweep {
            word: 1,
            ..good.clone()
        }],
        vec![CaptionSweep {
            fraction: f32::NAN,
            ..good.clone()
        }],
        vec![CaptionSweep {
            fraction: 1.01,
            ..good.clone()
        }],
        vec![good.clone(), good],
    ] {
        assert!(CaptionPaint {
            sweeps,
            ..Default::default()
        }
        .validate(1)
        .is_err());
    }
}

#[test]
fn spatial_caption_sweep_preserves_actual_glyph_alpha_wrapping_draft_and_group_opacity() {
    use photonic_core::timeline::TextDirection;
    use photonic_render::{
        caption::{CaptionCompositor, CaptionCueRun, CaptionWordRun},
        caption_decoration::{CaptionPaint, CaptionSweep},
    };
    use photonic_video::contract::CaptionBatch;
    let gpu = GpuContext::request_blocking().expect("GPU required for sweep glyphs");
    let compositor =
        CaptionCompositor::new(gpu.device(), gpu.queue(), wgpu::TextureFormat::Rgba16Float);
    let mut eval = Evaluator::new(gpu.clone());
    let mut hash = 940_000;
    let encode = |v: f64| {
        if v <= 0.0031308 {
            12.92 * v
        } else {
            1.055 * v.powf(1. / 2.4) - 0.055
        }
    };
    for text in ["MMMM", "MMMMMMMMMMMMMMMM"] {
        let cue = CaptionCueRun {
            direction: TextDirection::Ltr,
            words: vec![CaptionWordRun {
                metrics: None,
                text: text.into(),
                font_family: "sans-serif".into(),
                font_weight: 400,
                color: [255; 4],
            }],
            font_size: 28.,
            line_height_mul: 1.2,
            anchor: [0.5, 0.05],
            max_width: 0.5,
        };
        for fraction in [0., 0.25, 0.5, 0.75, 1.] {
            for opacity in [0.25, 1.] {
                let make = |paint: Option<CaptionPaint>, hash: u128| FrameGraph {
                    working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                    nodes: vec![
                        IrNode {
                            op: IrOp::SolidColor {
                                color: LinearColor {
                                    r: 0.,
                                    g: 0.,
                                    b: 0.,
                                    a: 0.,
                                },
                            },
                            inputs: vec![],
                            content_hash: ContentHash(hash),
                        },
                        IrNode {
                            op: IrOp::NativeCanvas { w: 320, h: 180 },
                            inputs: vec![(IrNodeId(0), OutPort::default())],
                            content_hash: ContentHash(hash + 1),
                        },
                        IrNode {
                            op: IrOp::NativeSdrOutput,
                            inputs: vec![(IrNodeId(1), OutPort::default())],
                            content_hash: ContentHash(hash + 2),
                        },
                        IrNode {
                            op: IrOp::NativeCaptionOutput {
                                cue_batch: CaptionBatch {
                                    cues: vec![cue.clone()],
                                },
                                video: false,
                                paints: paint.into_iter().collect(),
                            },
                            inputs: vec![(IrNodeId(2), OutPort::default())],
                            content_hash: ContentHash(hash + 3),
                        },
                    ],
                    output: Some(IrNodeId(3)),
                };
                hash += 10;
                let baseline = make(None, hash);
                hash += 10;
                let swept = make(
                    Some(CaptionPaint {
                        opacity,
                        sweeps: vec![CaptionSweep {
                            word: 0,
                            fraction,
                            active: [255, 0, 0, 128],
                            inactive: [0, 0, 255, 255],
                        }],
                        ..Default::default()
                    }),
                    hash,
                );
                for (w, h) in [(320, 180), (160, 90), (320, 180)] {
                    let base = eval
                        .evaluate(&baseline, (w, h), &mut NullFrameSource)
                        .unwrap();
                    let coverage = read_texture_rgba16f(&gpu, &base, w, h);
                    let out = eval.evaluate(&swept, (w, h), &mut NullFrameSource).unwrap();
                    let actual = read_texture_rgba16f(&gpu, &out, w, h);
                    let mut scaled = cue.clone();
                    scaled.font_size *= w as f32 / 320.;
                    let spans = compositor.word_bounds_checked(&scaled, w, h).unwrap();
                    if text.len() > 4 {
                        assert!(spans.len() > 1, "fixture actually wraps");
                    }
                    let total: f64 = spans
                        .iter()
                        .map(|s| (s.bounds[2] - s.bounds[0]) as f64)
                        .sum();
                    let mut ink = 0;
                    for (index, p) in actual.iter().enumerate() {
                        let x = (index % w as usize) as f64;
                        let y = (index / w as usize) as f64;
                        let row = (0..spans.len())
                            .find(|i| {
                                *i + 1 == spans.len()
                                    || y < ((spans[*i].bounds[1]
                                        + spans[*i].bounds[3]
                                        + spans[*i + 1].bounds[1]
                                        + spans[*i + 1].bounds[3])
                                        * 0.25)
                                        .ceil() as f64
                            })
                            .unwrap();
                        let spent: f64 = spans[..row]
                            .iter()
                            .map(|s| (s.bounds[2] - s.bounds[0]) as f64)
                            .sum();
                        let b = spans[row].bounds;
                        let remaining = total * fraction as f64 - spent;
                        let weight = if remaining <= 0. {
                            0.
                        } else if remaining >= (b[2] - b[0]) as f64 {
                            1.
                        } else {
                            (b[0] as f64 + remaining - x).clamp(0., 1.)
                        };
                        let a = weight * 128. / 255.;
                        let b = 1. - weight;
                        let sum = a + b;
                        let alpha = coverage[index][3] as f64 * sum * opacity as f64;
                        let expected =
                            [encode(a / sum) * alpha, 0., encode(b / sum) * alpha, alpha];
                        for (v, want) in p.iter().zip(expected) {
                            assert!((*v as f64-want).abs()<0.004,"text{text} size{w} fraction{fraction} xy{x}/{y} got{p:?} expected{expected:?}");
                        }
                        if p[3] > 0.01 {
                            ink += 1;
                        }
                    }
                    assert!(ink > 15);
                }
            }
        }
    }
}

#[test]
fn native_channels_preserve_signed_hdr_rgb_alpha_and_independent_luminance() {
    use photonic_video::graph::ir::Channel;
    let gpu = GpuContext::request_blocking().expect("GPU required for native channels");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 960_000;
    for rgb in [
        [-0.5, 0.25, 3.],
        [2., -0.125, 0.001],
        [0., 0., 0.],
        [1., 1., 1.],
    ] {
        for alpha in [0., 0.0005, 0.125, 0.37, 0.5, 1.] {
            for selected in [
                Some(Channel::R),
                Some(Channel::G),
                Some(Channel::B),
                Some(Channel::A),
                None,
            ] {
                hash += 10;
                let pixel = [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha];
                let g = FrameGraph {
                    working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                    nodes: vec![
                        IrNode {
                            op: IrOp::SolidColor {
                                color: LinearColor {
                                    r: pixel[0],
                                    g: pixel[1],
                                    b: pixel[2],
                                    a: pixel[3],
                                },
                            },
                            inputs: vec![],
                            content_hash: ContentHash(hash),
                        },
                        IrNode {
                            op: selected.map_or(IrOp::NativeImageToMask, |channel| {
                                IrOp::NativeChannelExtract { channel }
                            }),
                            inputs: vec![(IrNodeId(0), OutPort::default())],
                            content_hash: ContentHash(hash + 1),
                        },
                        IrNode {
                            op: IrOp::NativeMaskToImage,
                            inputs: vec![(IrNodeId(1), OutPort::default())],
                            content_hash: ContentHash(hash + 2),
                        },
                    ],
                    output: Some(IrNodeId(2)),
                };
                assert!(g.validate_working_color_domain().is_ok());
                let expected = match selected {
                    Some(Channel::A) => alpha as f64,
                    Some(c) => {
                        if alpha == 0. {
                            0.
                        } else {
                            rgb[c as usize] as f64
                        }
                    }
                    None => {
                        if alpha == 0. {
                            0.
                        } else {
                            rgb[0] as f64 * 0.2722287168
                                + rgb[1] as f64 * 0.6740817658
                                + rgb[2] as f64 * 0.0536895174
                        }
                    }
                };
                let cpu = eval_cpu::evaluate(&g, (3, 2), &mut EmptyProvider);
                assert!((cpu.pixels[0][0] as f64 - expected).abs() < 1e-6);
                assert_eq!(cpu.pixels[0][3], 1.);
                for (w, h) in [(3, 2), (1, 1), (3, 2)] {
                    let tex = evaluator
                        .evaluate(&g, (w, h), &mut NullFrameSource)
                        .unwrap();
                    for p in read_texture_rgba16f(&gpu, &tex, w, h) {
                        for c in 0..3 {
                            assert!(
                                (p[c] as f64 - expected).abs() < 0.003,
                                "{selected:?} {rgb:?} alpha{alpha} got{p:?} expected{expected}"
                            );
                        }
                        assert_eq!(p[3], 1.);
                    }
                }
                let mut raw = g.clone();
                raw.output = Some(IrNodeId(1));
                assert!(
                    raw.validate_working_color_domain().is_err(),
                    "raw scalar cannot be presented"
                );
            }
            hash += 20;
            let mut nodes = vec![IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: rgb[0] * alpha,
                        g: rgb[1] * alpha,
                        b: rgb[2] * alpha,
                        a: alpha,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(hash),
            }];
            for (i, channel) in [Channel::R, Channel::G, Channel::B, Channel::A]
                .into_iter()
                .enumerate()
            {
                nodes.push(IrNode {
                    op: IrOp::NativeChannelExtract { channel },
                    inputs: vec![(IrNodeId(0), OutPort::default())],
                    content_hash: ContentHash(hash + 1 + i as u128),
                });
            }
            nodes.push(IrNode {
                op: IrOp::NativeChannelCombine,
                inputs: (1..5)
                    .map(|id| (IrNodeId(id), OutPort::default()))
                    .collect(),
                content_hash: ContentHash(hash + 5),
            });
            let g = FrameGraph {
                working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                nodes,
                output: Some(IrNodeId(5)),
            };
            assert!(g.validate_working_color_domain().is_ok());
            let out = evaluator
                .evaluate(&g, (3, 2), &mut NullFrameSource)
                .unwrap();
            for p in read_texture_rgba16f(&gpu, &out, 3, 2) {
                for (got, want) in
                    p.iter()
                        .zip([rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha])
                {
                    assert!((got - want).abs() < 0.003);
                }
            }
        }
    }
}

#[test]
fn native_channel_combine_clamps_only_alpha_and_rejects_domain_misuse() {
    use photonic_video::graph::ir::{Channel, FrameColorEncoding};
    let gpu = GpuContext::request_blocking().expect("GPU required for scalar alpha");
    let mut eval = Evaluator::new(gpu.clone());
    for (case, alpha) in [-0.5, 1.5].into_iter().enumerate() {
        let values = [-0.25, 2.5, 0.75, alpha];
        let mut nodes = Vec::new();
        for (i, value) in values.into_iter().enumerate() {
            nodes.push(IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: value,
                        g: 0.,
                        b: 0.,
                        a: 1.,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(970_000 + case as u128 * 100 + i as u128 * 10),
            });
            nodes.push(IrNode {
                op: IrOp::NativeChannelExtract {
                    channel: Channel::R,
                },
                inputs: vec![(IrNodeId(i as u32 * 2), OutPort::default())],
                content_hash: ContentHash(970_001 + case as u128 * 100 + i as u128 * 10),
            });
        }
        nodes.push(IrNode {
            op: IrOp::NativeChannelCombine,
            inputs: [1, 3, 5, 7]
                .map(|id| (IrNodeId(id), OutPort::default()))
                .to_vec(),
            content_hash: ContentHash(970_090 + case as u128 * 100),
        });
        let graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes,
            output: Some(IrNodeId(8)),
        };
        assert_eq!(
            graph.node_color_encoding(IrNodeId(1)),
            Ok(FrameColorEncoding::ScalarChannel)
        );
        let alpha = alpha.clamp(0., 1.);
        let want = [-0.25 * alpha, 2.5 * alpha, 0.75 * alpha, alpha];
        let texture = eval.evaluate(&graph, (2, 1), &mut NullFrameSource).unwrap();
        for p in read_texture_rgba16f(&gpu, &texture, 2, 1) {
            assert_eq!(p, want);
        }
        let mut bad = graph.clone();
        bad.nodes[8].inputs.pop();
        assert!(bad.validate_working_color_domain().is_err());
        let mut bad = graph.clone();
        bad.nodes[8].inputs[0].0 = IrNodeId(0);
        assert!(bad.validate_working_color_domain().is_err());
        let mut bad = graph.clone();
        bad.nodes[8].op = IrOp::NativeInvert;
        assert!(bad.validate_working_color_domain().is_err());
        let mut bad = graph.clone();
        bad.output = Some(IrNodeId(1));
        assert!(bad.validate_working_color_domain().is_err());
        let mut bad = graph.clone();
        bad.working_color_domain = WorkingColorDomain::LegacyLinearRec709;
        assert!(bad.validate_working_color_domain().is_err());
    }
}

#[test]
fn native_channel_combine_reads_independent_spatial_signals_at_each_pixel() {
    use photonic_video::graph::ir::Channel;
    let gpu = GpuContext::request_blocking().expect("GPU required for spatial channel routing");
    let mut eval = Evaluator::new(gpu.clone());
    let mut nodes = Vec::new();
    for (i, (rect, center, size)) in [
        (true, [0.25, 0.5], [0.2, 0.4]),
        (false, [0.5, 0.5], [0.3, 0.3]),
        (true, [0.75, 0.5], [0.2, 0.4]),
    ]
    .into_iter()
    .enumerate()
    {
        nodes.push(IrNode {
            op: IrOp::NativeMaskShape {
                rect,
                center,
                size,
                rotation: 0.,
                feather: 0.2,
            },
            inputs: vec![],
            content_hash: ContentHash(980_000 + i as u128 * 10),
        });
        nodes.push(IrNode {
            op: IrOp::NativeChannelExtract {
                channel: Channel::A,
            },
            inputs: vec![(IrNodeId(i as u32 * 2), OutPort::default())],
            content_hash: ContentHash(980_001 + i as u128 * 10),
        });
    }
    nodes.push(IrNode {
        op: IrOp::SolidColor {
            color: LinearColor {
                r: 0.,
                g: 0.,
                b: 0.,
                a: 0.5,
            },
        },
        inputs: vec![],
        content_hash: ContentHash(980_040),
    });
    nodes.push(IrNode {
        op: IrOp::NativeChannelExtract {
            channel: Channel::A,
        },
        inputs: vec![(IrNodeId(6), OutPort::default())],
        content_hash: ContentHash(980_041),
    });
    nodes.push(IrNode {
        op: IrOp::NativeChannelCombine,
        inputs: [1, 3, 5, 7]
            .map(|id| (IrNodeId(id), OutPort::default()))
            .to_vec(),
        content_hash: ContentHash(980_050),
    });
    let g = FrameGraph {
        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
        nodes,
        output: Some(IrNodeId(8)),
    };
    for (w, h) in [(32, 20), (16, 10), (32, 20)] {
        let expected = eval_cpu::evaluate(&g, (w, h), &mut EmptyProvider);
        let texture = eval.evaluate(&g, (w, h), &mut NullFrameSource).unwrap();
        let actual = read_texture_rgba16f(&gpu, &texture, w, h);
        assert!(actual.iter().any(|p| p[0] > 0.4 && p[2] < 0.01));
        assert!(actual.iter().any(|p| p[2] > 0.4 && p[0] < 0.01));
        for (pixel, want) in actual.iter().zip(expected.pixels) {
            for (v, w) in pixel.iter().zip(want) {
                assert!((v - w).abs() < 0.002, "{pixel:?} vs{want:?}");
            }
        }
    }
}

#[test]
fn native_channel_signal_storage_stays_finite_when_unpremultiplication_exceeds_fp16() {
    use photonic_video::graph::ir::Channel;
    let gpu = GpuContext::request_blocking().expect("GPU required for channel storage");
    let mut eval = Evaluator::new(gpu.clone());
    for (i, r) in [-65504., 65504.].into_iter().enumerate() {
        let hash = 990_000 + i as u128 * 10;
        let graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![
                IrNode {
                    op: IrOp::SolidColor {
                        color: LinearColor {
                            r,
                            g: 0.,
                            b: 0.,
                            a: 0.5,
                        },
                    },
                    inputs: vec![],
                    content_hash: ContentHash(hash),
                },
                IrNode {
                    op: IrOp::NativeChannelExtract {
                        channel: Channel::R,
                    },
                    inputs: vec![(IrNodeId(0), OutPort::default())],
                    content_hash: ContentHash(hash + 1),
                },
                IrNode {
                    op: IrOp::NativeMaskToImage,
                    inputs: vec![(IrNodeId(1), OutPort::default())],
                    content_hash: ContentHash(hash + 2),
                },
            ],
            output: Some(IrNodeId(2)),
        };
        let cpu = eval_cpu::evaluate(&graph, (2, 1), &mut EmptyProvider);
        assert_eq!(cpu.pixels[0], [r, r, r, 1.]);
        let tex = eval.evaluate(&graph, (2, 1), &mut NullFrameSource).unwrap();
        for p in read_texture_rgba16f(&gpu, &tex, 2, 1) {
            assert_eq!(p, [r, r, r, 1.]);
        }
    }
}

#[test]
fn native_media_sources_validate_geometry_before_quality_resampling_and_cached_results() {
    use photonic_core::timeline::color::{
        InputMatrix, InputSignalRange, NativeInputColorInterpretation, NativeInputStandard,
    };
    use photonic_core::timeline::{AssetId, Tick, VectorRef};
    use photonic_video::{
        contract::VectorStateKey,
        graph::eval::{GpuFrame, GpuFrameSource},
    };
    struct Source {
        frame: GpuFrame,
        errors: Vec<String>,
        times: Vec<Tick>,
        still_requests: Vec<(u32, u32)>,
    }
    impl GpuFrameSource for Source {
        fn video_texture(
            &mut self,
            _: &GpuContext,
            _: AssetId,
            _: Tick,
            _: bool,
        ) -> Option<GpuFrame> {
            panic!("native MediaIn cannot use Legacy decoding")
        }
        fn native_video_texture(
            &mut self,
            _: &GpuContext,
            _: AssetId,
            time: Tick,
            _: &NativeInputColorInterpretation,
        ) -> Option<GpuFrame> {
            self.times.push(time);
            Some(self.frame.clone())
        }
        fn still_texture(
            &mut self,
            _: &GpuContext,
            _: AssetId,
            _: u32,
            _: u32,
        ) -> Option<GpuFrame> {
            panic!("native MediaIn cannot use Legacy stills")
        }
        fn native_still_texture(
            &mut self,
            _: &GpuContext,
            _: AssetId,
            w: u32,
            h: u32,
        ) -> Option<GpuFrame> {
            self.still_requests.push((w, h));
            Some(self.frame.clone())
        }
        fn report_native_source_error(&mut self, _: AssetId, error: String) {
            self.errors.push(error);
        }
        fn vector_texture(
            &mut self,
            _: &GpuContext,
            _: VectorRef,
            _: VectorStateKey,
            _: u32,
            _: u32,
        ) -> Option<GpuFrame> {
            None
        }
    }
    let gpu = GpuContext::request_blocking().expect("GPU required for graph media");
    let fixture = FrameGraph {
        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
        nodes: vec![IrNode {
            op: IrOp::SolidColor {
                color: LinearColor {
                    r: 0.1,
                    g: 0.2,
                    b: 0.3,
                    a: 0.5,
                },
            },
            inputs: vec![],
            content_hash: ContentHash(1_000_000),
        }],
        output: Some(IrNodeId(0)),
    };
    let texture = Evaluator::new(gpu.clone())
        .evaluate(&fixture, (4, 2), &mut NullFrameSource)
        .unwrap();
    let mut source = Source {
        frame: GpuFrame::new(texture, 4, 2),
        errors: vec![],
        times: vec![],
        still_requests: vec![],
    };
    let input = NativeInputColorInterpretation {
        version: 1,
        standard: NativeInputStandard::Bt709Scene,
        range: InputSignalRange::Limited,
        matrix: InputMatrix::Bt709,
        chroma_location: None,
        hlg_peak_nits: None,
        reference_white_nits: None,
    };
    let asset = AssetId::new();
    let mut eval = Evaluator::new(gpu.clone());
    for (index, op) in [
        IrOp::NativeMediaVideo {
            asset,
            src_time: Tick(1234),
            input: input.clone(),
            w: 4,
            h: 2,
            native_extent: false,
        },
        IrOp::NativeMediaStill {
            asset,
            w: 4,
            h: 2,
            native_extent: false,
        },
    ]
    .into_iter()
    .enumerate()
    {
        let graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![IrNode {
                op,
                inputs: vec![],
                content_hash: ContentHash(1_000_010 + index as u128),
            }],
            output: Some(IrNodeId(0)),
        };
        assert!(graph.validate_working_color_domain().is_ok());
        for (w, h) in [(4, 2), (2, 1), (4, 2)] {
            let out = eval.evaluate(&graph, (w, h), &mut source).unwrap();
            let expected = if index == 0 {
                [0.1, 0.2, 0.3, 0.5]
            } else {
                [
                    0.6130974024 * 0.1 + 0.3395231462 * 0.2 + 0.0473794514 * 0.3,
                    0.0701937225 * 0.1 + 0.9163538791 * 0.2 + 0.0134523985 * 0.3,
                    0.0206155929 * 0.1 + 0.1095697729 * 0.2 + 0.8698146342 * 0.3,
                    0.5,
                ]
            };
            for p in read_texture_rgba16f(&gpu, &out, w, h) {
                for (v, want) in p.iter().zip(expected) {
                    assert!((*v as f64 - want).abs() < 0.001, "{p:?} vs{expected:?}");
                }
            }
        }
        // Even with a previously cached complete result, bad source dimensions
        // may not normalize silently into a plausible picture.
        source.frame.width = 8;
        assert!(eval.evaluate(&graph, (2, 1), &mut source).is_none());
        assert!(source.errors.last().unwrap().contains("geometry changed"));
        source.frame.width = 4;
        assert!(eval.evaluate(&graph, (4, 2), &mut source).is_some());
        let mut bad = graph.clone();
        bad.working_color_domain = WorkingColorDomain::LegacyLinearRec709;
        assert!(bad.validate_working_color_domain().is_err());
    }
    assert!(source.times.iter().all(|t| *t == Tick(1234)));
    assert!(
        source.still_requests.iter().all(|d| *d == (4, 2)),
        "source geometry request is independent of Draft target"
    );
}

#[test]
fn native_decode_planning_prunes_unused_sources_but_keeps_requested_scope_taps() {
    use photonic_core::timeline::color::{
        InputMatrix, InputSignalRange, NativeInputColorInterpretation, NativeInputStandard,
    };
    use photonic_core::timeline::{AssetId, Tick, VectorRef};
    use photonic_video::contract::VectorStateKey;
    use photonic_video::graph::eval::{GpuFrame, GpuFrameSource};
    struct Source {
        planned: Vec<(AssetId, Tick)>,
        calls: usize,
        reject: bool,
    }
    impl GpuFrameSource for Source {
        fn prepare_native_video_sources(&mut self, r: &[(AssetId, Tick)]) -> bool {
            self.planned = r.to_vec();
            !self.reject
        }
        fn native_video_texture(
            &mut self,
            _: &GpuContext,
            _: AssetId,
            _: Tick,
            _: &NativeInputColorInterpretation,
        ) -> Option<GpuFrame> {
            self.calls += 1;
            None
        }
        fn video_texture(
            &mut self,
            _: &GpuContext,
            _: AssetId,
            _: Tick,
            _: bool,
        ) -> Option<GpuFrame> {
            panic!("Legacy source not requested")
        }
        fn still_texture(
            &mut self,
            _: &GpuContext,
            _: AssetId,
            _: u32,
            _: u32,
        ) -> Option<GpuFrame> {
            None
        }
        fn vector_texture(
            &mut self,
            _: &GpuContext,
            _: VectorRef,
            _: VectorStateKey,
            _: u32,
            _: u32,
        ) -> Option<GpuFrame> {
            None
        }
    }
    let asset = AssetId::new();
    let graph = FrameGraph {
        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
        nodes: vec![
            IrNode {
                op: IrOp::NativeDecodeVideo {
                    asset,
                    src_time: Tick(123),
                    input: NativeInputColorInterpretation {
                        version: 1,
                        standard: NativeInputStandard::Bt709Scene,
                        range: InputSignalRange::Limited,
                        matrix: InputMatrix::Bt709,
                        chroma_location: None,
                        hlg_peak_nits: None,
                        reference_white_nits: None,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(0x6563686f01),
            },
            IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: 0.1,
                        g: 0.2,
                        b: 0.3,
                        a: 1.,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(0x6563686f02),
            },
        ],
        output: Some(IrNodeId(1)),
    };
    assert_eq!(graph.demanded_nodes(None), vec![false, true]);
    assert!(graph.native_decode_requests(None).is_empty());
    assert_eq!(
        graph.native_decode_requests(Some(IrNodeId(0))),
        vec![(asset, Tick(123))]
    );
    let gpu =
        GpuContext::request_blocking().expect("GPU required for decode planning qualification");
    let mut evaluator = Evaluator::new(gpu);
    let mut source = Source {
        planned: vec![],
        calls: 0,
        reject: false,
    };
    assert!(evaluator.evaluate(&graph, (8, 4), &mut source).is_some());
    assert!(source.planned.is_empty());
    assert_eq!(source.calls, 0);
    let (output, tap) =
        evaluator.evaluate_with_tap_frame(&graph, (8, 4), &mut source, Some(IrNodeId(0)));
    assert!(output.is_some() && tap.is_none());
    assert_eq!(source.calls, 1);
    assert_eq!(source.planned, vec![(asset, Tick(123))]);
    source.reject = true;
    assert!(evaluator.evaluate(&graph, (8, 4), &mut source).is_none());
    assert_eq!(source.calls, 1, "rejected planning must not start a decode");
}

#[test]
fn native_mixed_resolution_resize_preserves_geometry_alpha_scope_and_quality_cache() {
    check_native_mixed_resolution_resize(None);
}

#[test]
fn native_mixed_resolution_pointwise_processing_preserves_source_extent_and_taps() {
    for op in [
        IrOp::NativeInvert,
        IrOp::NativeChromaKey {
            key: [0.0, 1.0, 0.0],
            tolerance: 0.3,
            softness: 0.2,
            spill: 0.0,
        },
        IrOp::NativeLumaKey {
            threshold: 0.2,
            softness: 0.1,
            invert: false,
        },
    ] {
        check_native_mixed_resolution_resize(Some(op));
    }
}

#[test]
fn native_source_resolution_gaussian_preserves_sigma_draft_cache_and_taps() {
    for sigma in [0.0, 0.49, 0.5, 1.25, 3.0, 50.0] {
        check_native_mixed_resolution_resize(Some(IrOp::NativeGaussianBlur { sigma }));
    }
}

#[test]
fn native_source_detail_filters_preserve_source_radius_alpha_and_quality_cache() {
    for op in [
        IrOp::NativeUnsharp {
            amount: 0.0,
            sigma: 1.25,
        },
        IrOp::NativeUnsharp {
            amount: 0.75,
            sigma: 1.25,
        },
        IrOp::NativeUnsharp {
            amount: 2.0,
            sigma: 3.0,
        },
        IrOp::NativeGlow {
            sigma: 1.25,
            threshold: 0.25,
            intensity: 0.0,
            tint: [0.5, 1.0, 0.2],
        },
        IrOp::NativeGlow {
            sigma: 1.25,
            threshold: 0.25,
            intensity: 0.75,
            tint: [0.5, 1.0, 0.2],
        },
        IrOp::NativeGlow {
            sigma: 3.0,
            threshold: 0.25,
            intensity: 2.0,
            tint: [0.5, 1.0, 0.2],
        },
    ] {
        check_native_mixed_resolution_resize(Some(op));
    }
    check_native_mixed_resolution_chain(&[
        IrOp::NativeUnsharp {
            amount: 0.5,
            sigma: 1.25,
        },
        IrOp::NativeGlow {
            sigma: 1.25,
            threshold: 0.25,
            intensity: 0.5,
            tint: [0.5, 1.0, 0.2],
        },
        IrOp::NativeInvert,
    ]);
}

#[test]
fn native_source_channels_preserve_geometry_signal_alpha_and_quality_cache() {
    use photonic_video::graph::ir::Channel;
    for channel in [Channel::R, Channel::G, Channel::B, Channel::A] {
        check_native_mixed_resolution_chain(&[
            IrOp::NativeChannelExtract { channel },
            IrOp::NativeMaskToImage,
        ]);
    }
    for channels in [
        [Channel::R, Channel::G, Channel::B, Channel::A],
        [Channel::B, Channel::G, Channel::R, Channel::A],
    ] {
        let mut ops = channels
            .into_iter()
            .map(|channel| IrOp::NativeChannelExtract { channel })
            .collect::<Vec<_>>();
        ops.push(IrOp::NativeChannelCombine);
        check_native_mixed_resolution_chain(&ops);
    }
    check_native_mixed_resolution_chain(&[IrOp::NativeImageToMask, IrOp::NativeMaskToImage]);
}

#[test]
fn native_source_geometry_preserves_source_canvas_alpha_and_quality_cache() {
    use photonic_video::graph::ir::Sampling;
    let crop = IrOp::Crop {
        left: 0.25,
        top: 0.0,
        right: 0.0,
        bottom: 0.25,
    };
    let center = glam::Vec2::new(2.0, 1.0);
    for mat in [
        glam::Mat3::IDENTITY,
        glam::Mat3::from_translation(glam::Vec2::new(0.5, -0.25)),
        glam::Mat3::from_translation(center)
            * glam::Mat3::from_angle(0.35)
            * glam::Mat3::from_scale(glam::Vec2::new(0.75, 1.25))
            * glam::Mat3::from_translation(-center),
    ] {
        let transform = IrOp::Transform2DTransparent {
            mat,
            sampling: Sampling::Bilinear,
        };
        check_native_mixed_resolution_chain(&[transform.clone()]);
        check_native_mixed_resolution_chain(&[crop.clone(), transform.clone(), IrOp::NativeInvert]);
        check_native_mixed_resolution_chain(&[transform, crop.clone()]);
    }
}

#[test]
fn native_same_extent_merge_preserves_source_geometry_alpha_and_quality_cache() {
    for mode in [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
    ] {
        check_native_mixed_resolution_chain(&[
            IrOp::NativeInvert,
            IrOp::NativeGaussianBlur { sigma: 1.25 },
            IrOp::Merge { mode, opacity: 0.5 },
        ]);
    }
}

#[test]
fn native_source_processing_chains_preserve_order_alpha_and_quality_cache() {
    let chain = [
        IrOp::NativeInvert,
        IrOp::NativeGaussianBlur { sigma: 1.25 },
        IrOp::NativeChromaKey {
            key: [0.0, 1.0, 0.0],
            tolerance: 0.3,
            softness: 0.2,
            spill: 0.0,
        },
        IrOp::NativeLumaKey {
            threshold: 0.2,
            softness: 0.1,
            invert: false,
        },
    ];
    check_native_mixed_resolution_chain(&chain);
    check_native_mixed_resolution_chain(&chain.into_iter().rev().collect::<Vec<_>>());
}

fn check_native_mixed_resolution_resize(pointwise: Option<IrOp>) {
    check_native_mixed_resolution_chain(&pointwise.into_iter().collect::<Vec<_>>());
}

fn check_native_mixed_resolution_chain(processing: &[IrOp]) {
    use photonic_core::timeline::color::{
        InputMatrix, InputSignalRange, NativeInputColorInterpretation, NativeInputStandard,
    };
    use photonic_core::timeline::{AssetId, Tick, VectorRef};
    use photonic_video::{
        contract::VectorStateKey,
        graph::eval::{GpuFrame, GpuFrameSource},
    };
    struct Source {
        frame: GpuFrame,
        errors: Vec<String>,
        times: Vec<Tick>,
        still_requests: Vec<(u32, u32)>,
    }
    impl GpuFrameSource for Source {
        fn video_texture(
            &mut self,
            _: &GpuContext,
            _: AssetId,
            _: Tick,
            _: bool,
        ) -> Option<GpuFrame> {
            panic!("native MediaIn cannot use Legacy decoding")
        }
        fn native_video_texture(
            &mut self,
            _: &GpuContext,
            _: AssetId,
            time: Tick,
            _: &NativeInputColorInterpretation,
        ) -> Option<GpuFrame> {
            self.times.push(time);
            Some(self.frame.clone())
        }
        fn still_texture(
            &mut self,
            _: &GpuContext,
            _: AssetId,
            _: u32,
            _: u32,
        ) -> Option<GpuFrame> {
            panic!("native MediaIn cannot use Legacy stills")
        }
        fn native_still_texture(
            &mut self,
            _: &GpuContext,
            _: AssetId,
            w: u32,
            h: u32,
        ) -> Option<GpuFrame> {
            self.still_requests.push((w, h));
            Some(self.frame.clone())
        }
        fn report_native_source_error(&mut self, _: AssetId, error: String) {
            self.errors.push(error);
        }
        fn vector_texture(
            &mut self,
            _: &GpuContext,
            _: VectorRef,
            _: VectorStateKey,
            _: u32,
            _: u32,
        ) -> Option<GpuFrame> {
            None
        }
    }
    use photonic_video::graph::eval_cpu::FrameProvider;
    use photonic_video::graph::ir::FitMode;
    use photonic_video::graph::ops::Image;
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let pixels: Vec<[f32; 4]> = (0..8)
        .map(|i| {
            [
                [-0.125, 0.125, 0.5, 1.0][i % 4],
                if i < 4 { 0.125 } else { 0.25 },
                0.125,
                0.5,
            ]
        })
        .collect();
    let mut data = vec![0_u16; 64 * 64 * 4];
    for p in data.chunks_exact_mut(4) {
        p.copy_from_slice(&[0x3c00, 0, 0x3c00, 0x3c00]);
    }
    let half = |v: f32| match v {
        0. => 0,
        -0.125 => 0xb000,
        0.125 => 0x3000,
        0.25 => 0x3400,
        0.5 => 0x3800,
        1.0 => 0x3c00,
        _ => panic!("fixture value"),
    };
    for y in 0..2 {
        for x in 0..4 {
            for c in 0..4 {
                data[(y * 64 + x) * 4 + c] = half(pixels[y * 4 + x][c]);
            }
        }
    }
    let texture = gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("mixed source with poisoned bucket padding"),
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    gpu.queue().write_texture(
        texture.as_image_copy(),
        bytemuck::cast_slice(&data),
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(64 * 8),
            rows_per_image: Some(64),
        },
        texture.size(),
    );
    let mut source = Source {
        frame: GpuFrame::new(std::sync::Arc::new(texture), 4, 2),
        errors: vec![],
        times: vec![],
        still_requests: vec![],
    };
    struct Cpu(Image);
    impl FrameProvider for Cpu {
        fn decode_video(&mut self, _: AssetId, _: Tick, _: bool, _: u32, _: u32) -> Image {
            panic!("legacy")
        }
        fn decode_native_video(
            &mut self,
            _: AssetId,
            _: Tick,
            _: &NativeInputColorInterpretation,
            _: u32,
            _: u32,
        ) -> Image {
            self.0.clone()
        }
        fn decode_still(&mut self, _: AssetId, _: u32, _: u32) -> Image {
            self.0.clone()
        }
        fn raster_vector(&mut self, _: VectorRef, _: VectorStateKey, w: u32, h: u32) -> Image {
            Image::new(w, h)
        }
    }
    let input = NativeInputColorInterpretation {
        version: 1,
        standard: NativeInputStandard::Bt709Scene,
        range: InputSignalRange::Limited,
        matrix: InputMatrix::Bt709,
        chroma_location: None,
        hlg_peak_nits: None,
        reference_white_nits: None,
    };
    let asset = AssetId::new();
    let mut eval = Evaluator::new(gpu.clone());
    for still in [false, true] {
        for (index, fit) in [FitMode::Fit, FitMode::Fill, FitMode::Stretch]
            .into_iter()
            .enumerate()
        {
            for (target_index, (iw, ih)) in [(8, 8), (3, 7), (12, 4), (1, 1), (16, 16)]
                .into_iter()
                .enumerate()
            {
                let hash = 1_080_000
                    + 1000 * (still as u128)
                    + 100 * index as u128
                    + 10 * target_index as u128;
                let op = if still {
                    IrOp::NativeMediaStill {
                        asset,
                        w: 4,
                        h: 2,
                        native_extent: true,
                    }
                } else {
                    IrOp::NativeMediaVideo {
                        asset,
                        src_time: Tick::ZERO,
                        input: input.clone(),
                        w: 4,
                        h: 2,
                        native_extent: true,
                    }
                };
                let mut graph = FrameGraph {
                    working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                    nodes: vec![
                        IrNode {
                            op,
                            inputs: vec![],
                            content_hash: ContentHash(hash),
                        },
                        IrNode {
                            op: IrOp::Resize { w: iw, h: ih, fit },
                            inputs: vec![(IrNodeId(0), OutPort(0))],
                            content_hash: ContentHash(hash + 1),
                        },
                        IrNode {
                            op: IrOp::Resize {
                                w: 8,
                                h: 8,
                                fit: FitMode::Stretch,
                            },
                            inputs: vec![(IrNodeId(1), OutPort(0))],
                            content_hash: ContentHash(hash + 2),
                        },
                        IrNode {
                            op: IrOp::NativeCanvas { w: 8, h: 8 },
                            inputs: vec![(IrNodeId(2), OutPort(0))],
                            content_hash: ContentHash(hash + 3),
                        },
                    ],
                    output: Some(IrNodeId(3)),
                };
                let count = processing.len() as u32;
                for node in &mut graph.nodes[1..] {
                    for (id, _) in &mut node.inputs {
                        if id.0 >= 1 {
                            id.0 += count;
                        }
                    }
                }
                graph.nodes[1].inputs[0].0 = IrNodeId(count);
                graph.nodes.splice(
                    1..1,
                    processing.iter().enumerate().map(|(i, op)| IrNode {
                        op: op.clone(),
                        inputs: if matches!(op, IrOp::Merge { .. }) {
                            vec![(IrNodeId(i as u32), OutPort(0)), (IrNodeId(0), OutPort(0))]
                        } else if matches!(op, IrOp::NativeChannelExtract { .. }) {
                            vec![(IrNodeId(0), OutPort(0))]
                        } else if matches!(op, IrOp::NativeChannelCombine) {
                            ((i as u32 - 3)..=i as u32)
                                .map(|j| (IrNodeId(j), OutPort(0)))
                                .collect()
                        } else {
                            vec![(IrNodeId(i as u32), OutPort(0))]
                        },
                        content_hash: ContentHash(hash + 5 + i as u128),
                    }),
                );
                graph.output.as_mut().unwrap().0 += count;
                let tap_id = IrNodeId(count);
                assert!(graph.validate_working_color_domain().is_ok());
                for size in [8, 4, 8] {
                    let mut cpu = Cpu(Image {
                        width: 4,
                        height: 2,
                        pixels: pixels.clone(),
                    });
                    let expected = eval_cpu::evaluate(&graph, (size, size), &mut cpu);
                    let (out, tap) = eval.evaluate_with_tap_frame(
                        &graph,
                        (size, size),
                        &mut source,
                        Some(tap_id),
                    );
                    let out = out.expect("resized picture");
                    let tap = tap.expect("source scope");
                    assert_eq!((out.width, out.height), (size, size));
                    assert_eq!((tap.width, tap.height), (4, 2));
                    let actual = read_texture_rgba16f(&gpu, &out.texture, size, size);
                    for (i, (p, want)) in actual.iter().zip(&expected.pixels).enumerate() {
                        for c in 0..4 {
                            // Scalar RGB extraction exposes unassociated values
                            // above one. Bound its three subsequent fp16 output
                            // stores by three relative fp16 ULPs; retain the
                            // existing absolute alpha and ordinary-image checks.
                            let channel_signal = processing.iter().any(|op| {
                                matches!(
                                    op,
                                    IrOp::NativeChannelExtract { .. } | IrOp::NativeImageToMask
                                )
                            });
                            let tolerance = if channel_signal && c < 3 {
                                3.0 * (1.0 / 1024.0) * want[c].abs().max(1.0)
                            } else {
                                0.002
                            };
                            assert!(
                                (p[c] - want[c]).abs() < tolerance,
                                "still{still} fit{fit:?} intermediate{iw}x{ih} size{size} pixel{i}: {p:?} vs{want:?}, bound {tolerance}"
                            );
                        }
                        let inside = !matches!(fit, FitMode::Fit)
                            || (i / (size as usize) >= size as usize / 4
                                && i / (size as usize) < size as usize * 3 / 4);
                        if (iw, ih) == (8, 8) && processing.is_empty() {
                            assert!(
                                (p[3] - if inside { 0.5 } else { 0. }).abs() < 0.002,
                                "explicit fit coverage"
                            );
                        }
                    }
                    let scope = read_texture_rgba16f(&gpu, &tap.texture, 4, 2);
                    if !processing.is_empty() {
                        let reference =
                            eval_cpu::evaluate_at(&graph, (size, size), &mut cpu, Some(tap_id));
                        assert_eq!((reference.width, reference.height), (4, 2));
                        for (actual, expected) in scope.iter().zip(&reference.pixels) {
                            for c in 0..4 {
                                assert!((actual[c] - expected[c]).abs() < 0.003);
                            }
                        }
                        for id in 0..count {
                            let id = IrNodeId(id);
                            let (_, stage) = eval.evaluate_with_tap_frame(
                                &graph,
                                (size, size),
                                &mut source,
                                Some(id),
                            );
                            let stage = stage.expect("intermediate source-pixel scope");
                            assert_eq!((stage.width, stage.height), (4, 2));
                            let actual = read_texture_rgba16f(&gpu, &stage.texture, 4, 2);
                            let reference =
                                eval_cpu::evaluate_at(&graph, (size, size), &mut cpu, Some(id));
                            for (p, q) in actual.iter().zip(&reference.pixels) {
                                for c in 0..4 {
                                    assert!((p[c] - q[c]).abs() < 0.003);
                                }
                            }
                        }
                        continue;
                    }
                    for (p, raw) in scope.iter().zip(&pixels) {
                        let want = if still {
                            let m = photonic_video::color::native::linear_srgb_to_ap1();
                            let x = m.transform([raw[0] as f64, raw[1] as f64, raw[2] as f64]);
                            [x[0] as f32, x[1] as f32, x[2] as f32, raw[3]]
                        } else {
                            *raw
                        };
                        for c in 0..4 {
                            assert!(
                                (p[c] - want[c]).abs() < 0.002,
                                "source scope changed geometry or padding"
                            );
                        }
                    }
                }
                let mut bad = graph.clone();
                bad.nodes[1].op = IrOp::NativeExposure { stops: 0. };
                assert!(bad.validate_working_color_domain().is_err());
                let mut bad = graph.clone();
                bad.output = Some(IrNodeId(0));
                assert!(bad.validate_working_color_domain().is_err());
            }
        }
    }
}

#[test]
fn native_mixed_resolution_source_budget_counts_instances_and_padding() {
    use photonic_core::timeline::AssetId;
    use photonic_video::graph::ir::FitMode;
    for (count, w, h, allowed) in [
        (1, 4096, 2048, true),
        (2, 4096, 2048, true),
        (3, 4096, 2048, false),
        (2, 4097, 2048, false),
        (1, u32::MAX, 1, false),
        (1, 0, 1, false),
    ] {
        let mut nodes = Vec::new();
        for _ in 0..count {
            let id = IrNodeId(nodes.len() as u32);
            nodes.push(IrNode {
                op: IrOp::NativeMediaStill {
                    asset: AssetId::new(),
                    w,
                    h,
                    native_extent: true,
                },
                inputs: vec![],
                content_hash: ContentHash(1),
            });
            nodes.push(IrNode {
                op: IrOp::Resize {
                    w: 16,
                    h: 16,
                    fit: FitMode::Fit,
                },
                inputs: vec![(id, OutPort(0))],
                content_hash: ContentHash(2),
            });
        }
        let output = Some(IrNodeId(nodes.len() as u32 - 1));
        let graph = FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes,
            output,
        };
        assert_eq!(
            graph.validate_working_color_domain().is_ok(),
            allowed,
            "count{count} {w}x{h}"
        );
    }
}

#[test]
fn native_source_extent_budget_includes_pointwise_intermediates() {
    use photonic_core::timeline::AssetId;
    use photonic_video::graph::ir::FitMode;
    for (passes, allowed) in [(0, true), (1, true), (2, false)] {
        let mut nodes = vec![IrNode {
            op: IrOp::NativeMediaStill {
                asset: AssetId::new(),
                w: 4096,
                h: 2048,
                native_extent: true,
            },
            inputs: vec![],
            content_hash: ContentHash(1),
        }];
        for _ in 0..passes {
            let input = IrNodeId(nodes.len() as u32 - 1);
            nodes.push(IrNode {
                op: IrOp::NativeInvert,
                inputs: vec![(input, OutPort(0))],
                content_hash: ContentHash(1),
            });
        }
        let input = IrNodeId(nodes.len() as u32 - 1);
        nodes.push(IrNode {
            op: IrOp::Resize {
                w: 16,
                h: 16,
                fit: FitMode::Stretch,
            },
            inputs: vec![(input, OutPort(0))],
            content_hash: ContentHash(2),
        });
        let graph = FrameGraph {
            output: Some(IrNodeId(nodes.len() as u32 - 1)),
            nodes,
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
        };
        assert_eq!(graph.validate_working_color_domain().is_ok(), allowed);
        if passes > 0 {
            let mut unnormalized = graph.clone();
            unnormalized.output = Some(input);
            assert!(unnormalized.validate_working_color_domain().is_err());
        }
    }
}

#[test]
fn native_source_gaussian_budget_counts_three_temporaries() {
    use photonic_core::timeline::AssetId;
    use photonic_video::graph::ir::FitMode;
    for (width, allowed) in [(1024, true), (2048, false)] {
        let graph = FrameGraph {
            nodes: vec![
                IrNode {
                    op: IrOp::NativeMediaStill {
                        asset: AssetId::new(),
                        w: width,
                        h: 2048,
                        native_extent: true,
                    },
                    inputs: vec![],
                    content_hash: ContentHash(1),
                },
                IrNode {
                    op: IrOp::NativeGaussianBlur { sigma: 2.0 },
                    inputs: vec![(IrNodeId(0), OutPort(0))],
                    content_hash: ContentHash(2),
                },
                IrNode {
                    op: IrOp::Resize {
                        w: 16,
                        h: 16,
                        fit: FitMode::Stretch,
                    },
                    inputs: vec![(IrNodeId(1), OutPort(0))],
                    content_hash: ContentHash(3),
                },
            ],
            output: Some(IrNodeId(2)),
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
        };
        assert_eq!(graph.validate_working_color_domain().is_ok(), allowed);
    }
}

#[test]
fn native_source_detail_budget_counts_all_scratch_textures() {
    use photonic_core::timeline::AssetId;
    use photonic_video::graph::ir::FitMode;
    for (op, width, allowed) in [
        (
            IrOp::NativeUnsharp {
                amount: 1.0,
                sigma: 1.25,
            },
            1280,
            true,
        ),
        (
            IrOp::NativeUnsharp {
                amount: 1.0,
                sigma: 1.25,
            },
            1408,
            false,
        ),
        (
            IrOp::NativeGlow {
                sigma: 1.25,
                threshold: 0.2,
                intensity: 1.0,
                tint: [1.0; 3],
            },
            1152,
            true,
        ),
        (
            IrOp::NativeGlow {
                sigma: 1.25,
                threshold: 0.2,
                intensity: 1.0,
                tint: [1.0; 3],
            },
            1216,
            false,
        ),
    ] {
        let graph = FrameGraph {
            nodes: vec![
                IrNode {
                    op: IrOp::NativeMediaStill {
                        asset: AssetId::new(),
                        w: width,
                        h: 2048,
                        native_extent: true,
                    },
                    inputs: vec![],
                    content_hash: ContentHash(1),
                },
                IrNode {
                    op,
                    inputs: vec![(IrNodeId(0), OutPort(0))],
                    content_hash: ContentHash(2),
                },
                IrNode {
                    op: IrOp::Resize {
                        w: 16,
                        h: 16,
                        fit: FitMode::Stretch,
                    },
                    inputs: vec![(IrNodeId(1), OutPort(0))],
                    content_hash: ContentHash(3),
                },
            ],
            output: Some(IrNodeId(2)),
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
        };
        assert_eq!(
            graph.validate_working_color_domain().is_ok(),
            allowed,
            "width {width}"
        );
    }
}

#[test]
fn native_source_channels_refuse_mixed_extents_missing_inputs_and_excess_budget() {
    use photonic_core::timeline::AssetId;
    use photonic_video::graph::ir::{Channel, FitMode};
    for (width, height, other_width, other_native, allowed) in [
        (4, 2, 4, true, true),
        (4, 2, 5, true, false),
        (4, 2, 4, false, false),
        (1600, 2048, 1600, true, true),
        (1664, 2048, 1664, true, false),
    ] {
        let source = |w, native_extent| IrOp::NativeMediaStill {
            asset: AssetId::new(),
            w,
            h: height,
            native_extent,
        };
        let mut graph = FrameGraph {
            nodes: vec![
                IrNode {
                    op: source(width, true),
                    inputs: vec![],
                    content_hash: ContentHash(1),
                },
                IrNode {
                    op: source(other_width, other_native),
                    inputs: vec![],
                    content_hash: ContentHash(2),
                },
                IrNode {
                    op: IrOp::NativeChannelExtract {
                        channel: Channel::R,
                    },
                    inputs: vec![(IrNodeId(0), OutPort(0))],
                    content_hash: ContentHash(3),
                },
                IrNode {
                    op: IrOp::NativeChannelExtract {
                        channel: Channel::A,
                    },
                    inputs: vec![(IrNodeId(1), OutPort(0))],
                    content_hash: ContentHash(4),
                },
                IrNode {
                    op: IrOp::NativeChannelCombine,
                    inputs: vec![
                        (IrNodeId(2), OutPort(0)),
                        (IrNodeId(2), OutPort(0)),
                        (IrNodeId(2), OutPort(0)),
                        (IrNodeId(3), OutPort(0)),
                    ],
                    content_hash: ContentHash(5),
                },
                IrNode {
                    op: IrOp::Resize {
                        w: 16,
                        h: 16,
                        fit: FitMode::Stretch,
                    },
                    inputs: vec![(IrNodeId(4), OutPort(0))],
                    content_hash: ContentHash(6),
                },
            ],
            output: Some(IrNodeId(5)),
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
        };
        assert_eq!(
            graph.validate_working_color_domain().is_ok(),
            allowed,
            "width{width} other{other_width} native{other_native}"
        );
        graph.nodes[4].inputs.pop();
        assert!(graph.validate_working_color_domain().is_err());
    }
}

#[test]
fn native_source_merge_refuses_mismatched_or_canvas_inputs() {
    use photonic_core::timeline::AssetId;
    use photonic_video::graph::ir::FitMode;
    for (w, native_extent, allowed) in [(4, true, true), (5, true, false), (4, false, false)] {
        let source = |width, native| IrOp::NativeMediaStill {
            asset: AssetId::new(),
            w: width,
            h: 2,
            native_extent: native,
        };
        let graph = FrameGraph {
            nodes: vec![
                IrNode {
                    op: source(4, true),
                    inputs: vec![],
                    content_hash: ContentHash(1),
                },
                IrNode {
                    op: source(w, native_extent),
                    inputs: vec![],
                    content_hash: ContentHash(2),
                },
                IrNode {
                    op: IrOp::Merge {
                        mode: BlendMode::Normal,
                        opacity: 0.5,
                    },
                    inputs: vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))],
                    content_hash: ContentHash(3),
                },
                IrNode {
                    op: IrOp::Resize {
                        w: 16,
                        h: 16,
                        fit: FitMode::Stretch,
                    },
                    inputs: vec![(IrNodeId(2), OutPort(0))],
                    content_hash: ContentHash(4),
                },
            ],
            output: Some(IrNodeId(3)),
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
        };
        assert_eq!(
            graph.validate_working_color_domain().is_ok(),
            allowed,
            "width{w} native{native_extent}"
        );
        let mut swapped = graph.clone();
        swapped.nodes[2].inputs.reverse();
        assert_eq!(swapped.validate_working_color_domain().is_ok(), allowed);
        if allowed {
            let mut height_mismatch = graph.clone();
            if let IrOp::NativeMediaStill { h, .. } = &mut height_mismatch.nodes[1].op {
                *h = 3;
            }
            assert!(height_mismatch.validate_working_color_domain().is_err());
            // Two resident sources plus the merge output must all count.
            // These adjacent 64-pixel buckets straddle the 16M-texel limit.
            for (width, fits) in [(2688, true), (2752, false)] {
                let mut bounded = graph.clone();
                for source in &mut bounded.nodes[..2] {
                    if let IrOp::NativeMediaStill { w, h, .. } = &mut source.op {
                        *w = width;
                        *h = 2048;
                    }
                }
                assert_eq!(bounded.validate_working_color_domain().is_ok(), fits);
            }
        }
        let mut missing = graph.clone();
        missing.nodes[2].inputs.pop();
        assert!(missing.validate_working_color_domain().is_err());
        let mut unnormalized = graph.clone();
        unnormalized.output = Some(IrNodeId(2));
        assert!(unnormalized.validate_working_color_domain().is_err());
    }
}

fn directional_reference(w: u32, h: u32, rgba: [f32; 4], delta: [f32; 2]) -> Vec<[f64; 4]> {
    // Independent binary64 evaluation of the documented line/bilinear contract.
    let source = |x: u32, y: u32| {
        let px = x as f64 + 0.5;
        let py = y as f64 + 0.5;
        if px >= w as f64 * 0.25
            && px < w as f64 * 0.875
            && py >= h as f64 * 0.25
            && py < h as f64 * 0.75
        {
            rgba.map(f64::from)
        } else {
            [0.0; 4]
        }
    };
    let dx = f64::from(delta[0]);
    let dy = f64::from(delta[1]);
    let steps = dx.hypot(dy).ceil().clamp(1.0, 128.0) as u32;
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let mut pixel = [0.0; 4];
            for i in 0..=steps {
                let u = i as f64 / steps as f64 - 0.5;
                let sx = (x as f64 + dx * u).clamp(0.0, (w - 1) as f64);
                let sy = (y as f64 + dy * u).clamp(0.0, (h - 1) as f64);
                let x0 = sx.floor() as u32;
                let y0 = sy.floor() as u32;
                let fx = sx - x0 as f64;
                let fy = sy - y0 as f64;
                for (ix, iy, weight) in [
                    (x0, y0, (1.0 - fx) * (1.0 - fy)),
                    ((x0 + 1).min(w - 1), y0, fx * (1.0 - fy)),
                    (x0, (y0 + 1).min(h - 1), (1.0 - fx) * fy),
                    ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1), fx * fy),
                ] {
                    let v = source(ix, iy);
                    for c in 0..4 {
                        pixel[c] += v[c] * weight / (steps + 1) as f64;
                    }
                }
            }
            out.push(pixel);
        }
    }
    out
}

fn directional_graph(
    rgba: [f32; 4],
    delta: [f32; 2],
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut nodes = vec![
        IrNode {
            op: IrOp::SolidColor {
                color: LinearColor {
                    r: rgba[0],
                    g: rgba[1],
                    b: rgba[2],
                    a: rgba[3],
                },
            },
            inputs: vec![],
            content_hash: ContentHash(hash),
        },
        IrNode {
            op: IrOp::Crop {
                left: 0.25,
                top: 0.25,
                right: 0.125,
                bottom: 0.25,
            },
            inputs: vec![(IrNodeId(0), OutPort::default())],
            content_hash: ContentHash(hash + 1),
        },
        IrNode {
            op: IrOp::NativeMotionBlur { delta },
            inputs: vec![(IrNodeId(1), OutPort::default())],
            content_hash: ContentHash(hash + 2),
        },
    ];
    if let Some((w, h)) = canvas {
        nodes.push(IrNode {
            op: IrOp::NativeCanvas { w, h },
            inputs: vec![(IrNodeId(2), OutPort::default())],
            content_hash: ContentHash(hash + 3),
        });
    }
    FrameGraph {
        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
        output: Some(IrNodeId(nodes.len() as u32 - 1)),
        nodes,
    }
}

#[test]
fn native_motion_blur_matches_binary64_reference_and_zero_padding() {
    let gpu =
        GpuContext::request_blocking().expect("GPU required for native motion blur qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 2_500_000;
    for (w, h) in [(1, 1), (7, 5), (16, 12)] {
        for alpha in [0.0f32, 0.25, 1.0] {
            for angle in [0.0f64, 37.0, 90.0, 225.0, 359.0] {
                for distance in [0.0f64, 0.25, 1.0, 8.0, 128.5, 500.0] {
                    hash += 8;
                    let rgba = [-0.5 * alpha, 3.0 * alpha, 0.25 * alpha, alpha];
                    let a = angle.to_radians();
                    let clean = |v: f64| if v.abs() < 1e-10 { 0.0 } else { v as f32 };
                    let delta = [clean(a.cos() * distance), clean(a.sin() * distance)];
                    let graph = directional_graph(rgba, delta, hash, None);
                    assert!(graph.validate_working_color_domain().is_ok());
                    let expected = directional_reference(w, h, rgba, delta);
                    let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
                    let frame = evaluator
                        .evaluate_with_tap_frame(&graph, (w, h), &mut NullFrameSource, None)
                        .0
                        .unwrap();
                    let pixels = read_texture_rgba16f(&gpu, &frame.texture, w, h);
                    for (i, (actual, expected)) in pixels.iter().zip(&expected).enumerate() {
                        for c in 0..4 {
                            assert!((cpu.pixels[i][c] as f64-expected[c]).abs()<0.00003,"CPU {w} {h} {angle} {distance} {i}/{c}: {:?} expected {expected:?}",cpu.pixels[i]);
                            assert!((actual[c] as f64-expected[c]).abs()<0.006,"GPU {w} {h} {angle} {distance} {i}/{c}: {actual:?} expected {expected:?}");
                        }
                    }
                    if alpha == 1.0 && angle == 37.0 && distance == 128.5 {
                        let pw = frame.texture.width();
                        let ph = frame.texture.height();
                        let padded = read_texture_rgba16f(&gpu, &frame.texture, pw, ph);
                        for y in 0..ph {
                            for x in 0..pw {
                                if x >= w || y >= h {
                                    assert_eq!(padded[(y * pw + x) as usize], [0.0; 4]);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn native_motion_blur_draft_scales_each_axis_and_cache_is_size_aware() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let rgba = [-0.25, 1.5, 0.125, 0.5];
    let graph = directional_graph(rgba, [8.0, 6.0], 2_600_000, Some((16, 12)));
    for (w, h) in [(16, 12), (8, 3), (4, 6), (16, 12), (8, 3)] {
        let delta = [8.0 * w as f32 / 16.0, 6.0 * h as f32 / 12.0];
        let expected = directional_reference(w, h, rgba, delta);
        let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
        let frame = evaluator
            .evaluate_with_tap_frame(&graph, (w, h), &mut NullFrameSource, None)
            .0
            .unwrap();
        assert_eq!((frame.width, frame.height), (w, h));
        let pixels = read_texture_rgba16f(&gpu, &frame.texture, w, h);
        for (i, pixel) in pixels.iter().enumerate() {
            for c in 0..4 {
                assert!(
                    (cpu.pixels[i][c] as f64 - expected[i][c]).abs() < 0.00003,
                    "CPU {w}x{h}"
                );
                assert!(
                    (pixel[c] as f64 - expected[i][c]).abs() < 0.006,
                    "GPU {w}x{h}: {pixel:?} vs {:?}",
                    expected[i]
                );
            }
        }
    }
}

#[test]
fn native_motion_blur_rejects_invalid_domain_arity_and_vectors() {
    let mut graph = directional_graph([0.1, 0.2, 0.3, 1.0], [3.0, 4.0], 2_700_000, None);
    assert!(graph.validate_working_color_domain().is_ok());
    for delta in [
        [f32::NAN, 0.0],
        [f32::INFINITY, 0.0],
        [501.0, 0.0],
        [400.0, 400.0],
    ] {
        graph.nodes[2].op = IrOp::NativeMotionBlur { delta };
        assert!(graph.validate_working_color_domain().is_err());
    }
    graph.nodes[2].op = IrOp::NativeMotionBlur { delta: [3.0, 4.0] };
    graph.nodes[2].inputs.clear();
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[2].inputs = vec![
        (IrNodeId(1), OutPort::default()),
        (IrNodeId(0), OutPort::default()),
    ];
    assert!(graph.validate_working_color_domain().is_err());
}

fn mosaic_reference(w: u32, h: u32, rgba: [f32; 4], block: [u32; 2]) -> Vec<[f64; 4]> {
    // Independent summed-area table, rather than the production cell loops.
    let stride = (w + 1) as usize;
    let mut integral = vec![[0.0f64; 4]; stride * (h + 1) as usize];
    for y in 0..h {
        for x in 0..w {
            let inside = (x as f64 + 0.5) >= w as f64 * 0.25
                && (x as f64 + 0.5) < w as f64 * 0.875
                && (y as f64 + 0.5) >= h as f64 * 0.25
                && (y as f64 + 0.5) < h as f64 * 0.75;
            let p = if inside {
                rgba.map(f64::from)
            } else {
                [0.0; 4]
            };
            let i = (y + 1) as usize * stride + (x + 1) as usize;
            for c in 0..4 {
                integral[i][c] = p[c] + integral[i - 1][c] + integral[i - stride][c]
                    - integral[i - stride - 1][c];
            }
        }
    }
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let x0 = x / block[0] * block[0];
            let y0 = y / block[1] * block[1];
            let x1 = (x0 + block[0]).min(w);
            let y1 = (y0 + block[1]).min(h);
            let area = ((x1 - x0) * (y1 - y0)) as f64;
            let mut p = [0.0; 4];
            for c in 0..4 {
                p[c] = (integral[y1 as usize * stride + x1 as usize][c]
                    - integral[y0 as usize * stride + x1 as usize][c]
                    - integral[y1 as usize * stride + x0 as usize][c]
                    + integral[y0 as usize * stride + x0 as usize][c])
                    / area;
            }
            out.push(p);
        }
    }
    out
}

fn mosaic_graph(
    rgba: [f32; 4],
    block: [u32; 2],
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut graph = directional_graph(rgba, [0.0; 2], hash, canvas);
    graph.nodes[2].op = IrOp::NativeMosaic { block };
    graph
}

#[test]
fn native_mosaic_matches_integral_reference_and_zero_padding() {
    let gpu = GpuContext::request_blocking().expect("GPU required for native mosaic");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 2_800_000;
    for (w, h) in [(1, 1), (7, 5), (16, 12), (257, 259)] {
        for alpha in [0.0f32, 0.25, 1.0] {
            for block in [[1, 1], [2, 3], [7, 5], [32, 4], [256, 256]] {
                hash += 8;
                let rgba = [-0.5 * alpha, 3.0 * alpha, 0.25 * alpha, alpha];
                let graph = mosaic_graph(rgba, block, hash, None);
                assert!(graph.validate_working_color_domain().is_ok());
                let expected = mosaic_reference(w, h, rgba, block);
                let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
                let frame = evaluator
                    .evaluate_with_tap_frame(&graph, (w, h), &mut NullFrameSource, None)
                    .0
                    .unwrap();
                let pixels = read_texture_rgba16f(&gpu, &frame.texture, w, h);
                for (i, (actual, expected)) in pixels.iter().zip(&expected).enumerate() {
                    for c in 0..4 {
                        assert!(
                            (cpu.pixels[i][c] as f64 - expected[c]).abs() < 0.000003,
                            "CPU {w}x{h} {block:?} {i}/{c}"
                        );
                        assert!(
                            (actual[c] as f64 - expected[c]).abs() < 0.004,
                            "GPU {w}x{h} {block:?} {i}/{c}: {actual:?} expected{expected:?}"
                        );
                    }
                }
                if alpha == 1.0 && block == [256, 256] {
                    let (pw, ph) = (frame.texture.width(), frame.texture.height());
                    let padded = read_texture_rgba16f(&gpu, &frame.texture, pw, ph);
                    for y in 0..ph {
                        for x in 0..pw {
                            if x >= w || y >= h {
                                assert_eq!(padded[(y * pw + x) as usize], [0.0; 4]);
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn native_mosaic_draft_rounds_each_axis_and_reuses_cache_safely() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let rgba = [-0.25, 1.5, 0.125, 0.5];
    let graph = mosaic_graph(rgba, [7, 5], 2_900_000, Some((16, 12)));
    for (w, h) in [(16, 12), (8, 3), (4, 6), (16, 12), (8, 3)] {
        let block = [
            (7.0 * w as f64 / 16.0).round().max(1.0) as u32,
            (5.0 * h as f64 / 12.0).round().max(1.0) as u32,
        ];
        let expected = mosaic_reference(w, h, rgba, block);
        let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
        let frame = evaluator
            .evaluate_with_tap_frame(&graph, (w, h), &mut NullFrameSource, None)
            .0
            .unwrap();
        for (i, p) in read_texture_rgba16f(&gpu, &frame.texture, w, h)
            .iter()
            .enumerate()
        {
            for c in 0..4 {
                assert!((cpu.pixels[i][c] as f64 - expected[i][c]).abs() < 0.000003);
                assert!(
                    (p[c] as f64 - expected[i][c]).abs() < 0.004,
                    "{w}x{h} {block:?}: {p:?} != {:?}",
                    expected[i]
                );
            }
        }
    }
}

#[test]
fn native_mosaic_rejects_invalid_cells_domain_and_arity() {
    let mut graph = mosaic_graph([0.1, 0.2, 0.3, 1.0], [2, 3], 3_000_000, None);
    for block in [[0, 1], [1, 0], [257, 1], [1, u32::MAX]] {
        graph.nodes[2].op = IrOp::NativeMosaic { block };
        assert!(graph.validate_working_color_domain().is_err());
    }
    graph.nodes[2].op = IrOp::NativeMosaic { block: [2, 3] };
    graph.nodes[1].op = IrOp::NativeAcescct {
        direction: photonic_render::native_transfer::AcescctDirection::Encode,
    };
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    graph.nodes[2].inputs.clear();
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(graph.validate_working_color_domain().is_err());
}

// Independent binary64 oracle: count contributing clamped samples along each
// axis of the rectangular source, then multiply the two coverage fractions.
// This does not invoke the CPU sliding window or GPU convolution kernels.
fn box_reference(w: u32, h: u32, rgba: [f32; 4], radius: [u32; 2]) -> Vec<[f64; 4]> {
    let axis = |position: u32, size: u32, r: u32, lo: f64, hi: f64| {
        let count = (-(r as i64)..=r as i64)
            .filter(|offset| {
                let sample =
                    (i64::from(position) + offset).clamp(0, i64::from(size) - 1) as f64 + 0.5;
                sample >= size as f64 * lo && sample < size as f64 * hi
            })
            .count();
        count as f64 / (2 * r + 1) as f64
    };
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let a = axis(x, w, radius[0], 0.25, 0.875) * axis(y, h, radius[1], 0.25, 0.75);
            out.push(rgba.map(|v| f64::from(v) * a));
        }
    }
    out
}
fn box_graph(
    rgba: [f32; 4],
    radius: [u32; 2],
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut graph = directional_graph(rgba, [0.0; 2], hash, canvas);
    graph.nodes[2].op = IrOp::NativeBoxBlur { radius };
    graph
}
#[test]
fn native_box_blur_matches_independent_coverage_and_zero_padding() {
    let gpu = GpuContext::request_blocking().expect("GPU required for native box blur");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 3_100_000;
    for (w, h) in [(1, 1), (1, 7), (7, 1), (7, 5), (16, 12), (65, 67)] {
        for alpha in [0.0f32, 0.25, 1.0] {
            for radius in [[0, 0], [0, 3], [2, 0], [2, 3], [7, 5], [500, 500]] {
                hash += 8;
                let rgba = [-0.5 * alpha, 3.0 * alpha, 0.25 * alpha, alpha];
                let graph = box_graph(rgba, radius, hash, None);
                assert!(graph.validate_working_color_domain().is_ok());
                let expected = box_reference(w, h, rgba, radius);
                let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
                let frame = evaluator
                    .evaluate_with_tap_frame(&graph, (w, h), &mut NullFrameSource, None)
                    .0
                    .unwrap();
                let pixels = read_texture_rgba16f(&gpu, &frame.texture, w, h);
                for (i, (actual, expected)) in pixels.iter().zip(&expected).enumerate() {
                    for c in 0..4 {
                        assert!(
                            (cpu.pixels[i][c] as f64 - expected[c]).abs() < 0.000003,
                            "CPU {w}x{h} {radius:?} {i}/{c}"
                        );
                        assert!(
                            (actual[c] as f64 - expected[c]).abs() < 0.004,
                            "GPU {w}x{h} {radius:?} {i}/{c}: {actual:?} expected{expected:?}"
                        );
                    }
                }
                if alpha == 1.0 && radius == [500, 500] {
                    let (pw, ph) = (frame.texture.width(), frame.texture.height());
                    let padded = read_texture_rgba16f(&gpu, &frame.texture, pw, ph);
                    for y in 0..ph {
                        for x in 0..pw {
                            if x >= w || y >= h {
                                assert_eq!(padded[(y * pw + x) as usize], [0.0; 4]);
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_box_blur_draft_rounds_axes_and_reuses_cache_safely() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let rgba = [-0.25, 1.5, 0.125, 0.5];
    let graph = box_graph(rgba, [7, 1], 3_200_000, Some((16, 12)));
    for (w, h) in [(16, 12), (8, 3), (4, 6), (16, 12), (8, 3)] {
        let radius = [
            (7.0 * w as f64 / 16.0).round() as u32,
            (h as f64 / 12.0).round() as u32,
        ];
        let expected = box_reference(w, h, rgba, radius);
        let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
        let frame = evaluator
            .evaluate_with_tap_frame(&graph, (w, h), &mut NullFrameSource, None)
            .0
            .unwrap();
        for (i, p) in read_texture_rgba16f(&gpu, &frame.texture, w, h)
            .iter()
            .enumerate()
        {
            for c in 0..4 {
                assert!((cpu.pixels[i][c] as f64 - expected[i][c]).abs() < 0.000003);
                assert!(
                    (p[c] as f64 - expected[i][c]).abs() < 0.004,
                    "{w}x{h} {radius:?} {p:?} != {:?}",
                    expected[i]
                );
            }
        }
    }
}
#[test]
fn native_box_blur_rejects_invalid_radii_domain_and_arity() {
    let mut graph = box_graph([0.1, 0.2, 0.3, 1.0], [2, 3], 3_300_000, None);
    for radius in [[501, 0], [0, 501], [u32::MAX, 1]] {
        graph.nodes[2].op = IrOp::NativeBoxBlur { radius };
        assert!(graph.validate_working_color_domain().is_err());
    }
    graph.nodes[2].op = IrOp::NativeBoxBlur { radius: [2, 3] };
    graph.nodes[1].op = IrOp::NativeAcescct {
        direction: photonic_render::native_transfer::AcescctDirection::Encode,
    };
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    graph.nodes[2].inputs.clear();
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(graph.validate_working_color_domain().is_err());
}

fn geometric_graph(
    kind: photonic_video::graph::ir::NativeWarpKind,
    rgba: [f32; 4],
    amount: f32,
    wavelength: f32,
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut g = directional_graph(rgba, [0.0; 2], hash, canvas);
    g.nodes[2].op = IrOp::NativeGeometricWarp {
        kind,
        amount,
        wavelength,
        pixel_scale: [1.0; 2],
    };
    g
}
fn geometric_reference(
    w: u32,
    h: u32,
    rgba: [f32; 4],
    kind: photonic_video::graph::ir::NativeWarpKind,
    amount: f64,
    wavelength: f64,
    scale: [f64; 2],
) -> Vec<[f64; 4]> {
    use photonic_video::graph::ir::NativeWarpKind;
    let size = [w as f64 / scale[0], h as f64 / scale[1]];
    let center = [size[0] / 2.0, size[1] / 2.0];
    let radius = size[0].min(size[1]) / 2.0;
    let source = |x: u32, y: u32| {
        if (x as f64 + 0.5) >= w as f64 * 0.25
            && (x as f64 + 0.5) < w as f64 * 0.875
            && (y as f64 + 0.5) >= h as f64 * 0.25
            && (y as f64 + 0.5) < h as f64 * 0.75
        {
            rgba.map(f64::from)
        } else {
            [0.0; 4]
        }
    };
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let px = (x as f64 + 0.5) / scale[0];
            let py = (y as f64 + 0.5) / scale[1];
            let mut q = [px, py];
            if amount != 0.0 {
                if kind == NativeWarpKind::Ripple {
                    q = [
                        px + amount * (std::f64::consts::TAU * py / wavelength).sin(),
                        py + amount * (std::f64::consts::TAU * px / wavelength).sin(),
                    ];
                } else {
                    let dx = px - center[0];
                    let dy = py - center[1];
                    let r = dx.hypot(dy);
                    if r > 0.0 && r < radius {
                        let n = r / radius;
                        let dest_r = match kind {
                            NativeWarpKind::Pinch => r + amount * r * (1.0 - n) * (1.0 - n),
                            NativeWarpKind::Spherize => {
                                let curved = if amount >= 0.0 {
                                    radius * n.asin() / std::f64::consts::FRAC_PI_2
                                } else {
                                    radius * (n * std::f64::consts::FRAC_PI_2).sin()
                                };
                                r * (1.0 - amount.abs()) + curved * amount.abs()
                            }
                            _ => unreachable!(),
                        };
                        q = [center[0] + dx / r * dest_r, center[1] + dy / r * dest_r];
                    }
                }
            }
            let sx = (q[0] * scale[0] - 0.5).clamp(0.0, w as f64 - 1.0);
            let sy = (q[1] * scale[1] - 0.5).clamp(0.0, h as f64 - 1.0);
            let x0 = sx.floor() as u32;
            let y0 = sy.floor() as u32;
            let fx = sx - x0 as f64;
            let fy = sy - y0 as f64;
            let mut pixel = [0.0; 4];
            for (xx, yy, weight) in [
                (x0, y0, (1.0 - fx) * (1.0 - fy)),
                ((x0 + 1).min(w - 1), y0, fx * (1.0 - fy)),
                (x0, (y0 + 1).min(h - 1), (1.0 - fx) * fy),
                ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1), fx * fy),
            ] {
                let p = source(xx, yy);
                for c in 0..4 {
                    pixel[c] += p[c] * weight;
                }
            }
            out.push(pixel);
        }
    }
    out
}
#[test]
fn native_geometric_warps_match_independent_radial_and_wave_reference() {
    use photonic_video::graph::ir::NativeWarpKind::*;
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 3_400_000;
    for (w, h) in [(1, 1), (7, 5), (16, 12), (257, 3)] {
        for alpha in [0.0f32, 0.25, 1.0] {
            let mut cases = Vec::new();
            for kind in [Pinch, Spherize] {
                for amount in [-1.0f32, -0.35, 0.0, 0.4, 1.0] {
                    cases.push((kind, amount, 1.0));
                }
            }
            for amount in [-64.0f32, -3.5, 0.0, 5.0, 64.0] {
                for wavelength in [1.0f32, 7.5, 512.0] {
                    cases.push((Ripple, amount, wavelength));
                }
            }
            for (kind, amount, wavelength) in cases {
                hash += 8;
                let rgba = [-0.5 * alpha, 3.0 * alpha, 0.25 * alpha, alpha];
                let graph = geometric_graph(kind, rgba, amount, wavelength, hash, None);
                assert!(graph.validate_working_color_domain().is_ok());
                let expected = geometric_reference(
                    w,
                    h,
                    rgba,
                    kind,
                    amount as f64,
                    wavelength as f64,
                    [1.0; 2],
                );
                let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
                let frame = evaluator
                    .evaluate_with_tap_frame(&graph, (w, h), &mut NullFrameSource, None)
                    .0
                    .unwrap();
                let pixels = read_texture_rgba16f(&gpu, &frame.texture, w, h);
                for (i, (actual, expected)) in pixels.iter().zip(&expected).enumerate() {
                    for c in 0..4 {
                        assert!((cpu.pixels[i][c] as f64-expected[c]).abs()<0.001,"CPU {kind:?} {w}x{h} {amount}/{wavelength} {i}/{c}: {:?} vs{expected:?}",cpu.pixels[i]);
                        assert!((actual[c] as f64-expected[c]).abs()<0.006,"GPU {kind:?} {w}x{h} {amount}/{wavelength} {i}/{c}: {actual:?} vs{expected:?}");
                    }
                }
                if alpha == 1.0 && amount == 0.0 {
                    let (pw, ph) = (frame.texture.width(), frame.texture.height());
                    let padded = read_texture_rgba16f(&gpu, &frame.texture, pw, ph);
                    for y in 0..ph {
                        for x in 0..pw {
                            if x >= w || y >= h {
                                assert_eq!(padded[(y * pw + x) as usize], [0.0; 4]);
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_geometric_warps_draft_preserves_authored_geometry_and_cache() {
    use photonic_video::graph::ir::NativeWarpKind::*;
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let rgba = [-0.25, 1.5, 0.125, 0.5];
    for (i, (kind, amount, wavelength)) in [
        (Pinch, 0.8, 1.0),
        (Spherize, -0.75, 1.0),
        (Ripple, 7.0, 11.0),
    ]
    .into_iter()
    .enumerate()
    {
        let graph = geometric_graph(
            kind,
            rgba,
            amount,
            wavelength,
            3_500_000 + 8 * i as u128,
            Some((16, 12)),
        );
        for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12), (8, 3)] {
            let scale = [w as f64 / 16.0, h as f64 / 12.0];
            let expected =
                geometric_reference(w, h, rgba, kind, amount as f64, wavelength as f64, scale);
            let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
            let frame = evaluator
                .evaluate_with_tap_frame(&graph, (w, h), &mut NullFrameSource, None)
                .0
                .unwrap();
            for (i, p) in read_texture_rgba16f(&gpu, &frame.texture, w, h)
                .iter()
                .enumerate()
            {
                for c in 0..4 {
                    assert!((cpu.pixels[i][c] as f64 - expected[i][c]).abs() < 0.001);
                    assert!(
                        (p[c] as f64 - expected[i][c]).abs() < 0.006,
                        "{kind:?} {w}x{h}: {p:?} vs{:?}",
                        expected[i]
                    );
                }
            }
        }
    }
}
#[test]
fn native_geometric_warps_refuse_invalid_parameters_and_domain() {
    use photonic_video::graph::ir::NativeWarpKind::*;
    let mut graph = geometric_graph(Pinch, [0.1, 0.2, 0.3, 1.0], 0.5, 1.0, 3_600_000, None);
    for (kind, amount, wavelength, scale) in [
        (Pinch, 1.01, 1.0, [1.0; 2]),
        (Spherize, -1.01, 1.0, [1.0; 2]),
        (Ripple, 64.01, 1.0, [1.0; 2]),
        (Ripple, 1.0, 0.0, [1.0; 2]),
        (Ripple, 1.0, 512.01, [1.0; 2]),
        (Pinch, 0.0, 2.0, [1.0; 2]),
        (Pinch, f32::NAN, 1.0, [1.0; 2]),
        (Ripple, 1.0, f32::INFINITY, [1.0; 2]),
        (Pinch, 0.0, 1.0, [0.0, 1.0]),
    ] {
        graph.nodes[2].op = IrOp::NativeGeometricWarp {
            kind,
            amount,
            wavelength,
            pixel_scale: scale,
        };
        assert!(graph.validate_working_color_domain().is_err());
    }
    graph.nodes[2].op = IrOp::NativeGeometricWarp {
        kind: Pinch,
        amount: 0.5,
        wavelength: 1.0,
        pixel_scale: [1.0; 2],
    };
    graph.nodes[1].op = IrOp::NativeAcescct {
        direction: photonic_render::native_transfer::AcescctDirection::Encode,
    };
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    graph.nodes[2].inputs.clear();
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(graph.validate_working_color_domain().is_err());
}

fn detail_graph(
    kind: photonic_video::graph::ir::NativeDetailKind,
    amount: f32,
    sigma: f32,
    threshold: f32,
    alpha: f32,
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut index = 0;
    let mut node = |op, inputs: Vec<u32>| {
        index += 1;
        IrNode {
            op,
            inputs: inputs
                .into_iter()
                .map(|i| (IrNodeId(i), OutPort(0)))
                .collect(),
            content_hash: ContentHash(hash + index),
        }
    };
    let mut nodes = vec![
        node(
            IrOp::SolidColor {
                color: LinearColor {
                    r: 0.125 * alpha,
                    g: 0.2 * alpha,
                    b: 0.05 * alpha,
                    a: 0.5 * alpha,
                },
            },
            vec![],
        ),
        node(
            IrOp::Crop {
                left: 0.0,
                top: 0.0,
                right: 0.6,
                bottom: 0.0,
            },
            vec![0],
        ),
        node(
            IrOp::SolidColor {
                color: LinearColor {
                    r: 0.75 * alpha,
                    g: -0.0625 * alpha,
                    b: 0.125 * alpha,
                    a: 0.25 * alpha,
                },
            },
            vec![],
        ),
        node(
            IrOp::Crop {
                left: 0.6,
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
            },
            vec![2],
        ),
        node(
            IrOp::Merge {
                mode: BlendMode::Normal,
                opacity: 1.0,
            },
            vec![3, 1],
        ),
        node(
            IrOp::NativeDetail {
                kind,
                amount,
                sigma,
                threshold,
                pixel_scale: [1.0; 2],
            },
            vec![4],
        ),
    ];
    if let Some((w, h)) = canvas {
        nodes.push(node(IrOp::NativeCanvas { w, h }, vec![5]));
    }
    FrameGraph {
        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
        output: Some(IrNodeId(nodes.len() as u32 - 1)),
        nodes,
    }
}
fn detail_reference(
    w: u32,
    h: u32,
    kind: photonic_video::graph::ir::NativeDetailKind,
    amount: f64,
    sigma: f64,
    threshold: f64,
    scale: [f64; 2],
    alpha: f64,
) -> Vec<[f64; 4]> {
    use photonic_video::graph::ir::NativeDetailKind::*;
    let sigma = if kind == Clarity {
        ((w as f64 / scale[0]).min(h as f64 / scale[1]) / 8.0).clamp(3.0, 60.0)
    } else {
        sigma
    } * scale[0].min(scale[1]);
    let original: Vec<[f64; 4]> = (0..w)
        .map(|x| {
            let v = if (x as f64 + 0.5) < w as f64 * 0.4 {
                [0.125, 0.2, 0.05, 0.5]
            } else if (x as f64 + 0.5) >= w as f64 * 0.6 {
                [0.75, -0.0625, 0.125, 0.25]
            } else {
                [0.0; 4]
            };
            v.map(|v| v * alpha)
        })
        .collect();
    let mut low = original.clone();
    // Independent binary64 finite-kernel Gaussian planning. Rows are constant,
    // so vertical filtering is identity, including repeated planned passes.
    if sigma >= 0.5 {
        let n = if sigma <= 12.0 {
            1
        } else {
            ((sigma / 12.0).powi(2).ceil() as usize).clamp(2, 16)
        };
        let sp = sigma / (n as f64).sqrt();
        let step = ((sp * 3.0).ceil() / 128.0).ceil().clamp(1.0, 4.0);
        let radius = (((sp * 3.0).ceil() / step).ceil() as i32).min(128);
        let weights: Vec<_> = (-radius..=radius)
            .map(|k| (-((k as f64 * step).powi(2)) / (2.0 * sp * sp)).exp())
            .collect();
        let total: f64 = weights.iter().sum();
        for _ in 0..n {
            low = (0..w)
                .map(|x| {
                    let mut p = [0.0; 4];
                    for (i, weight) in weights.iter().enumerate() {
                        let offset = ((i as i32 - radius) as f64 * step).round() as i32;
                        let sample = low[(x as i32 + offset).clamp(0, w as i32 - 1) as usize];
                        for c in 0..4 {
                            p[c] += sample[c] * weight / total;
                        }
                    }
                    p
                })
                .collect();
        }
    }
    original
        .iter()
        .zip(&low)
        .map(|(src, low)| {
            if kind != HighPass && (amount == 0.0 || sigma < 0.5) {
                return *src;
            }
            if src[3] == 0.0 {
                return [0.0; 4];
            }
            let straight = [src[0] / src[3], src[1] / src[3], src[2] / src[3]];
            let luma = straight[0] * 0.2722287168
                + straight[1] * 0.6740817658
                + straight[2] * 0.0536895174;
            let bell = (1.0 - 2.0 * (luma - 0.5).abs()).clamp(0.0, 1.0);
            let mut out = *src;
            for c in 0..3 {
                let delta = if low[3] > 1e-8 {
                    straight[c] - low[c] / low[3]
                } else {
                    0.0
                };
                out[c] = src[3]
                    * match kind {
                        HighPass => 128.0 / 255.0 + delta,
                        Clarity => straight[c] + amount * bell * delta,
                        SmartSharpen => {
                            straight[c]
                                + amount
                                    * ((delta.abs() - threshold) / (32.0 / 255.0)).clamp(0.0, 1.0)
                                    * delta
                        }
                    };
            }
            out
        })
        .collect()
}
#[test]
fn native_detail_filters_match_independent_extended_alpha_reference() {
    use photonic_video::graph::ir::NativeDetailKind::*;
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 3_700_000;
    let mut cases = Vec::new();
    for sigma in [0.0f32, 0.4, 1.0, 12.0, 100.0, 500.0] {
        cases.push((HighPass, 1.0, sigma, 0.0));
    }
    for amount in [0.0f32, 2.0, 10.0] {
        for threshold in [0.0f32, 0.1, 1.0] {
            for sigma in [0.0f32, 0.4, 1.0, 12.0, 100.0] {
                cases.push((SmartSharpen, amount, sigma, threshold));
            }
        }
    }
    for amount in [-1.0f32, -0.5, 0.0, 0.5, 1.0] {
        cases.push((Clarity, amount, 0.0, 0.0));
    }
    for alpha in [0.0f32, 0.5, 1.0] {
        for &(kind, amount, sigma, threshold) in &cases {
            hash += 10;
            let graph = detail_graph(kind, amount, sigma, threshold, alpha, hash, None);
            assert!(graph.validate_working_color_domain().is_ok());
            let expected = detail_reference(
                17,
                9,
                kind,
                amount as f64,
                sigma as f64,
                threshold as f64,
                [1.0; 2],
                alpha as f64,
            );
            let cpu = eval_cpu::evaluate(&graph, (17, 9), &mut EmptyProvider);
            let frame = evaluator
                .evaluate_with_tap_frame(&graph, (17, 9), &mut NullFrameSource, None)
                .0
                .unwrap();
            let pixels = read_texture_rgba16f(&gpu, &frame.texture, 17, 9);
            for (i, (c, g)) in cpu.pixels.iter().zip(&pixels).enumerate() {
                let e = expected[i % 17];
                for k in 0..3 {
                    assert!(
                        (c[k] as f64 - e[k]).abs() < 0.0005,
                        "CPU {kind:?} {amount}/{sigma}/{threshold} {i}/{k}: {c:?} vs{e:?}"
                    );
                    assert!(
                        (g[k] as f64 - e[k]).abs() < 0.045 * e[k].abs().max(1.0),
                        "GPU {kind:?} {amount}/{sigma}/{threshold} {i}/{k}: {g:?} vs{e:?}"
                    );
                }
                assert_eq!(c[3], e[3] as f32);
                assert_eq!(g[3], e[3] as f32);
            }
            if alpha == 1.0 && amount == 1.0 {
                let (pw, ph) = (frame.texture.width(), frame.texture.height());
                let padded = read_texture_rgba16f(&gpu, &frame.texture, pw, ph);
                for y in 0..ph {
                    for x in 0..pw {
                        if x >= 17 || y >= 9 {
                            assert_eq!(padded[(y * pw + x) as usize], [0.0; 4]);
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_detail_draft_and_cache_follow_authored_radius() {
    use photonic_video::graph::ir::NativeDetailKind::*;
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    for (i, (kind, amount, sigma, threshold)) in [
        (HighPass, 1.0, 12.0, 0.0),
        (SmartSharpen, 2.0, 12.0, 0.1),
        (Clarity, -0.75, 0.0, 0.0),
    ]
    .into_iter()
    .enumerate()
    {
        let graph = detail_graph(
            kind,
            amount,
            sigma,
            threshold,
            1.0,
            3_800_000 + i as u128 * 10,
            Some((128, 96)),
        );
        for (w, h) in [(128, 96), (64, 24), (16, 12), (128, 96)] {
            let expected = detail_reference(
                w,
                h,
                kind,
                amount as f64,
                sigma as f64,
                threshold as f64,
                [w as f64 / 128.0, h as f64 / 96.0],
                1.0,
            );
            let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
            let frame = evaluator
                .evaluate_with_tap_frame(&graph, (w, h), &mut NullFrameSource, None)
                .0
                .unwrap();
            for (i, p) in read_texture_rgba16f(&gpu, &frame.texture, w, h)
                .iter()
                .enumerate()
            {
                let e = expected[i % w as usize];
                for c in 0..4 {
                    assert!((cpu.pixels[i][c] as f64 - e[c]).abs() < 0.0005);
                    assert!(
                        (p[c] as f64 - e[c]).abs() < 0.02 * e[c].abs().max(1.0),
                        "{kind:?} {w}x{h} {i}/{c}: {p:?} vs{e:?}"
                    );
                }
            }
        }
    }
}
#[test]
fn native_detail_rejects_invalid_parameters_domain_and_arity() {
    use photonic_video::graph::ir::NativeDetailKind::*;
    let mut graph = detail_graph(HighPass, 1.0, 2.0, 0.0, 1.0, 3_900_000, None);
    for (kind, amount, sigma, threshold) in [
        (HighPass, 0.0, 2.0, 0.0),
        (HighPass, 1.0, 500.01, 0.0),
        (HighPass, 1.0, 2.0, 0.1),
        (SmartSharpen, -0.1, 1.0, 0.0),
        (SmartSharpen, 11.0, 1.0, 0.0),
        (SmartSharpen, 1.0, 101.0, 0.0),
        (SmartSharpen, 1.0, 1.0, 1.01),
        (Clarity, 1.01, 0.0, 0.0),
        (Clarity, 0.5, 1.0, 0.0),
        (Clarity, 0.5, 0.0, f32::NAN),
    ] {
        graph.nodes[5].op = IrOp::NativeDetail {
            kind,
            amount,
            sigma,
            threshold,
            pixel_scale: [1.0; 2],
        };
        assert!(graph.validate_working_color_domain().is_err());
    }
    graph.nodes[5].op = IrOp::NativeDetail {
        kind: HighPass,
        amount: 1.0,
        sigma: 2.0,
        threshold: 0.0,
        pixel_scale: [1.0; 2],
    };
    graph.nodes[4].op = IrOp::NativeAcescct {
        direction: photonic_render::native_transfer::AcescctDirection::Encode,
    };
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[4].op = IrOp::Merge {
        mode: BlendMode::Normal,
        opacity: 1.0,
    };
    graph.nodes[5].inputs.clear();
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[5].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(4), OutPort(0))];
    assert!(graph.validate_working_color_domain().is_err());
}

fn stylize_graph(
    kind: photonic_video::graph::ir::NativeStylizeKind,
    rgba: [f32; 4],
    amount: f32,
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut graph = directional_graph(rgba, [0.0; 2], hash, canvas);
    graph.nodes[2].op = IrOp::NativeStylize {
        kind,
        amount,
        pixel_scale: [1.0; 2],
    };
    graph
}
fn stylize_reference(
    w: u32,
    h: u32,
    rgba: [f32; 4],
    kind: photonic_video::graph::ir::NativeStylizeKind,
    amount: f64,
    scale: [f64; 2],
) -> Vec<[f64; 4]> {
    use photonic_video::graph::ir::NativeStylizeKind::*;
    let source = |x: u32, y: u32| {
        if (x as f64 + 0.5) >= w as f64 * 0.25
            && (x as f64 + 0.5) < w as f64 * 0.875
            && (y as f64 + 0.5) >= h as f64 * 0.25
            && (y as f64 + 0.5) < h as f64 * 0.75
        {
            rgba.map(f64::from)
        } else {
            [0.0; 4]
        }
    };
    let sample = |px: f64, py: f64| {
        let x = (px - 0.5).clamp(0.0, w as f64 - 1.0);
        let y = (py - 0.5).clamp(0.0, h as f64 - 1.0);
        let x0 = x.floor() as u32;
        let y0 = y.floor() as u32;
        let fx = x - x0 as f64;
        let fy = y - y0 as f64;
        let mut p = [0.0; 4];
        for (xx, yy, weight) in [
            (x0, y0, (1.0 - fx) * (1.0 - fy)),
            ((x0 + 1).min(w - 1), y0, fx * (1.0 - fy)),
            (x0, (y0 + 1).min(h - 1), (1.0 - fx) * fy),
            ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1), fx * fy),
        ] {
            let v = source(xx, yy);
            for c in 0..4 {
                p[c] += v[c] * weight;
            }
        }
        if p[3] > 1e-8 {
            [p[0] / p[3], p[1] / p[3], p[2] / p[3]]
        } else {
            [0.0; 3]
        }
    };
    let luma = |p: [f64; 3]| p[0] * 0.2722287168 + p[1] * 0.6740817658 + p[2] * 0.0536895174;
    let aw = w as f64 / scale[0];
    let ah = h as f64 / scale[1];
    let diagonal = ((aw - 1.0) / 2.0).hypot((ah - 1.0) / 2.0).max(0.0001);
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let src = source(x, y);
            if kind == ChromaticAberration && amount == 0.0 {
                out.push(src);
                continue;
            }
            if src[3] <= 0.0 {
                out.push([0.0; 4]);
                continue;
            }
            let px = x as f64 + 0.5;
            let py = y as f64 + 0.5;
            if kind == ChromaticAberration {
                let ax = px / scale[0];
                let ay = py / scale[1];
                let dx = (ax - aw / 2.0) * amount / diagonal;
                let dy = (ay - ah / 2.0) * amount / diagonal;
                let red = sample((ax + dx) * scale[0], (ay + dy) * scale[1]);
                let blue = sample((ax - dx) * scale[0], (ay - dy) * scale[1]);
                out.push([red[0] * src[3], src[1], blue[2] * src[3], src[3]]);
            } else {
                let mut values = [0.0; 9];
                for k in 0..9 {
                    values[k] = luma(sample(
                        px + (k % 3) as f64 * scale[0] - scale[0],
                        py + (k / 3) as f64 * scale[1] - scale[1],
                    ));
                }
                let dot =
                    |kernel: [f64; 9]| values.iter().zip(kernel).map(|(a, b)| a * b).sum::<f64>();
                let gray = if kind == Emboss {
                    128.0 / 255.0 + dot([-1.0, -1.0, 0.0, -1.0, 0.0, 1.0, 0.0, 1.0, 1.0])
                } else {
                    dot([-1.0, 0.0, 1.0, -2.0, 0.0, 2.0, -1.0, 0.0, 1.0])
                        .hypot(dot([-1.0, -2.0, -1.0, 0.0, 0.0, 0.0, 1.0, 2.0, 1.0]))
                };
                out.push([gray * src[3], gray * src[3], gray * src[3], src[3]]);
            }
        }
    }
    out
}
#[test]
fn native_stylize_matches_independent_spatial_extended_alpha_reference() {
    use photonic_video::graph::ir::NativeStylizeKind::*;
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 4_000_000;
    let mut cases = vec![(Emboss, 0.0), (FindEdges, 0.0)];
    for a in [-64.0f32, -8.0, -1.0, 0.0, 0.5, 1.0, 8.0, 32.0, 64.0] {
        cases.push((ChromaticAberration, a));
    }
    for (w, h) in [(1, 1), (1, 7), (7, 1), (7, 5), (16, 12), (257, 3)] {
        for alpha in [0.0f32, 0.25, 1.0] {
            for &(kind, amount) in &cases {
                hash += 8;
                let rgba = [-0.5 * alpha, 3.0 * alpha, 0.25 * alpha, alpha];
                let graph = stylize_graph(kind, rgba, amount, hash, None);
                assert!(graph.validate_working_color_domain().is_ok());
                let expected = stylize_reference(w, h, rgba, kind, amount as f64, [1.0; 2]);
                let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
                let frame = evaluator
                    .evaluate_with_tap_frame(&graph, (w, h), &mut NullFrameSource, None)
                    .0
                    .unwrap();
                let pixels = read_texture_rgba16f(&gpu, &frame.texture, w, h);
                for (i, (c, g)) in cpu.pixels.iter().zip(&pixels).enumerate() {
                    for k in 0..4 {
                        assert!(
                            (c[k] as f64 - expected[i][k]).abs() < 0.0002,
                            "CPU {kind:?} {w}x{h} amount{amount} {i}/{k}: {c:?} vs{:?}",
                            expected[i]
                        );
                        assert!(
                            (g[k] as f64 - expected[i][k]).abs()
                                < 0.006 * expected[i][k].abs().max(1.0),
                            "GPU {kind:?} {w}x{h} amount{amount} {i}/{k}: {g:?} vs{:?}",
                            expected[i]
                        );
                    }
                }
                if alpha == 1.0 && amount == 0.0 {
                    let (pw, ph) = (frame.texture.width(), frame.texture.height());
                    let pad = read_texture_rgba16f(&gpu, &frame.texture, pw, ph);
                    for y in 0..ph {
                        for x in 0..pw {
                            if x >= w || y >= h {
                                assert_eq!(pad[(y * pw + x) as usize], [0.0; 4]);
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_stylize_draft_preserves_authored_stencils_and_cache() {
    use photonic_video::graph::ir::NativeStylizeKind::*;
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let rgba = [-0.25, 1.5, 0.125, 0.5];
    for (i, (kind, amount)) in [(Emboss, 0.0), (FindEdges, 0.0), (ChromaticAberration, 5.0)]
        .into_iter()
        .enumerate()
    {
        let graph = stylize_graph(
            kind,
            rgba,
            amount,
            4_100_000 + 8 * i as u128,
            Some((16, 12)),
        );
        for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
            let expected = stylize_reference(
                w,
                h,
                rgba,
                kind,
                amount as f64,
                [w as f64 / 16.0, h as f64 / 12.0],
            );
            let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
            let frame = evaluator
                .evaluate_with_tap_frame(&graph, (w, h), &mut NullFrameSource, None)
                .0
                .unwrap();
            for (i, p) in read_texture_rgba16f(&gpu, &frame.texture, w, h)
                .iter()
                .enumerate()
            {
                for c in 0..4 {
                    assert!((cpu.pixels[i][c] as f64 - expected[i][c]).abs() < 0.0002);
                    assert!(
                        (p[c] as f64 - expected[i][c]).abs()
                            < 0.006 * expected[i][c].abs().max(1.0),
                        "{kind:?} {w}x{h} {i}/{c}: {p:?} vs{:?}",
                        expected[i]
                    );
                }
            }
        }
    }
}
#[test]
fn native_stylize_refuses_bad_parameters_domain_and_arity() {
    use photonic_video::graph::ir::NativeStylizeKind::*;
    let mut graph = stylize_graph(Emboss, [0.1, 0.2, 0.3, 1.0], 0.0, 4_200_000, None);
    for (kind, amount, scale) in [
        (Emboss, 1.0, [1.0; 2]),
        (FindEdges, -1.0, [1.0; 2]),
        (ChromaticAberration, 64.01, [1.0; 2]),
        (ChromaticAberration, -64.01, [1.0; 2]),
        (ChromaticAberration, f32::NAN, [1.0; 2]),
        (Emboss, 0.0, [0.0, 1.0]),
    ] {
        graph.nodes[2].op = IrOp::NativeStylize {
            kind,
            amount,
            pixel_scale: scale,
        };
        assert!(graph.validate_working_color_domain().is_err());
    }
    graph.nodes[2].op = IrOp::NativeStylize {
        kind: Emboss,
        amount: 0.0,
        pixel_scale: [1.0; 2],
    };
    graph.nodes[1].op = IrOp::NativeAcescct {
        direction: photonic_render::native_transfer::AcescctDirection::Encode,
    };
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    graph.nodes[2].inputs.clear();
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(graph.validate_working_color_domain().is_err());
}

fn perspective_graph(
    corners: [f32; 8],
    rgba: [f32; 4],
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut graph = directional_graph(rgba, [0.0; 2], hash, canvas);
    graph.nodes[2].op = IrOp::NativePerspective {
        inverse: photonic_video::graph::ops::native_perspective_inverse(corners)
            .expect("valid test quad"),
    };
    graph
}
/// Independent forward unit-square homography followed by adjugate inversion.
/// Deliberately does not use the production pivoted inverse solve.
fn perspective_reference_inverse(c: [f32; 8]) -> [f64; 9] {
    let [x0, y0, x1, y1, x2, y2, x3, y3] = c.map(f64::from);
    let dx1 = x1 - x2;
    let dx2 = x3 - x2;
    let dx3 = x0 - x1 + x2 - x3;
    let dy1 = y1 - y2;
    let dy2 = y3 - y2;
    let dy3 = y0 - y1 + y2 - y3;
    let (g, h) = if dx3.abs() + dy3.abs() < 1e-12 {
        (0.0, 0.0)
    } else {
        let det = dx1 * dy2 - dx2 * dy1;
        ((dx3 * dy2 - dx2 * dy3) / det, (dx1 * dy3 - dx3 * dy1) / det)
    };
    let a = [
        x1 - x0 + g * x1,
        x3 - x0 + h * x3,
        x0,
        y1 - y0 + g * y1,
        y3 - y0 + h * y3,
        y0,
        g,
        h,
        1.0,
    ];
    let b = [
        a[4] * a[8] - a[5] * a[7],
        a[2] * a[7] - a[1] * a[8],
        a[1] * a[5] - a[2] * a[4],
        a[5] * a[6] - a[3] * a[8],
        a[0] * a[8] - a[2] * a[6],
        a[2] * a[3] - a[0] * a[5],
        a[3] * a[7] - a[4] * a[6],
        a[1] * a[6] - a[0] * a[7],
        a[0] * a[4] - a[1] * a[3],
    ];
    b.map(|v| v / b[8])
}
fn perspective_reference(w: u32, h: u32, rgba: [f32; 4], corners: [f32; 8]) -> Vec<[f64; 4]> {
    let a = perspective_reference_inverse(corners);
    let source = |x: u32, y: u32| {
        if (x as f64 + 0.5) >= w as f64 * 0.25
            && (x as f64 + 0.5) < w as f64 * 0.875
            && (y as f64 + 0.5) >= h as f64 * 0.25
            && (y as f64 + 0.5) < h as f64 * 0.75
        {
            rgba.map(f64::from)
        } else {
            [0.0; 4]
        }
    };
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let px = (x as f64 + 0.5) / w as f64;
            let py = (y as f64 + 0.5) / h as f64;
            let d = a[6] * px + a[7] * py + a[8];
            let u = (a[0] * px + a[1] * py + a[2]) / d;
            let v = (a[3] * px + a[4] * py + a[5]) / d;
            if d.abs() < 1e-6 || !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                out.push([0.0; 4]);
                continue;
            }
            let sx = (u * w as f64 - 0.5).clamp(0.0, w as f64 - 1.0);
            let sy = (v * h as f64 - 0.5).clamp(0.0, h as f64 - 1.0);
            let x0 = sx.floor() as u32;
            let y0 = sy.floor() as u32;
            let fx = sx - x0 as f64;
            let fy = sy - y0 as f64;
            let mut p = [0.0; 4];
            for (xx, yy, weight) in [
                (x0, y0, (1.0 - fx) * (1.0 - fy)),
                ((x0 + 1).min(w - 1), y0, fx * (1.0 - fy)),
                (x0, (y0 + 1).min(h - 1), (1.0 - fx) * fy),
                ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1), fx * fy),
            ] {
                let v = source(xx, yy);
                for k in 0..4 {
                    p[k] += v[k] * weight;
                }
            }
            out.push(p);
        }
    }
    out
}
fn perspective_quads() -> [[f32; 8]; 8] {
    [
        [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
        [0.137, 0.183, 0.863, 0.183, 0.863, 0.817, 0.137, 0.817],
        [0.137, -0.083, 1.137, -0.083, 1.137, 0.917, 0.137, 0.917],
        [0.231, 0.133, 0.811, 0.079, 0.957, 0.911, 0.059, 0.867],
        [-1.0, -1.0, 2.0, -1.0, 2.0, 2.0, -1.0, 2.0],
        [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        [0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0],
        [-0.183, 0.157, 1.217, -0.111, 0.789, 1.137, 0.117, 0.843],
    ]
}
#[test]
fn native_perspective_true_projective_extended_alpha_reference() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 4_300_000;
    for corners in perspective_quads() {
        let expected_h = perspective_reference_inverse(corners);
        let actual = photonic_video::graph::ops::native_perspective_inverse(corners).unwrap();
        for k in 0..9 {
            assert!(
                (actual[k] as f64 - expected_h[k]).abs() < 1e-5,
                "matrix {corners:?}: {actual:?} vs{expected_h:?}"
            );
        }
        for (w, h) in [(1, 1), (1, 7), (7, 1), (7, 5), (16, 12), (257, 3)] {
            for alpha in [0.0f32, 0.25, 1.0] {
                hash += 8;
                let rgba = [-0.5 * alpha, 3.0 * alpha, 0.25 * alpha, alpha];
                let graph = perspective_graph(corners, rgba, hash, None);
                assert!(graph.validate_working_color_domain().is_ok());
                let expected = perspective_reference(w, h, rgba, corners);
                let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
                let frame = evaluator
                    .evaluate_with_tap_frame(&graph, (w, h), &mut NullFrameSource, None)
                    .0
                    .unwrap();
                let pixels = read_texture_rgba16f(&gpu, &frame.texture, w, h);
                for (i, (c, g)) in cpu.pixels.iter().zip(&pixels).enumerate() {
                    for k in 0..4 {
                        assert!(
                            (c[k] as f64 - expected[i][k]).abs() < 0.0003,
                            "CPU {corners:?} {w}x{h} {i}/{k}: {c:?} vs{:?}",
                            expected[i]
                        );
                        assert!(
                            (g[k] as f64 - expected[i][k]).abs()
                                < 0.007 * expected[i][k].abs().max(1.0),
                            "GPU {corners:?} {w}x{h} {i}/{k}: {g:?} vs{:?}",
                            expected[i]
                        );
                    }
                }
                if alpha == 1.0 {
                    let (pw, ph) = (frame.texture.width(), frame.texture.height());
                    let pad = read_texture_rgba16f(&gpu, &frame.texture, pw, ph);
                    for y in 0..ph {
                        for x in 0..pw {
                            if x >= w || y >= h {
                                assert_eq!(pad[(y * pw + x) as usize], [0.0; 4]);
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_perspective_normalized_draft_and_cache() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let rgba = [-0.25, 1.5, 0.125, 0.5];
    for (i, corners) in perspective_quads().into_iter().enumerate() {
        let graph = perspective_graph(corners, rgba, 4_400_000 + 8 * i as u128, Some((16, 12)));
        for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
            let expected = perspective_reference(w, h, rgba, corners);
            let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
            let frame = evaluator
                .evaluate_with_tap_frame(&graph, (w, h), &mut NullFrameSource, None)
                .0
                .unwrap();
            for (i, p) in read_texture_rgba16f(&gpu, &frame.texture, w, h)
                .iter()
                .enumerate()
            {
                for c in 0..4 {
                    assert!((cpu.pixels[i][c] as f64 - expected[i][c]).abs() < 0.0003);
                    assert!(
                        (p[c] as f64 - expected[i][c]).abs()
                            < 0.007 * expected[i][c].abs().max(1.0),
                        "{corners:?} {w}x{h} {i}/{c}: {p:?} vs{:?}",
                        expected[i]
                    );
                }
            }
        }
    }
}
#[test]
fn native_perspective_refuses_bad_quads_domain_and_arity() {
    for c in [
        [0.0; 8],
        [0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0, 0.0, 0.2, 0.2, 0.0, 1.0],
        [-1.001, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
        [f32::NAN, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
        [0.0, 0.0, 0.00000001, 0.0, 0.00000001, 1.0, 0.0, 1.0],
    ] {
        assert!(
            photonic_video::graph::ops::native_perspective_inverse(c).is_none(),
            "{c:?}"
        );
    }
    let mut graph = perspective_graph(
        perspective_quads()[0],
        [0.1, 0.2, 0.3, 1.0],
        4_500_000,
        None,
    );
    for inverse in [[0.0; 9], [f32::NAN; 9], [1e7; 9]] {
        graph.nodes[2].op = IrOp::NativePerspective { inverse };
        assert!(graph.validate_working_color_domain().is_err());
    }
    graph.nodes[2].op = IrOp::NativePerspective {
        inverse: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
    };
    graph.nodes[1].op = IrOp::NativeAcescct {
        direction: photonic_render::native_transfer::AcescctDirection::Encode,
    };
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    graph.nodes[2].inputs.clear();
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(graph.validate_working_color_domain().is_err());
}

fn shadow_graph(
    rgba: [f32; 4],
    offset: [f32; 2],
    radius: u32,
    color: [f32; 3],
    opacity: f32,
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut graph = directional_graph(rgba, [0.0; 2], hash, canvas);
    graph.nodes[2].op = IrOp::NativeDropShadow {
        offset,
        radius,
        color,
        opacity,
        pixel_scale: [1.0; 2],
    };
    graph
}
/// Binary64 direct separable sums, independent of the production sliding window.
fn shadow_reference(
    w: u32,
    h: u32,
    rgba: [f32; 4],
    offset: [f64; 2],
    radius: u32,
    color: [f64; 3],
    opacity: f64,
    scale: [f64; 2],
) -> Vec<[f64; 4]> {
    let source = |x: u32, y: u32| {
        if (x as f64 + 0.5) >= w as f64 * 0.25
            && (x as f64 + 0.5) < w as f64 * 0.875
            && (y as f64 + 0.5) >= h as f64 * 0.25
            && (y as f64 + 0.5) < h as f64 * 0.75
        {
            rgba.map(f64::from)
        } else {
            [0.0; 4]
        }
    };
    let sample = |px: f64, py: f64| {
        let x = (px - 0.5).clamp(0.0, w as f64 - 1.0);
        let y = (py - 0.5).clamp(0.0, h as f64 - 1.0);
        let x0 = x.floor() as u32;
        let y0 = y.floor() as u32;
        let fx = x - x0 as f64;
        let fy = y - y0 as f64;
        let mut a = 0.0;
        for (xx, yy, weight) in [
            (x0, y0, (1.0 - fx) * (1.0 - fy)),
            ((x0 + 1).min(w - 1), y0, fx * (1.0 - fy)),
            (x0, (y0 + 1).min(h - 1), (1.0 - fx) * fy),
            ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1), fx * fy),
        ] {
            a += source(xx, yy)[3] * weight;
        }
        a
    };
    let mut a = Vec::new();
    for y in 0..h {
        for x in 0..w {
            a.push(sample(
                x as f64 + 0.5 - offset[0] * scale[0],
                y as f64 + 0.5 - offset[1] * scale[1],
            ));
        }
    }
    for (axis, r) in scale
        .map(|v| (radius as f64 * v).round().clamp(0.0, 500.0) as i64)
        .into_iter()
        .enumerate()
    {
        let mut b = vec![0.0; a.len()];
        for y in 0..h {
            for x in 0..w {
                let mut sum = 0.0;
                for d in -r..=r {
                    let xx = if axis == 0 {
                        (x as i64 + d).clamp(0, w as i64 - 1) as u32
                    } else {
                        x
                    };
                    let yy = if axis == 1 {
                        (y as i64 + d).clamp(0, h as i64 - 1) as u32
                    } else {
                        y
                    };
                    sum += a[(yy * w + xx) as usize];
                }
                b[(y * w + x) as usize] = sum / (2 * r + 1) as f64;
            }
        }
        a = b;
    }
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let c = source(x, y);
            let aa = a[(y * w + x) as usize] * opacity * (1.0 - c[3]);
            out.push([
                c[0] + color[0] * aa,
                c[1] + color[1] * aa,
                c[2] + color[2] * aa,
                c[3] + aa,
            ]);
        }
    }
    out
}
#[test]
fn native_drop_shadow_matches_independent_alpha_blur_composite_reference() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 4_600_000;
    let cases = [
        ([0.0, 0.0], 0, 0.0),
        ([0.0, 0.0], 0, 1.0),
        ([1.25, -0.75], 0, 0.7),
        ([-1.5, 2.25], 1, 0.5),
        ([4.0, 3.0], 2, 1.0),
        ([-4.0, -3.0], 8, 0.6),
        ([200.0, -200.0], 64, 1.0),
        ([-200.0, 200.0], 64, 0.5),
        ([0.25, 0.5], 64, 1.0),
    ];
    for (w, h) in [(1, 1), (1, 7), (7, 1), (7, 5), (16, 12), (257, 3)] {
        for alpha in [0.0f32, 0.25, 1.0] {
            for (offset, radius, opacity) in cases {
                hash += 8;
                let rgba = [-0.5 * alpha, 3.0 * alpha, 0.25 * alpha, alpha];
                let color = [0.7, 0.1, 0.35];
                let graph = shadow_graph(rgba, offset, radius, color, opacity, hash, None);
                assert!(graph.validate_working_color_domain().is_ok());
                let expected = shadow_reference(
                    w,
                    h,
                    rgba,
                    offset.map(f64::from),
                    radius,
                    color.map(f64::from),
                    opacity as f64,
                    [1.0; 2],
                );
                let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
                let frame = evaluator
                    .evaluate_with_tap_frame(&graph, (w, h), &mut NullFrameSource, None)
                    .0
                    .unwrap();
                let pixels = read_texture_rgba16f(&gpu, &frame.texture, w, h);
                for (i, (c, g)) in cpu.pixels.iter().zip(&pixels).enumerate() {
                    for k in 0..4 {
                        assert!(
                            (c[k] as f64 - expected[i][k]).abs() < 0.0002,
                            "CPU {w}x{h} {offset:?} r{radius} {i}/{k}: {c:?} vs{:?}",
                            expected[i]
                        );
                        assert!(
                            (g[k] as f64 - expected[i][k]).abs()
                                < 0.007 * expected[i][k].abs().max(1.0),
                            "GPU {w}x{h} {offset:?} r{radius} {i}/{k}: {g:?} vs{:?}",
                            expected[i]
                        );
                    }
                }
                if alpha == 1.0 && radius == 0 {
                    let (pw, ph) = (frame.texture.width(), frame.texture.height());
                    let pad = read_texture_rgba16f(&gpu, &frame.texture, pw, ph);
                    for y in 0..ph {
                        for x in 0..pw {
                            if x >= w || y >= h {
                                assert_eq!(pad[(y * pw + x) as usize], [0.0; 4]);
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_drop_shadow_draft_cache_and_bounded_radius() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let rgba = [-0.25, 1.5, 0.125, 0.5];
    let offset = [3.25, -2.5];
    let color = [0.3, 0.6, 0.2];
    let graph = shadow_graph(rgba, offset, 5, color, 0.7, 4_700_000, Some((16, 12)));
    for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
        let expected = shadow_reference(
            w,
            h,
            rgba,
            offset.map(f64::from),
            5,
            color.map(f64::from),
            0.7,
            [w as f64 / 16.0, h as f64 / 12.0],
        );
        let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
        let frame = evaluator
            .evaluate_with_tap_frame(&graph, (w, h), &mut NullFrameSource, None)
            .0
            .unwrap();
        for (i, p) in read_texture_rgba16f(&gpu, &frame.texture, w, h)
            .iter()
            .enumerate()
        {
            for c in 0..4 {
                assert!((cpu.pixels[i][c] as f64 - expected[i][c]).abs() < 0.0002);
                assert!(
                    (p[c] as f64 - expected[i][c]).abs() < 0.007 * expected[i][c].abs().max(1.0)
                );
            }
        }
    }
    assert_eq!(
        photonic_video::graph::ops::native_shadow_radius(64, [100.0, 0.5]),
        [500, 32]
    );
}
#[test]
fn native_drop_shadow_refuses_bad_parameters_domain_and_arity() {
    let mut graph = shadow_graph(
        [0.1, 0.2, 0.3, 1.0],
        [0.0; 2],
        0,
        [0.0; 3],
        0.5,
        4_800_000,
        None,
    );
    for (offset, radius, color, opacity, scale) in [
        ([200.1, 0.0], 0, [0.0; 3], 0.5, [1.0; 2]),
        ([0.0; 2], 65, [0.0; 3], 0.5, [1.0; 2]),
        ([0.0; 2], 0, [-0.1, 0.0, 0.0], 0.5, [1.0; 2]),
        ([0.0; 2], 0, [0.0; 3], 1.01, [1.0; 2]),
        ([0.0; 2], 0, [0.0; 3], f32::NAN, [1.0; 2]),
        ([0.0; 2], 0, [0.0; 3], 0.5, [0.0, 1.0]),
    ] {
        graph.nodes[2].op = IrOp::NativeDropShadow {
            offset,
            radius,
            color,
            opacity,
            pixel_scale: scale,
        };
        assert!(graph.validate_working_color_domain().is_err());
    }
    graph.nodes[2].op = IrOp::NativeDropShadow {
        offset: [0.0; 2],
        radius: 0,
        color: [0.0; 3],
        opacity: 0.5,
        pixel_scale: [1.0; 2],
    };
    graph.nodes[1].op = IrOp::NativeAcescct {
        direction: photonic_render::native_transfer::AcescctDirection::Encode,
    };
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    graph.nodes[2].inputs.clear();
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(graph.validate_working_color_domain().is_err());
}

/// Independent NumPy-derived matrices from sRGB/AP1 chromaticities and Bradford.
fn transfer_artistic_reference(p: [f32; 4], posterize: bool, parameter: f64) -> [f64; 4] {
    if p[3] <= 0.0 {
        return [0.0; 4];
    }
    let to_srgb = [
        [1.705050992657983, -0.6217921206570056, -0.08325887200097853],
        [
            -0.13025641750704342,
            1.1408047365754022,
            -0.010548319068358056,
        ],
        [
            -0.024003356804618032,
            -0.12896897606497063,
            1.1529723328695884,
        ],
    ];
    let to_ap1 = [
        [0.6130974024011878, 0.33952314618410606, 0.04737945141470665],
        [
            0.07019372246958168,
            0.9163538790573436,
            0.013452398473074122,
        ],
        [
            0.020615592882226925,
            0.10956977293813544,
            0.8698146341796377,
        ],
    ];
    let straight = [
        p[0] as f64 / p[3] as f64,
        p[1] as f64 / p[3] as f64,
        p[2] as f64 / p[3] as f64,
    ];
    let linear = if straight[0] == straight[1] && straight[1] == straight[2] {
        straight
    } else {
        to_srgb.map(|row| row[0] * straight[0] + row[1] * straight[1] + row[2] * straight[2])
    };
    let encoded = linear.map(|v| {
        let v = v.clamp(0.0, 1.0);
        if v == 1.0 {
            1.0
        } else if v <= 0.0031308 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    });
    let encoded = if posterize {
        encoded.map(|v| (v * (parameter - 1.0) + 0.5).floor() / (parameter - 1.0))
    } else {
        let y = if encoded[0] == encoded[1] && encoded[1] == encoded[2] {
            encoded[0]
        } else {
            encoded[0] * 0.299 + encoded[1] * 0.587 + encoded[2] * 0.114
        };
        [if y >= parameter { 1.0 } else { 0.0 }; 3]
    };
    let decoded = encoded.map(|v| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    });
    let rgb = to_ap1.map(|row| row[0] * decoded[0] + row[1] * decoded[1] + row[2] * decoded[2]);
    [
        rgb[0] * p[3] as f64,
        rgb[1] * p[3] as f64,
        rgb[2] * p[3] as f64,
        p[3] as f64,
    ]
}
fn transfer_artistic_graph(
    rgba: [f32; 4],
    posterize: bool,
    parameter: f32,
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    use photonic_video::graph::ir::NativeTransferArtisticKind::*;
    let mut graph = directional_graph(rgba, [0.0; 2], hash, canvas);
    graph.nodes[2].op = IrOp::NativeTransferArtistic {
        kind: if posterize { Posterize } else { Threshold },
        parameter,
    };
    graph
}
#[test]
fn native_transfer_artistic_independent_reference_and_representable_input() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 4_900_000;
    let cases = [
        (true, 2.0),
        (true, 3.0),
        (true, 4.0),
        (true, 8.0),
        (true, 16.0),
        (true, 255.0),
        (false, 0.0),
        (false, 0.1),
        (false, 0.25),
        (false, 0.5),
        (false, 0.75),
        (false, 1.0),
    ];
    for rgb in [
        [0.0; 3],
        [0.18; 3],
        [0.5; 3],
        [1.0; 3],
        [-0.5, 3.0, 0.25],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.7, 0.1, 0.4],
    ] {
        for alpha in [0.0f32, 0.25, 0.5, 1.0] {
            for (posterize, parameter) in cases {
                hash += 8;
                let rgba = [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha];
                let graph = transfer_artistic_graph(rgba, posterize, parameter, hash, None);
                assert!(graph.validate_working_color_domain().is_ok());
                let cpu = eval_cpu::evaluate(&graph, (7, 5), &mut EmptyProvider);
                let mut source_graph = graph.clone();
                source_graph.output = Some(IrNodeId(1));
                let cpu_source = eval_cpu::evaluate(&source_graph, (7, 5), &mut EmptyProvider);
                let (out, tap) = evaluator.evaluate_with_tap_frame(
                    &graph,
                    (7, 5),
                    &mut NullFrameSource,
                    Some(IrNodeId(1)),
                );
                let out = out.unwrap();
                let tap = tap.unwrap();
                let pixels = read_texture_rgba16f(&gpu, &out.texture, 7, 5);
                let gpu_source = read_texture_rgba16f(&gpu, &tap.texture, 7, 5);
                for i in 0..35 {
                    let expected_cpu = transfer_artistic_reference(
                        cpu_source.pixels[i],
                        posterize,
                        parameter as f64,
                    );
                    let expected_gpu =
                        transfer_artistic_reference(gpu_source[i], posterize, parameter as f64);
                    for c in 0..4 {
                        assert!(
                            (cpu.pixels[i][c] as f64 - expected_cpu[c]).abs() < 0.00001,
                            "CPU {posterize}/{parameter} {rgb:?}/{alpha}: {:?} vs{expected_cpu:?}",
                            cpu.pixels[i]
                        );
                        assert!((pixels[i][c]as f64-expected_gpu[c]).abs()<0.003,"GPU {posterize}/{parameter} {rgb:?}/{alpha}: {:?} vs{expected_gpu:?}, input{:?}",pixels[i],gpu_source[i]);
                    }
                }
                if alpha == 1.0 && parameter == 1.0 {
                    let (pw, ph) = (out.texture.width(), out.texture.height());
                    let pad = read_texture_rgba16f(&gpu, &out.texture, pw, ph);
                    for y in 0..ph {
                        for x in 0..pw {
                            if x >= 7 || y >= 5 {
                                assert_eq!(pad[(y * pw + x) as usize], [0.0; 4]);
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_transfer_artistic_draft_cache_and_discrete_threshold_neighbors() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    for (index, (posterize, parameter)) in [(true, 4.0), (false, 0.5)].into_iter().enumerate() {
        let graph = transfer_artistic_graph(
            [0.14, 0.02, 0.08, 0.2],
            posterize,
            parameter,
            5_000_000 + index as u128 * 8,
            Some((16, 12)),
        );
        for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
            let (out, tap) = evaluator.evaluate_with_tap_frame(
                &graph,
                (w, h),
                &mut NullFrameSource,
                Some(IrNodeId(1)),
            );
            let out = out.unwrap();
            let tap = tap.unwrap();
            let pixels = read_texture_rgba16f(&gpu, &out.texture, w, h);
            let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
            for (i, p) in pixels.iter().enumerate() {
                let expected = transfer_artistic_reference(input[i], posterize, parameter as f64);
                for c in 0..4 {
                    assert!((p[c] as f64 - expected[c]).abs() < 0.003);
                }
            }
        }
    }
    // Adjacent exact binary16 neutral inputs straddling encoded sRGB 0.5. The
    // oracle uses their represented light values, never an unrepresentable ideal.
    for (i, linear) in [0.2139892578125f32, 0.214111328125].into_iter().enumerate() {
        let graph = transfer_artistic_graph(
            [linear, linear, linear, 1.0],
            false,
            0.5,
            5_000_100 + i as u128 * 8,
            None,
        );
        let out = evaluator
            .evaluate_with_tap_frame(&graph, (7, 5), &mut NullFrameSource, None)
            .0
            .unwrap();
        let pixels = read_texture_rgba16f(&gpu, &out.texture, 7, 5);
        let center = pixels[2 * 7 + 3];
        assert!(
            (center[0] - i as f32).abs() < 0.001,
            "threshold neighbor {linear}: {center:?}"
        );
    }
}
#[test]
fn native_transfer_artistic_refuses_bad_parameters_domain_and_arity() {
    use photonic_video::graph::ir::NativeTransferArtisticKind::*;
    let mut graph = transfer_artistic_graph([0.1, 0.2, 0.3, 1.0], true, 4.0, 5_100_000, None);
    for (kind, parameter) in [
        (Posterize, 1.0),
        (Posterize, 256.0),
        (Posterize, 3.5),
        (Threshold, -0.01),
        (Threshold, 1.01),
        (Threshold, f32::NAN),
        (Posterize, f32::INFINITY),
    ] {
        graph.nodes[2].op = IrOp::NativeTransferArtistic { kind, parameter };
        assert!(graph.validate_working_color_domain().is_err());
    }
    graph.nodes[2].op = IrOp::NativeTransferArtistic {
        kind: Posterize,
        parameter: 4.0,
    };
    graph.nodes[1].op = IrOp::NativeAcescct {
        direction: photonic_render::native_transfer::AcescctDirection::Encode,
    };
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    graph.nodes[2].inputs.clear();
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(graph.validate_working_color_domain().is_err());
}

fn desaturate_reference(p: [f32; 4]) -> [f64; 4] {
    if p[3] <= 0.0 {
        return [0.0; 4];
    }
    if p[0] == p[1] && p[1] == p[2] {
        return p.map(f64::from);
    }
    let a = [
        [1.705050992657983, -0.6217921206570056, -0.08325887200097853],
        [
            -0.13025641750704342,
            1.1408047365754022,
            -0.010548319068358056,
        ],
        [
            -0.024003356804618032,
            -0.12896897606497063,
            1.1529723328695884,
        ],
    ];
    let b = [
        [0.6130974024011878, 0.33952314618410606, 0.04737945141470665],
        [
            0.07019372246958168,
            0.9163538790573436,
            0.013452398473074122,
        ],
        [
            0.020615592882226925,
            0.10956977293813544,
            0.8698146341796377,
        ],
    ];
    let rgb = [
        p[0] as f64 / p[3] as f64,
        p[1] as f64 / p[3] as f64,
        p[2] as f64 / p[3] as f64,
    ];
    let rgb = a
        .map(|r| r[0] * rgb[0] + r[1] * rgb[1] + r[2] * rgb[2])
        .map(|v| {
            let x = v.abs();
            if x <= 0.0031308 {
                v * 12.92
            } else {
                v.signum() * (1.055 * x.powf(1.0 / 2.4) - 0.055)
            }
        });
    let g = (rgb.iter().copied().fold(f64::NEG_INFINITY, f64::max)
        + rgb.iter().copied().fold(f64::INFINITY, f64::min))
        / 2.0;
    let light = if g.abs() <= 0.04045 {
        g / 12.92
    } else {
        g.signum() * ((g.abs() + 0.055) / 1.055).powf(2.4)
    };
    let rgb = b.map(|r| (r[0] + r[1] + r[2]) * light * p[3] as f64);
    [rgb[0], rgb[1], rgb[2], p[3] as f64]
}
fn desaturate_graph(rgba: [f32; 4], hash: u128, canvas: Option<(u32, u32)>) -> FrameGraph {
    let mut graph = directional_graph(rgba, [0.0; 2], hash, canvas);
    graph.nodes[2].op = IrOp::NativeTransferArtistic {
        kind: photonic_video::graph::ir::NativeTransferArtisticKind::Desaturate,
        parameter: 0.0,
    };
    graph
}
#[test]
fn native_desaturate_preserves_extended_encoded_lightness_and_coverage() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 5_200_000;
    for rgb in [
        [-0.5, 3.0, 0.25],
        [0.5, -0.2, 1.5],
        [-2.0, -1.0, -4.0],
        [8.0, 16.0, 4.0],
        [0.7, 0.1, 0.4],
        [0.18; 3],
        [1.0; 3],
        [0.0; 3],
    ] {
        for alpha in [0.0f32, 0.25, 0.5, 1.0] {
            for (w, h) in [(1, 1), (1, 7), (7, 1), (7, 5), (16, 12), (257, 3)] {
                hash += 8;
                let rgba = [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha];
                let graph = desaturate_graph(rgba, hash, None);
                assert!(graph.validate_working_color_domain().is_ok());
                let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
                let mut srcgraph = graph.clone();
                srcgraph.output = Some(IrNodeId(1));
                let cpu_src = eval_cpu::evaluate(&srcgraph, (w, h), &mut EmptyProvider);
                let (out, tap) = evaluator.evaluate_with_tap_frame(
                    &graph,
                    (w, h),
                    &mut NullFrameSource,
                    Some(IrNodeId(1)),
                );
                let out = out.unwrap();
                let tap = tap.unwrap();
                let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
                let pixels = read_texture_rgba16f(&gpu, &out.texture, w, h);
                for (i, p) in pixels.iter().enumerate() {
                    let expected = desaturate_reference(input[i]);
                    let cpu_expected = desaturate_reference(cpu_src.pixels[i]);
                    for c in 0..4 {
                        assert!(
                            (cpu.pixels[i][c] as f64 - cpu_expected[c]).abs()
                                < 0.00002 * cpu_expected[c].abs().max(1.0)
                        );
                        assert!(
                            (p[c] as f64 - expected[c]).abs() < 0.006 * expected[c].abs().max(1.0),
                            "{rgb:?}/{alpha} {w}x{h} {i}/{c}: {p:?} vs{expected:?}"
                        );
                    }
                }
                if alpha == 1.0 {
                    let (pw, ph) = (out.texture.width(), out.texture.height());
                    let pad = read_texture_rgba16f(&gpu, &out.texture, pw, ph);
                    for y in 0..ph {
                        for x in 0..pw {
                            if x >= w || y >= h {
                                assert_eq!(pad[(y * pw + x) as usize], [0.0; 4]);
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_desaturate_draft_and_cache_keep_extended_domain() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let graph = desaturate_graph([-0.25, 1.5, 0.125, 0.5], 5_300_000, Some((16, 12)));
    for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
        let (out, tap) = evaluator.evaluate_with_tap_frame(
            &graph,
            (w, h),
            &mut NullFrameSource,
            Some(IrNodeId(1)),
        );
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
        for (i, p) in read_texture_rgba16f(&gpu, &out.texture, w, h)
            .iter()
            .enumerate()
        {
            let expected = desaturate_reference(input[i]);
            for c in 0..4 {
                assert!((p[c] as f64 - expected[c]).abs() < 0.006 * expected[c].abs().max(1.0));
            }
        }
    }
}
#[test]
fn native_desaturate_refuses_fake_parameters_domain_and_arity() {
    use photonic_video::graph::ir::NativeTransferArtisticKind::Desaturate;
    let mut graph = desaturate_graph([0.1, 0.2, 0.3, 1.0], 5_400_000, None);
    for parameter in [1.0, -1.0, f32::NAN] {
        graph.nodes[2].op = IrOp::NativeTransferArtistic {
            kind: Desaturate,
            parameter,
        };
        assert!(graph.validate_working_color_domain().is_err());
    }
    graph.nodes[2].op = IrOp::NativeTransferArtistic {
        kind: Desaturate,
        parameter: 0.0,
    };
    graph.nodes[1].op = IrOp::NativeAcescct {
        direction: photonic_render::native_transfer::AcescctDirection::Encode,
    };
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    graph.nodes[2].inputs.clear();
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(graph.validate_working_color_domain().is_err());
}

fn transfer_matrix_reference(p: [f32; 4], matrix: [[f32; 3]; 3]) -> [f64; 4] {
    if p[3] <= 0.0 {
        return [0.0; 4];
    }
    if matrix == [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
        return p.map(f64::from);
    }
    let a = [
        [1.705050992657983, -0.6217921206570056, -0.08325887200097853],
        [
            -0.13025641750704342,
            1.1408047365754022,
            -0.010548319068358056,
        ],
        [
            -0.024003356804618032,
            -0.12896897606497063,
            1.1529723328695884,
        ],
    ];
    let b = [
        [0.6130974024011878, 0.33952314618410606, 0.04737945141470665],
        [
            0.07019372246958168,
            0.9163538790573436,
            0.013452398473074122,
        ],
        [
            0.020615592882226925,
            0.10956977293813544,
            0.8698146341796377,
        ],
    ];
    let rgb = [
        p[0] as f64 / p[3] as f64,
        p[1] as f64 / p[3] as f64,
        p[2] as f64 / p[3] as f64,
    ];
    let rgb = a
        .map(|r| r[0] * rgb[0] + r[1] * rgb[1] + r[2] * rgb[2])
        .map(|v| {
            let x = v.abs();
            if x <= 0.0031308 {
                v * 12.92
            } else {
                v.signum() * (1.055 * x.powf(1.0 / 2.4) - 0.055)
            }
        });
    let rgb = matrix
        .map(|r| r[0] as f64 * rgb[0] + r[1] as f64 * rgb[1] + r[2] as f64 * rgb[2])
        .map(|v| {
            if v.abs() <= 0.04045 {
                v / 12.92
            } else {
                v.signum() * ((v.abs() + 0.055) / 1.055).powf(2.4)
            }
        });
    let rgb = b.map(|r| (r[0] * rgb[0] + r[1] * rgb[1] + r[2] * rgb[2]) * p[3] as f64);
    [rgb[0], rgb[1], rgb[2], p[3] as f64]
}
fn transfer_matrix_graph(
    rgba: [f32; 4],
    matrix: [[f32; 3]; 3],
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut graph = directional_graph(rgba, [0.0; 2], hash, canvas);
    graph.nodes[2].op = IrOp::NativeTransferMatrix { matrix };
    graph
}
#[test]
fn native_transfer_matrix_independent_signed_hdr_alpha_reference() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 5_500_000;
    let matrices = [
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        [[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        [[2.0, -1.0, 0.5], [0.25, 0.5, 0.25], [-1.0, 0.0, 2.0]],
        [[0.299, 0.587, 0.114]; 3],
        [[1.0 / 3.0; 3]; 3],
        [[0.0; 3]; 3],
    ];
    for matrix in matrices {
        for rgb in [
            [-0.5, 3.0, 0.25],
            [0.5, -0.2, 1.5],
            [-2.0, -1.0, -4.0],
            [8.0, 16.0, 4.0],
            [0.7, 0.1, 0.4],
            [0.18; 3],
        ] {
            for alpha in [0.0f32, 0.25, 1.0] {
                for (w, h) in [(1, 1), (7, 5), (16, 12)] {
                    hash += 8;
                    let rgba = [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha];
                    let graph = transfer_matrix_graph(rgba, matrix, hash, None);
                    assert!(graph.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
                    let mut srcgraph = graph.clone();
                    srcgraph.output = Some(IrNodeId(1));
                    let cpu_src = eval_cpu::evaluate(&srcgraph, (w, h), &mut EmptyProvider);
                    let (out, tap) = evaluator.evaluate_with_tap_frame(
                        &graph,
                        (w, h),
                        &mut NullFrameSource,
                        Some(IrNodeId(1)),
                    );
                    let out = out.unwrap();
                    let tap = tap.unwrap();
                    let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
                    let pixels = read_texture_rgba16f(&gpu, &out.texture, w, h);
                    for (i, p) in pixels.iter().enumerate() {
                        let expected = transfer_matrix_reference(input[i], matrix);
                        let cpu_expected = transfer_matrix_reference(cpu_src.pixels[i], matrix);
                        for c in 0..4 {
                            assert!(
                                (cpu.pixels[i][c] as f64 - cpu_expected[c]).abs()
                                    < 0.00003 * cpu_expected[c].abs().max(1.0)
                            );
                            assert!(
                                (p[c] as f64 - expected[c]).abs()
                                    < 0.008 * expected[c].abs().max(1.0),
                                "{matrix:?}/{rgb:?}/{alpha} {w}x{h} {i}/{c}: {p:?} vs{expected:?}"
                            );
                        }
                    }
                    if alpha == 1.0 {
                        let (pw, ph) = (out.texture.width(), out.texture.height());
                        let pad = read_texture_rgba16f(&gpu, &out.texture, pw, ph);
                        for y in 0..ph {
                            for x in 0..pw {
                                if x >= w || y >= h {
                                    assert_eq!(pad[(y * pw + x) as usize], [0.0; 4]);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_transfer_matrix_draft_cache_and_identity() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let matrix = [[2.0, -1.0, 0.5], [0.0, 1.0, 0.0], [-1.0, 0.0, 2.0]];
    let graph = transfer_matrix_graph([-0.25, 1.5, 0.125, 0.5], matrix, 5_600_000, Some((16, 12)));
    for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
        let (out, tap) = evaluator.evaluate_with_tap_frame(
            &graph,
            (w, h),
            &mut NullFrameSource,
            Some(IrNodeId(1)),
        );
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
        for (i, p) in read_texture_rgba16f(&gpu, &out.texture, w, h)
            .iter()
            .enumerate()
        {
            let expected = transfer_matrix_reference(input[i], matrix);
            for c in 0..4 {
                assert!((p[c] as f64 - expected[c]).abs() < 0.008 * expected[c].abs().max(1.0));
            }
        }
    }
}
#[test]
fn native_transfer_matrix_refuses_domain_arity_and_bad_coefficients() {
    let matrix = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let mut graph = transfer_matrix_graph([0.1, 0.2, 0.3, 1.0], matrix, 5_700_000, None);
    for value in [2.01, -2.01, f32::NAN, f32::INFINITY] {
        let mut m = matrix;
        m[1][2] = value;
        graph.nodes[2].op = IrOp::NativeTransferMatrix { matrix: m };
        assert!(graph.validate_working_color_domain().is_err());
    }
    graph.nodes[2].op = IrOp::NativeTransferMatrix { matrix };
    graph.nodes[1].op = IrOp::NativeAcescct {
        direction: photonic_render::native_transfer::AcescctDirection::Encode,
    };
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    graph.nodes[2].inputs.clear();
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(graph.validate_working_color_domain().is_err());
}
fn photo_filter_reference(p: [f32; 4], color: [f32; 3], density: f32, preserve: bool) -> [f64; 4] {
    if p[3] <= 0.0 {
        return [0.0; 4];
    }
    if density == 0.0 || color == [1.0; 3] {
        return p.map(f64::from);
    }
    let a = [
        [1.705050992657983, -0.6217921206570056, -0.08325887200097853],
        [
            -0.13025641750704342,
            1.1408047365754022,
            -0.010548319068358056,
        ],
        [
            -0.024003356804618032,
            -0.12896897606497063,
            1.1529723328695884,
        ],
    ];
    let b = [
        [0.6130974024011878, 0.33952314618410606, 0.04737945141470665],
        [
            0.07019372246958168,
            0.9163538790573436,
            0.013452398473074122,
        ],
        [
            0.020615592882226925,
            0.10956977293813544,
            0.8698146341796377,
        ],
    ];
    let rgb = [
        p[0] as f64 / p[3] as f64,
        p[1] as f64 / p[3] as f64,
        p[2] as f64 / p[3] as f64,
    ];
    let rgb = a
        .map(|r| r[0] * rgb[0] + r[1] * rgb[1] + r[2] * rgb[2])
        .map(|v| {
            let x = v.abs();
            if x <= 0.0031308 {
                v * 12.92
            } else {
                v.signum() * (1.055 * x.powf(1.0 / 2.4) - 0.055)
            }
        });
    let mut filtered = [0.0; 3];
    for i in 0..3 {
        filtered[i] = rgb[i] * (1.0 - density as f64) + rgb[i] * color[i] as f64 * density as f64;
    }
    if preserve {
        let before = 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2];
        let after = 0.299 * filtered[0] + 0.587 * filtered[1] + 0.114 * filtered[2];
        if after > 0.0001 {
            for c in &mut filtered {
                *c *= before / after;
            }
        }
    }
    let rgb = filtered.map(|v| {
        if v.abs() <= 0.04045 {
            v / 12.92
        } else {
            v.signum() * ((v.abs() + 0.055) / 1.055).powf(2.4)
        }
    });
    let rgb = b.map(|r| (r[0] * rgb[0] + r[1] * rgb[1] + r[2] * rgb[2]) * p[3] as f64);
    [rgb[0], rgb[1], rgb[2], p[3] as f64]
}

fn photo_filter_graph(
    rgba: [f32; 4],
    color: [f32; 3],
    density: f32,
    preserve: bool,
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut graph = directional_graph(rgba, [0.0; 2], hash, canvas);
    graph.nodes[2].op = IrOp::NativePhotoFilter {
        color,
        density,
        preserve,
    };
    graph
}
#[test]
fn native_photo_filter_signed_hdr_alpha_and_independent_reference() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 5_800_000;
    for (color, density, preserve) in [
        ([1.0, 0.5, 0.2], 0.0, false),
        ([1.0; 3], 1.0, true),
        ([0.2, 0.6, 1.0], 0.25, false),
        ([0.2, 0.6, 1.0], 0.75, true),
        ([0.0; 3], 1.0, true),
        ([1.0, 0.0, 0.0], 1.0, true),
        ([1.0, 0.5, 0.2], 0.8, false),
        ([0.0, 0.0, 1.0], 0.5, true),
    ] {
        for rgb in [
            [-0.5, 3.0, 0.25],
            [-2.0, -1.0, -4.0],
            [8.0, 16.0, 4.0],
            [0.7, 0.1, 0.4],
            [0.000001; 3],
            [0.18; 3],
        ] {
            for alpha in [0.0f32, 0.25, 1.0] {
                for (w, h) in [(1, 1), (7, 5), (16, 12)] {
                    hash += 8;
                    let rgba = [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha];
                    let graph = photo_filter_graph(rgba, color, density, preserve, hash, None);
                    assert!(graph.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
                    let mut srcgraph = graph.clone();
                    srcgraph.output = Some(IrNodeId(1));
                    let cpu_src = eval_cpu::evaluate(&srcgraph, (w, h), &mut EmptyProvider);
                    let (out, tap) = evaluator.evaluate_with_tap_frame(
                        &graph,
                        (w, h),
                        &mut NullFrameSource,
                        Some(IrNodeId(1)),
                    );
                    let out = out.unwrap();
                    let tap = tap.unwrap();
                    let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
                    let pixels = read_texture_rgba16f(&gpu, &out.texture, w, h);
                    for (i, p) in pixels.iter().enumerate() {
                        let expected = photo_filter_reference(input[i], color, density, preserve);
                        let cpu_expected =
                            photo_filter_reference(cpu_src.pixels[i], color, density, preserve);
                        for c in 0..4 {
                            assert!(
                                (cpu.pixels[i][c] as f64 - cpu_expected[c]).abs()
                                    < 0.00004 * cpu_expected[c].abs().max(1.0)
                            );
                            assert!((p[c]as f64-expected[c]).abs()<0.008*expected[c].abs().max(1.0),"{color:?}/{density}/{preserve}/{rgb:?}/{alpha} {w}x{h} {i}/{c}: {p:?} vs{expected:?}");
                        }
                    }
                    if alpha == 1.0 {
                        let (pw, ph) = (out.texture.width(), out.texture.height());
                        let pad = read_texture_rgba16f(&gpu, &out.texture, pw, ph);
                        for y in 0..ph {
                            for x in 0..pw {
                                if x >= w || y >= h {
                                    assert_eq!(pad[(y * pw + x) as usize], [0.0; 4]);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_photo_filter_draft_cache_and_identity() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let (color, density, preserve) = ([1.0, 0.5, 0.2], 0.5, true);
    let graph = photo_filter_graph(
        [-0.25, 1.5, 0.125, 0.5],
        color,
        density,
        preserve,
        5_900_000,
        Some((16, 12)),
    );
    for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
        let (out, tap) = evaluator.evaluate_with_tap_frame(
            &graph,
            (w, h),
            &mut NullFrameSource,
            Some(IrNodeId(1)),
        );
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
        for (i, p) in read_texture_rgba16f(&gpu, &out.texture, w, h)
            .iter()
            .enumerate()
        {
            let expected = photo_filter_reference(input[i], color, density, preserve);
            for c in 0..4 {
                assert!((p[c] as f64 - expected[c]).abs() < 0.008 * expected[c].abs().max(1.0));
            }
        }
    }
}
#[test]
fn native_photo_filter_refuses_domain_arity_and_bad_parameters() {
    let mut graph = photo_filter_graph(
        [0.1, 0.2, 0.3, 1.0],
        [1.0, 0.5, 0.2],
        0.5,
        true,
        6_000_000,
        None,
    );
    for value in [1.01, -0.01, f32::NAN, f32::INFINITY] {
        for color_bad in [false, true] {
            graph.nodes[2].op = IrOp::NativePhotoFilter {
                color: if color_bad {
                    [value, 0.5, 0.2]
                } else {
                    [1.0, 0.5, 0.2]
                },
                density: if color_bad { 0.5 } else { value },
                preserve: true,
            };
            assert!(graph.validate_working_color_domain().is_err());
        }
    }
    graph.nodes[2].op = IrOp::NativePhotoFilter {
        color: [1.0, 0.5, 0.2],
        density: 0.5,
        preserve: true,
    };
    graph.nodes[1].op = IrOp::NativeAcescct {
        direction: photonic_render::native_transfer::AcescctDirection::Encode,
    };
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    graph.nodes[2].inputs.clear();
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(graph.validate_working_color_domain().is_err());
}

#[test]
fn native_photo_filter_luminosity_gate_has_qualified_half_float_neighbors() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    for (index, value) in [0.000015437602996826172f32, 0.000015497207641601562f32]
        .into_iter()
        .enumerate()
    {
        let graph = photo_filter_graph(
            [value, value, value, 1.0],
            [0.5; 3],
            1.0,
            true,
            6_010_000 + index as u128 * 8,
            None,
        );
        let (out, tap) = evaluator.evaluate_with_tap_frame(
            &graph,
            (1, 1),
            &mut NullFrameSource,
            Some(IrNodeId(1)),
        );
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, 1, 1)[0];
        let actual = read_texture_rgba16f(&gpu, &out.texture, 1, 1)[0];
        let expected = photo_filter_reference(input, [0.5; 3], 1.0, true);
        for c in 0..3 {
            assert!(
                (actual[c] as f64 - expected[c]).abs() < 0.0000001,
                "neighbor {index}: {actual:?} vs {expected:?}"
            );
        }
        assert_eq!(actual[3], 1.0);
        if index == 0 {
            assert!(actual[0] < input[0] * 0.6);
        } else {
            assert!(actual[0] > input[0] * 0.9);
        }
    }
}
fn color_balance_reference(p: [f32; 4], shifts: [[f32; 3]; 3], preserve: bool) -> [f64; 4] {
    if p[3] <= 0.0 {
        return [0.0; 4];
    }
    if shifts == [[0.0; 3]; 3] {
        return p.map(f64::from);
    }
    let a = [
        [1.705050992657983, -0.6217921206570056, -0.08325887200097853],
        [
            -0.13025641750704342,
            1.1408047365754022,
            -0.010548319068358056,
        ],
        [
            -0.024003356804618032,
            -0.12896897606497063,
            1.1529723328695884,
        ],
    ];
    let b = [
        [0.6130974024011878, 0.33952314618410606, 0.04737945141470665],
        [
            0.07019372246958168,
            0.9163538790573436,
            0.013452398473074122,
        ],
        [
            0.020615592882226925,
            0.10956977293813544,
            0.8698146341796377,
        ],
    ];
    let rgb = [
        p[0] as f64 / p[3] as f64,
        p[1] as f64 / p[3] as f64,
        p[2] as f64 / p[3] as f64,
    ];
    let rgb = a
        .map(|r| r[0] * rgb[0] + r[1] * rgb[1] + r[2] * rgb[2])
        .map(|v| {
            let x = v.abs();
            if x <= 0.0031308 {
                v * 12.92
            } else {
                v.signum() * (1.055 * x.powf(1.0 / 2.4) - 0.055)
            }
        });
    let original = 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2];
    let sw = (1.0 - 2.0 * original).clamp(0.0, 1.0);
    let hw = (2.0 * original - 1.0).clamp(0.0, 1.0);
    let mw = (1.0 - sw - hw).clamp(0.0, 1.0);
    let mut filtered = [0.0; 3];
    for i in 0..3 {
        filtered[i] = rgb[i]
            + 0.5
                * (shifts[0][i] as f64 * sw + shifts[1][i] as f64 * mw + shifts[2][i] as f64 * hw);
    }
    if preserve {
        let after = 0.299 * filtered[0] + 0.587 * filtered[1] + 0.114 * filtered[2];
        if after > 0.0001 {
            for c in &mut filtered {
                *c *= original / after;
            }
        }
    }
    let rgb = filtered.map(|v| {
        if v.abs() <= 0.04045 {
            v / 12.92
        } else {
            v.signum() * ((v.abs() + 0.055) / 1.055).powf(2.4)
        }
    });
    let rgb = b.map(|r| (r[0] * rgb[0] + r[1] * rgb[1] + r[2] * rgb[2]) * p[3] as f64);
    [rgb[0], rgb[1], rgb[2], p[3] as f64]
}

fn color_balance_graph(
    rgba: [f32; 4],
    shifts: [[f32; 3]; 3],
    preserve: bool,
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut graph = directional_graph(rgba, [0.0; 2], hash, canvas);
    graph.nodes[2].op = IrOp::NativeColorBalance { shifts, preserve };
    graph
}
#[test]
fn native_color_balance_signed_hdr_alpha_and_independent_reference() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 6_100_000;
    for (shifts, preserve) in [
        ([[0.0; 3]; 3], true),
        (
            [[0.4, -0.2, 0.1], [-0.3, 0.4, 0.2], [0.1, -0.4, 0.3]],
            false,
        ),
        ([[0.4, -0.2, 0.1], [-0.3, 0.4, 0.2], [0.1, -0.4, 0.3]], true),
        ([[1.0, -1.0, 0.0]; 3], false),
        ([[0.5, 0.0, 0.0], [0.0; 3], [0.0; 3]], true),
        ([[0.0; 3], [0.0, 0.5, 0.0], [0.0; 3]], true),
        ([[0.0; 3], [0.0; 3], [0.0, 0.0, 0.5]], true),
        ([[-0.2; 3]; 3], false),
    ] {
        for rgb in [
            [-0.5, 3.0, 0.25],
            [-2.0, -1.0, -4.0],
            [8.0, 16.0, 4.0],
            [0.7, 0.1, 0.4],
            [0.000001; 3],
            [0.18; 3],
        ] {
            for alpha in [0.0f32, 0.25, 1.0] {
                for (w, h) in [(1, 1), (7, 5), (16, 12)] {
                    hash += 8;
                    let rgba = [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha];
                    let graph = color_balance_graph(rgba, shifts, preserve, hash, None);
                    assert!(graph.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
                    let mut srcgraph = graph.clone();
                    srcgraph.output = Some(IrNodeId(1));
                    let cpu_src = eval_cpu::evaluate(&srcgraph, (w, h), &mut EmptyProvider);
                    let (out, tap) = evaluator.evaluate_with_tap_frame(
                        &graph,
                        (w, h),
                        &mut NullFrameSource,
                        Some(IrNodeId(1)),
                    );
                    let out = out.unwrap();
                    let tap = tap.unwrap();
                    let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
                    let pixels = read_texture_rgba16f(&gpu, &out.texture, w, h);
                    for (i, p) in pixels.iter().enumerate() {
                        let expected = color_balance_reference(input[i], shifts, preserve);
                        let cpu_expected =
                            color_balance_reference(cpu_src.pixels[i], shifts, preserve);
                        for c in 0..4 {
                            assert!(
                                (cpu.pixels[i][c] as f64 - cpu_expected[c]).abs()
                                    < 0.00004 * cpu_expected[c].abs().max(1.0)
                            );
                            assert!((p[c]as f64-expected[c]).abs()<0.008*expected[c].abs().max(1.0),"{shifts:?}/{preserve}/{rgb:?}/{alpha} {w}x{h} {i}/{c}: {p:?} vs{expected:?}");
                        }
                    }
                    if alpha == 1.0 {
                        let (pw, ph) = (out.texture.width(), out.texture.height());
                        let pad = read_texture_rgba16f(&gpu, &out.texture, pw, ph);
                        for y in 0..ph {
                            for x in 0..pw {
                                if x >= w || y >= h {
                                    assert_eq!(pad[(y * pw + x) as usize], [0.0; 4]);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_color_balance_draft_cache_and_identity() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let shifts = [[0.4, -0.2, 0.1], [-0.3, 0.4, 0.2], [0.1, -0.4, 0.3]];
    let graph = color_balance_graph(
        [-0.25, 1.5, 0.125, 0.5],
        shifts,
        true,
        6_200_000,
        Some((16, 12)),
    );
    for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
        let (out, tap) = evaluator.evaluate_with_tap_frame(
            &graph,
            (w, h),
            &mut NullFrameSource,
            Some(IrNodeId(1)),
        );
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
        for (i, p) in read_texture_rgba16f(&gpu, &out.texture, w, h)
            .iter()
            .enumerate()
        {
            let expected = color_balance_reference(input[i], shifts, true);
            for c in 0..4 {
                assert!((p[c] as f64 - expected[c]).abs() < 0.008 * expected[c].abs().max(1.0));
            }
        }
    }
}
#[test]
fn native_color_balance_refuses_domain_arity_and_bad_parameters() {
    let mut graph = color_balance_graph([0.1, 0.2, 0.3, 1.0], [[0.0; 3]; 3], true, 6_300_000, None);
    for value in [1.01, -1.01, f32::NAN, f32::INFINITY] {
        for i in 0..3 {
            for j in 0..3 {
                let mut shifts = [[0.0; 3]; 3];
                shifts[i][j] = value;
                graph.nodes[2].op = IrOp::NativeColorBalance {
                    shifts,
                    preserve: true,
                };
                assert!(graph.validate_working_color_domain().is_err());
            }
        }
    }
    graph.nodes[2].op = IrOp::NativeColorBalance {
        shifts: [[0.0; 3]; 3],
        preserve: true,
    };
    graph.nodes[1].op = IrOp::NativeAcescct {
        direction: photonic_render::native_transfer::AcescctDirection::Encode,
    };
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    graph.nodes[2].inputs.clear();
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(graph.validate_working_color_domain().is_err());
}

#[test]
fn native_color_balance_luminosity_gate_has_qualified_half_float_neighbors() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let shifts = [[-0.2; 3]; 3];
    for (index, value) in [0.01003265380859375f32, 0.010040283203125f32]
        .into_iter()
        .enumerate()
    {
        let graph = color_balance_graph(
            [value, value, value, 1.0],
            shifts,
            true,
            6_310_000 + index as u128 * 8,
            None,
        );
        let (out, tap) = evaluator.evaluate_with_tap_frame(
            &graph,
            (1, 1),
            &mut NullFrameSource,
            Some(IrNodeId(1)),
        );
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, 1, 1)[0];
        let actual = read_texture_rgba16f(&gpu, &out.texture, 1, 1)[0];
        let expected = color_balance_reference(input, shifts, true);
        for c in 0..3 {
            assert!(
                (actual[c] as f64 - expected[c]).abs() < 0.00003,
                "neighbor {index}: {actual:?} vs {expected:?}"
            );
        }
        assert_eq!(actual[3], 1.0);
        if index == 0 {
            assert!(actual[0] < input[0] * 0.01);
        } else {
            assert!(actual[0] > input[0] * 0.99);
        }
    }
}
fn catalog_curve_reference(p: [f32; 4], knots: [[f32; 2]; 5], contrast: f32) -> [f64; 4] {
    if p[3] <= 0.0 {
        return [0.0; 4];
    }
    let mut points: Vec<_> = knots.map(|k| [k[0] as f64, k[1] as f64]).to_vec();
    if contrast.abs() > 1e-6 {
        points[2][1] = (0.5 + contrast as f64 * 0.25).clamp(0.05, 0.95);
    }
    points.sort_by(|a, b| a[0].total_cmp(&b[0]));
    points.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-6);
    if points.len() < 2 || points.iter().all(|k| k[0] == k[1]) {
        return p.map(f64::from);
    }
    let n = points.len();
    let slopes: Vec<_> = points
        .windows(2)
        .map(|q| (q[1][1] - q[0][1]) / (q[1][0] - q[0][0]))
        .collect();
    let mut tangent = vec![0.0; n];
    tangent[0] = slopes[0];
    tangent[n - 1] = slopes[n - 2];
    for i in 1..n - 1 {
        if slopes[i - 1] * slopes[i] > 0.0 {
            tangent[i] = (slopes[i - 1] + slopes[i]) * 0.5;
        }
    }
    for i in 0..n - 1 {
        if slopes[i].abs() < 1e-12 {
            tangent[i] = 0.0;
            tangent[i + 1] = 0.0;
        } else {
            let a = tangent[i] / slopes[i];
            let b = tangent[i + 1] / slopes[i];
            let norm = a * a + b * b;
            if norm > 9.0 {
                let t = 3.0 / norm.sqrt();
                tangent[i] = t * a * slopes[i];
                tangent[i + 1] = t * b * slopes[i];
            }
        }
    }
    let sample = |x: f64| {
        if x <= points[0][0] {
            return points[0][1] + (x - points[0][0]) * tangent[0];
        }
        if x >= points[n - 1][0] {
            return points[n - 1][1] + (x - points[n - 1][0]) * tangent[n - 1];
        }
        let i = points
            .windows(2)
            .position(|q| x >= q[0][0] && x <= q[1][0])
            .unwrap();
        let h = points[i + 1][0] - points[i][0];
        let t = (x - points[i][0]) / h;
        let t2 = t * t;
        let t3 = t2 * t;
        (2.0 * t3 - 3.0 * t2 + 1.0) * points[i][1]
            + (t3 - 2.0 * t2 + t) * h * tangent[i]
            + (-2.0 * t3 + 3.0 * t2) * points[i + 1][1]
            + (t3 - t2) * h * tangent[i + 1]
    };
    let a = [
        [1.705050992657983, -0.6217921206570056, -0.08325887200097853],
        [
            -0.13025641750704342,
            1.1408047365754022,
            -0.010548319068358056,
        ],
        [
            -0.024003356804618032,
            -0.12896897606497063,
            1.1529723328695884,
        ],
    ];
    let b = [
        [0.6130974024011878, 0.33952314618410606, 0.04737945141470665],
        [
            0.07019372246958168,
            0.9163538790573436,
            0.013452398473074122,
        ],
        [
            0.020615592882226925,
            0.10956977293813544,
            0.8698146341796377,
        ],
    ];
    let rgb = [
        p[0] as f64 / p[3] as f64,
        p[1] as f64 / p[3] as f64,
        p[2] as f64 / p[3] as f64,
    ];
    let rgb = a
        .map(|r| r[0] * rgb[0] + r[1] * rgb[1] + r[2] * rgb[2])
        .map(|v| {
            let x = v.abs();
            if x <= 0.0031308 {
                v * 12.92
            } else {
                v.signum() * (1.055 * x.powf(1.0 / 2.4) - 0.055)
            }
        })
        .map(sample)
        .map(|v| {
            if v.abs() <= 0.04045 {
                v / 12.92
            } else {
                v.signum() * ((v.abs() + 0.055) / 1.055).powf(2.4)
            }
        });
    let rgb = b.map(|r| {
        ((r[0] * rgb[0] + r[1] * rgb[1] + r[2] * rgb[2]) * p[3] as f64).clamp(-65504.0, 65504.0)
    });
    [rgb[0], rgb[1], rgb[2], p[3] as f64]
}

fn catalog_curves_graph(
    rgba: [f32; 4],
    knots: [[f32; 2]; 5],
    contrast: f32,
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut graph = directional_graph(rgba, [0.0; 2], hash, canvas);
    graph.nodes[2].op = IrOp::NativeTransferCurves { knots, contrast };
    graph
}
const CATALOG_IDENTITY_KNOTS: [[f32; 2]; 5] = [
    [0.0, 0.0],
    [0.25, 0.25],
    [0.5, 0.5],
    [0.75, 0.75],
    [1.0, 1.0],
];
#[test]
fn native_catalog_curves_signed_hdr_alpha_and_independent_reference() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 6_500_000;
    for (knots, contrast) in [
        (CATALOG_IDENTITY_KNOTS, 0.0),
        (CATALOG_IDENTITY_KNOTS, 0.8),
        (CATALOG_IDENTITY_KNOTS, -0.8),
        (
            [[0.0, 0.0], [0.25, 0.1], [0.5, 0.7], [0.75, 0.9], [1.0, 1.0]],
            0.0,
        ),
        (
            [[0.2, 0.4], [0.4, 0.1], [0.6, 0.8], [0.9, 0.5], [1.0, 0.2]],
            0.0,
        ),
        (
            [[1.0, 0.0], [0.0, 1.0], [0.5, 0.3], [0.5, 0.7], [0.75, 0.9]],
            0.0,
        ),
        ([[0.5, 0.2]; 5], 0.0),
        (
            [
                [0.0, 0.0],
                [0.000002, 1.0],
                [0.5, 0.4],
                [0.75, 0.9],
                [1.0, 1.0],
            ],
            0.0,
        ),
    ] {
        for rgb in [
            [-0.5, 3.0, 0.25],
            [-2.0, -1.0, -4.0],
            [8.0, 16.0, 4.0],
            [0.7, 0.1, 0.4],
            [0.000001; 3],
            [0.18; 3],
        ] {
            for alpha in [0.0f32, 0.25, 1.0] {
                for (w, h) in [(1, 1), (7, 5), (16, 12)] {
                    hash += 8;
                    let rgba = [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha];
                    let graph = catalog_curves_graph(rgba, knots, contrast, hash, None);
                    assert!(graph.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
                    let mut srcgraph = graph.clone();
                    srcgraph.output = Some(IrNodeId(1));
                    let cpu_src = eval_cpu::evaluate(&srcgraph, (w, h), &mut EmptyProvider);
                    let (out, tap) = evaluator.evaluate_with_tap_frame(
                        &graph,
                        (w, h),
                        &mut NullFrameSource,
                        Some(IrNodeId(1)),
                    );
                    let out = out.unwrap();
                    let tap = tap.unwrap();
                    let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
                    let pixels = read_texture_rgba16f(&gpu, &out.texture, w, h);
                    for (i, p) in pixels.iter().enumerate() {
                        let expected = catalog_curve_reference(input[i], knots, contrast);
                        let cpu_expected =
                            catalog_curve_reference(cpu_src.pixels[i], knots, contrast);
                        for c in 0..4 {
                            assert!(
                                (cpu.pixels[i][c] as f64 - cpu_expected[c]).abs()
                                    < 0.00005 * cpu_expected[c].abs().max(1.0),
                                "CPU {knots:?}/{contrast}: {:?} vs{cpu_expected:?}",
                                cpu.pixels[i]
                            );
                            assert!(p[c].is_finite()&&(p[c]as f64-expected[c]).abs()<0.008*expected[c].abs().max(1.0),"{knots:?}/{contrast}/{rgb:?}/{alpha} {w}x{h} {i}/{c}: {p:?} vs{expected:?}");
                        }
                    }
                    if alpha == 1.0 {
                        let (pw, ph) = (out.texture.width(), out.texture.height());
                        let pad = read_texture_rgba16f(&gpu, &out.texture, pw, ph);
                        for y in 0..ph {
                            for x in 0..pw {
                                if x >= w || y >= h {
                                    assert_eq!(pad[(y * pw + x) as usize], [0.0; 4]);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_catalog_curves_draft_cache_and_identity() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let graph = catalog_curves_graph(
        [-0.25, 1.5, 0.125, 0.5],
        CATALOG_IDENTITY_KNOTS,
        0.6,
        6_600_000,
        Some((16, 12)),
    );
    for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
        let (out, tap) = evaluator.evaluate_with_tap_frame(
            &graph,
            (w, h),
            &mut NullFrameSource,
            Some(IrNodeId(1)),
        );
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
        for (i, p) in read_texture_rgba16f(&gpu, &out.texture, w, h)
            .iter()
            .enumerate()
        {
            let expected = catalog_curve_reference(input[i], CATALOG_IDENTITY_KNOTS, 0.6);
            for c in 0..4 {
                assert!((p[c] as f64 - expected[c]).abs() < 0.008 * expected[c].abs().max(1.0));
            }
        }
    }
}
#[test]
fn native_catalog_curves_refuses_domain_arity_and_bad_parameters() {
    let mut graph = catalog_curves_graph(
        [0.1, 0.2, 0.3, 1.0],
        CATALOG_IDENTITY_KNOTS,
        0.0,
        6_700_000,
        None,
    );
    for value in [1.01, -0.01, f32::NAN, f32::INFINITY] {
        for i in 0..5 {
            for j in 0..2 {
                let mut knots = CATALOG_IDENTITY_KNOTS;
                knots[i][j] = value;
                graph.nodes[2].op = IrOp::NativeTransferCurves {
                    knots,
                    contrast: 0.0,
                };
                assert!(graph.validate_working_color_domain().is_err());
            }
        }
    }
    for contrast in [-1.01, 1.01, f32::NAN, f32::INFINITY] {
        graph.nodes[2].op = IrOp::NativeTransferCurves {
            knots: CATALOG_IDENTITY_KNOTS,
            contrast,
        };
        assert!(graph.validate_working_color_domain().is_err());
    }
    graph.nodes[2].op = IrOp::NativeTransferCurves {
        knots: CATALOG_IDENTITY_KNOTS,
        contrast: 0.0,
    };
    graph.nodes[1].op = IrOp::NativeAcescct {
        direction: photonic_render::native_transfer::AcescctDirection::Encode,
    };
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    graph.nodes[2].inputs.clear();
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(graph.validate_working_color_domain().is_err());
}

#[test]
fn native_catalog_curves_half_knot_neighbors_and_storage_limits() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let smooth = [[0.0, 0.0], [0.25, 0.1], [0.5, 0.8], [0.75, 0.9], [1.0, 1.0]];
    let steep = [
        [0.0, 0.0],
        [0.000002, 1.0],
        [0.5, 0.4],
        [0.75, 0.9],
        [1.0, 1.0],
    ];
    let mut centers = Vec::new();
    for (i, (value, knots)) in [
        (0.2139892578125f32, smooth),
        (0.214111328125, smooth),
        (-1.0, steep),
        (32752.0, steep),
    ]
    .into_iter()
    .enumerate()
    {
        let graph = catalog_curves_graph(
            [value, value, value, 1.0],
            knots,
            0.0,
            6_800_000 + i as u128 * 8,
            None,
        );
        let (out, tap) = evaluator.evaluate_with_tap_frame(
            &graph,
            (7, 5),
            &mut NullFrameSource,
            Some(IrNodeId(1)),
        );
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, 7, 5);
        let output = read_texture_rgba16f(&gpu, &out.texture, 7, 5);
        let mut covered = None;
        for (j, p) in output.iter().enumerate() {
            let expected = catalog_curve_reference(input[j], knots, 0.0);
            for c in 0..4 {
                assert!(p[c].is_finite());
                assert!((p[c] as f64 - expected[c]).abs() < 0.002 * expected[c].abs().max(1.0));
            }
            if input[j][3] > 0.0 {
                assert_eq!(
                    input[j][0], value,
                    "actual source is the specified half sample"
                );
                covered = Some(p[0]);
            }
        }
        let covered = covered.expect("covered sample");
        if i < 2 {
            centers.push(covered);
        }
        if i == 2 {
            assert_eq!(
                covered, -65504.0,
                "steep extrapolation saturates only at storage limit"
            );
        }
        if i == 3 {
            assert!(covered > 1.0, "native headroom remains extended");
        }
    }
    assert!(centers[1] >= centers[0] && centers[0] > 0.0);
}
fn levels_reference(p: [f32; 4], black_white: [f32; 2], gamma: f32, output: [f32; 2]) -> [f64; 4] {
    if p[3] <= 0.0 {
        return [0.0; 4];
    }
    if black_white == [0.0, 1.0] && gamma == 1.0 && output == [0.0, 1.0] {
        return p.map(f64::from);
    }
    let a = [
        [1.705050992657983, -0.6217921206570056, -0.08325887200097853],
        [
            -0.13025641750704342,
            1.1408047365754022,
            -0.010548319068358056,
        ],
        [
            -0.024003356804618032,
            -0.12896897606497063,
            1.1529723328695884,
        ],
    ];
    let b = [
        [0.6130974024011878, 0.33952314618410606, 0.04737945141470665],
        [
            0.07019372246958168,
            0.9163538790573436,
            0.013452398473074122,
        ],
        [
            0.020615592882226925,
            0.10956977293813544,
            0.8698146341796377,
        ],
    ];
    let rgb = [
        p[0] as f64 / p[3] as f64,
        p[1] as f64 / p[3] as f64,
        p[2] as f64 / p[3] as f64,
    ];
    let rgb = a
        .map(|r| r[0] * rgb[0] + r[1] * rgb[1] + r[2] * rgb[2])
        .map(|v| {
            let x = v.abs();
            if x <= 0.0031308 {
                v * 12.92
            } else {
                v.signum() * (1.055 * x.powf(1.0 / 2.4) - 0.055)
            }
        });
    let span = (black_white[1] as f64 - black_white[0] as f64).max(0.0001);
    let rgb = rgb
        .map(|v| {
            let n = (v - black_white[0] as f64) / span;
            let bounded = n.clamp(0.0, 1.0);
            output[0] as f64
                + (bounded.powf(1.0 / gamma as f64) + (n - bounded))
                    * (output[1] as f64 - output[0] as f64)
        })
        .map(|v| {
            if v.abs() <= 0.04045 {
                v / 12.92
            } else {
                v.signum() * ((v.abs() + 0.055) / 1.055).powf(2.4)
            }
        });
    let rgb = b.map(|r| {
        ((r[0] * rgb[0] + r[1] * rgb[1] + r[2] * rgb[2]) * p[3] as f64).clamp(-65504.0, 65504.0)
    });
    [rgb[0], rgb[1], rgb[2], p[3] as f64]
}

fn levels_graph(
    rgba: [f32; 4],
    black_white: [f32; 2],
    gamma: f32,
    output: [f32; 2],
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut graph = directional_graph(rgba, [0.0; 2], hash, canvas);
    graph.nodes[2].op = IrOp::NativeTransferLevels {
        black_white,
        gamma,
        output,
    };
    graph
}
#[test]
fn native_levels_signed_hdr_alpha_and_independent_reference() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 7_100_000;
    for (bw, gamma, output) in [
        ([0.0, 1.0], 1.0, [0.0, 1.0]),
        ([0.0, 1.0], 0.01, [0.0, 1.0]),
        ([0.0, 1.0], 9.99, [0.0, 1.0]),
        ([0.1, 0.8], 2.2, [0.05, 0.9]),
        ([0.1, 0.8], 0.5, [1.0, 0.0]),
        ([0.5, 0.5], 1.0, [0.0, 1.0]),
        ([0.8, 0.2], 0.01, [0.0, 1.0]),
        ([0.2, 0.9], 4.0, [0.3, 0.3]),
    ] {
        for rgb in [
            [-0.5, 3.0, 0.25],
            [-2.0, -1.0, -4.0],
            [8.0, 16.0, 4.0],
            [0.7, 0.1, 0.4],
            [0.000001; 3],
            [0.18; 3],
        ] {
            for alpha in [0.0f32, 0.25, 1.0] {
                for (w, h) in [(1, 1), (7, 5), (16, 12)] {
                    hash += 8;
                    let rgba = [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha];
                    let graph = levels_graph(rgba, bw, gamma, output, hash, None);
                    assert!(graph.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
                    let mut srcgraph = graph.clone();
                    srcgraph.output = Some(IrNodeId(1));
                    let cpu_src = eval_cpu::evaluate(&srcgraph, (w, h), &mut EmptyProvider);
                    let (out, tap) = evaluator.evaluate_with_tap_frame(
                        &graph,
                        (w, h),
                        &mut NullFrameSource,
                        Some(IrNodeId(1)),
                    );
                    let out = out.unwrap();
                    let tap = tap.unwrap();
                    let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
                    let pixels = read_texture_rgba16f(&gpu, &out.texture, w, h);
                    for (i, p) in pixels.iter().enumerate() {
                        let expected = levels_reference(input[i], bw, gamma, output);
                        let cpu_expected = levels_reference(cpu_src.pixels[i], bw, gamma, output);
                        for c in 0..4 {
                            assert!(
                                (cpu.pixels[i][c] as f64 - cpu_expected[c]).abs()
                                    < 0.00005 * cpu_expected[c].abs().max(1.0),
                                "CPU {bw:?}/{gamma}/{output:?}: {:?} vs{cpu_expected:?}",
                                cpu.pixels[i]
                            );
                            assert!(p[c].is_finite()&&(p[c]as f64-expected[c]).abs()<0.008*expected[c].abs().max(1.0),"{bw:?}/{gamma}/{output:?}/{rgb:?}/{alpha} {w}x{h} {i}/{c}: {p:?} vs{expected:?}");
                        }
                    }
                    if alpha == 1.0 {
                        let (pw, ph) = (out.texture.width(), out.texture.height());
                        let pad = read_texture_rgba16f(&gpu, &out.texture, pw, ph);
                        for y in 0..ph {
                            for x in 0..pw {
                                if x >= w || y >= h {
                                    assert_eq!(pad[(y * pw + x) as usize], [0.0; 4]);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_levels_draft_cache() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let bw = [0.1, 0.8];
    let gamma = 2.2;
    let output = [0.05, 0.9];
    let graph = levels_graph(
        [-0.25, 1.5, 0.125, 0.5],
        bw,
        gamma,
        output,
        7_200_000,
        Some((16, 12)),
    );
    for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
        let (out, tap) = evaluator.evaluate_with_tap_frame(
            &graph,
            (w, h),
            &mut NullFrameSource,
            Some(IrNodeId(1)),
        );
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
        for (i, p) in read_texture_rgba16f(&gpu, &out.texture, w, h)
            .iter()
            .enumerate()
        {
            let expected = levels_reference(input[i], bw, gamma, output);
            for c in 0..4 {
                assert!((p[c] as f64 - expected[c]).abs() < 0.008 * expected[c].abs().max(1.0));
            }
        }
    }
}
#[test]
fn native_levels_refuses_domain_arity_and_bad_parameters() {
    let mut graph = levels_graph(
        [0.1, 0.2, 0.3, 1.0],
        [0.0, 1.0],
        1.0,
        [0.0, 1.0],
        7_300_000,
        None,
    );
    for value in [1.01, -0.01, f32::NAN, f32::INFINITY] {
        for i in 0..4 {
            let mut params = [0.0, 1.0, 0.0, 1.0];
            params[i] = value;
            graph.nodes[2].op = IrOp::NativeTransferLevels {
                black_white: [params[0], params[1]],
                gamma: 1.0,
                output: [params[2], params[3]],
            };
            assert!(graph.validate_working_color_domain().is_err());
        }
    }
    for gamma in [0.0, 10.0, f32::NAN, f32::INFINITY] {
        graph.nodes[2].op = IrOp::NativeTransferLevels {
            black_white: [0.0, 1.0],
            gamma,
            output: [0.0, 1.0],
        };
        assert!(graph.validate_working_color_domain().is_err());
    }
    graph.nodes[2].op = IrOp::NativeTransferLevels {
        black_white: [0.0, 1.0],
        gamma: 1.0,
        output: [0.0, 1.0],
    };
    graph.nodes[1].op = IrOp::NativeAcescct {
        direction: photonic_render::native_transfer::AcescctDirection::Encode,
    };
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    graph.nodes[2].inputs.clear();
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(graph.validate_working_color_domain().is_err());
}

#[test]
fn native_levels_half_boundaries_and_small_alpha_storage_limits() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut samples = Vec::new();
    for (i, (value, bw, gamma, alpha)) in [
        (0.2139892578125f32, [0.5, 0.75], 2.2, 1.0),
        (0.214111328125, [0.5, 0.75], 2.2, 1.0),
        (0.5224609375, [0.5, 0.75], 2.2, 1.0),
        (0.52294921875, [0.5, 0.75], 2.2, 1.0),
        (-1.0, [0.5, 0.5], 0.01, 0.0009765625),
        (32752.0, [0.5, 0.5], 0.01, 0.0009765625),
    ]
    .into_iter()
    .enumerate()
    {
        let graph = levels_graph(
            [value * alpha, value * alpha, value * alpha, alpha],
            bw,
            gamma,
            [0.0, 1.0],
            7_400_000 + i as u128 * 8,
            None,
        );
        let (out, tap) = evaluator.evaluate_with_tap_frame(
            &graph,
            (7, 5),
            &mut NullFrameSource,
            Some(IrNodeId(1)),
        );
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, 7, 5);
        let output = read_texture_rgba16f(&gpu, &out.texture, 7, 5);
        let mut covered = None;
        for (j, p) in output.iter().enumerate() {
            let expected = levels_reference(input[j], bw, gamma, [0.0, 1.0]);
            for c in 0..4 {
                assert!(p[c].is_finite());
                let tolerance = if i < 2 {
                    0.00002
                } else {
                    0.002 * expected[c].abs().max(1.0)
                };
                assert!(
                    (p[c] as f64 - expected[c]).abs() < tolerance,
                    "case{i} {p:?} vs{expected:?}"
                );
            }
            if input[j][3] > 0.0 {
                assert_eq!(input[j][0], value * alpha);
                assert_eq!(p[3], alpha);
                covered = Some(p[0]);
            }
        }
        samples.push(covered.unwrap());
    }
    assert!(
        samples[0] < 0.0 && samples[1] > 0.0,
        "black boundary retains signed values"
    );
    assert!(
        samples[2] < 1.0 && samples[3] > 1.0,
        "white boundary retains headroom"
    );
    assert_eq!(samples[4], -65504.0);
    assert_eq!(samples[5], 65504.0);
}
fn alpha_view_reference(p: [f32; 4], mode: u8) -> [f64; 4] {
    let a = (p[3] as f64).clamp(0.0, 1.0);
    match mode {
        1 => p.map(f64::from),
        2 => {
            if a > 0.000001 {
                [
                    (p[0] as f64 / a).clamp(-65504.0, 65504.0),
                    (p[1] as f64 / a).clamp(-65504.0, 65504.0),
                    (p[2] as f64 / a).clamp(-65504.0, 65504.0),
                    1.0,
                ]
            } else {
                [0.0, 0.0, 0.0, 1.0]
            }
        }
        _ => [a, a, a, 1.0],
    }
}

fn alpha_view_graph(
    rgba: [f32; 4],
    mode: u8,
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut graph = directional_graph(rgba, [0.0; 2], hash, canvas);
    graph.nodes[2].op = IrOp::NativeAlphaView { mode };
    graph
}
#[test]
fn native_alpha_view_modes_signed_hdr_alpha_and_independent_reference() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 7_500_000;
    for mode in 0..=2 {
        for rgb in [
            [-0.5, 3.0, 0.25],
            [-2.0, -1.0, -4.0],
            [8.0, 16.0, 4.0],
            [0.7, 0.1, 0.4],
            [0.000001; 3],
            [0.18; 3],
        ] {
            for alpha in [0.0f32, 0.0009765625, 0.25, 1.0] {
                for (w, h) in [(1, 1), (7, 5), (16, 12)] {
                    hash += 8;
                    let rgba = [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha];
                    let graph = alpha_view_graph(rgba, mode, hash, None);
                    assert!(graph.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
                    let mut srcgraph = graph.clone();
                    srcgraph.output = Some(IrNodeId(1));
                    let cpu_src = eval_cpu::evaluate(&srcgraph, (w, h), &mut EmptyProvider);
                    let (out, tap) = evaluator.evaluate_with_tap_frame(
                        &graph,
                        (w, h),
                        &mut NullFrameSource,
                        Some(IrNodeId(1)),
                    );
                    let out = out.unwrap();
                    let tap = tap.unwrap();
                    let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
                    let pixels = read_texture_rgba16f(&gpu, &out.texture, w, h);
                    for (i, p) in pixels.iter().enumerate() {
                        let expected = alpha_view_reference(input[i], mode);
                        let cpu_expected = alpha_view_reference(cpu_src.pixels[i], mode);
                        for c in 0..4 {
                            assert!(
                                (cpu.pixels[i][c] as f64 - cpu_expected[c]).abs()
                                    < 0.000002 * cpu_expected[c].abs().max(1.0)
                            );
                            assert!(
                                p[c].is_finite()
                                    && (p[c] as f64 - expected[c]).abs()
                                        < 0.002 * expected[c].abs().max(1.0),
                                "mode{mode}/{rgb:?}/{alpha}/{w}x{h} {i}/{c}: {p:?} vs{expected:?}"
                            );
                        }
                        if mode == 1 {
                            assert_eq!(*p, input[i]);
                        } else {
                            assert_eq!(p[3], 1.0);
                        }
                    }
                    let (pw, ph) = (out.texture.width(), out.texture.height());
                    let pad = read_texture_rgba16f(&gpu, &out.texture, pw, ph);
                    for y in 0..ph {
                        for x in 0..pw {
                            if x >= w || y >= h {
                                assert_eq!(pad[(y * pw + x) as usize], [0.0; 4]);
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_alpha_view_draft_cache() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    for mode in 0..=2 {
        let graph = alpha_view_graph(
            [-0.25, 1.5, 0.125, 0.5],
            mode,
            7_600_000 + mode as u128 * 8,
            Some((16, 12)),
        );
        for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
            let (out, tap) = evaluator.evaluate_with_tap_frame(
                &graph,
                (w, h),
                &mut NullFrameSource,
                Some(IrNodeId(1)),
            );
            let out = out.unwrap();
            let tap = tap.unwrap();
            let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
            for (i, p) in read_texture_rgba16f(&gpu, &out.texture, w, h)
                .iter()
                .enumerate()
            {
                let expected = alpha_view_reference(input[i], mode);
                for c in 0..4 {
                    assert!((p[c] as f64 - expected[c]).abs() < 0.002 * expected[c].abs().max(1.0));
                }
            }
        }
    }
}
#[test]
fn native_alpha_view_refuses_domain_arity_and_bad_mode() {
    let mut graph = alpha_view_graph([0.1, 0.2, 0.3, 1.0], 0, 7_700_000, None);
    for mode in [3, 255] {
        graph.nodes[2].op = IrOp::NativeAlphaView { mode };
        assert!(graph.validate_working_color_domain().is_err());
    }
    graph.nodes[2].op = IrOp::NativeAlphaView { mode: 0 };
    graph.nodes[1].op = IrOp::NativeAcescct {
        direction: photonic_render::native_transfer::AcescctDirection::Encode,
    };
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    graph.nodes[2].inputs.clear();
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(graph.validate_working_color_domain().is_err());
}
#[test]
fn native_alpha_view_half_cutoff_and_storage_limits() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut covered_values = Vec::new();
    for (i, pixel) in [
        [0.000001f32, 0.000002, -0.000003, 0.00000095367431640625],
        [0.000001, 0.000002, -0.000003, 0.0000010132789611816406],
        [32752.0, -32752.0, 1.0, 0.0009765625],
    ]
    .into_iter()
    .enumerate()
    {
        let graph = alpha_view_graph(pixel, 2, 7_800_000 + i as u128 * 8, None);
        let (out, tap) = evaluator.evaluate_with_tap_frame(
            &graph,
            (7, 5),
            &mut NullFrameSource,
            Some(IrNodeId(1)),
        );
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, 7, 5);
        let output = read_texture_rgba16f(&gpu, &out.texture, 7, 5);
        let mut covered = None;
        for (j, p) in output.iter().enumerate() {
            let expected = alpha_view_reference(input[j], 2);
            for c in 0..4 {
                assert!(
                    p[c].is_finite()
                        && (p[c] as f64 - expected[c]).abs() < 0.002 * expected[c].abs().max(1.0)
                );
            }
            if input[j][3] > 0.0 {
                assert_eq!(input[j][3], pixel[3]);
                covered = Some(*p);
            }
        }
        covered_values.push(covered.unwrap());
    }
    assert_eq!(covered_values[0], [0.0, 0.0, 0.0, 1.0]);
    assert!(covered_values[1][0] > 0.0 && covered_values[1][2] < 0.0);
    assert_eq!(covered_values[2][0], 65504.0);
    assert_eq!(covered_values[2][1], -65504.0);
}

fn outline_graph(hash: u128, canvas: Option<(u32, u32)>) -> FrameGraph {
    let mut graph = directional_graph([-0.25, 1.5, 0.125, 0.75], [0.0; 2], hash, canvas);
    graph.nodes[2].op = IrOp::NativeOutline {
        thickness: 3.5,
        color: [0.125, 0.5, 1.0],
        opacity: 0.75,
        pixel_scale: [1.0; 2],
    };
    graph
}
#[test]
fn native_outline_draft_cache_matches_independent_distance() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let graph = outline_graph(7_900_000, Some((16, 12)));
    for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
        let mut srcgraph = graph.clone();
        srcgraph.output = Some(IrNodeId(1));
        let src = eval_cpu::evaluate(&srcgraph, (w, h), &mut EmptyProvider);
        let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
        let frame = evaluator
            .evaluate_with_tap_frame(&graph, (w, h), &mut NullFrameSource, None)
            .0
            .unwrap();
        let actual = read_texture_rgba16f(&gpu, &frame.texture, w, h);
        let scale = [w as f64 / 16.0, h as f64 / 12.0];
        let sites: Vec<_> = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(x, y)| src.pixel(x, y)[3] >= 0.5)
            .collect();
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                let p = src.pixels[i].map(f64::from);
                let d = if p[3] >= 0.5 {
                    -1.0
                } else {
                    sites
                        .iter()
                        .map(|&(sx, sy)| {
                            ((x as f64 - sx as f64) / scale[0])
                                .hypot((y as f64 - sy as f64) / scale[1])
                        })
                        .fold(f64::INFINITY, f64::min)
                };
                let t = ((d - 2.5) / 2.0).clamp(0.0, 1.0);
                let a = (1.0 - t * t * (3.0 - 2.0 * t)) * 0.75 * (1.0 - p[3]);
                let expected = [p[0] + 0.125 * a, p[1] + 0.5 * a, p[2] + a, p[3] + a];
                for c in 0..4 {
                    assert!((cpu.pixels[i][c] as f64 - expected[c]).abs() < 0.00002);
                    assert!((actual[i][c] as f64 - expected[c]).abs() < 0.005);
                }
            }
        }
    }
}
#[test]
fn native_outline_refuses_domain_arity_and_invalid_parameters() {
    let mut graph = outline_graph(8_000_000, None);
    for (thickness, color, opacity, pixel_scale) in [
        (64.01, [1.0; 3], 1.0, [1.0; 2]),
        (-0.1, [1.0; 3], 1.0, [1.0; 2]),
        (f32::NAN, [1.0; 3], 1.0, [1.0; 2]),
        (2.0, [-0.1, 1.0, 1.0], 1.0, [1.0; 2]),
        (2.0, [1.0; 3], 1.01, [1.0; 2]),
        (2.0, [1.0; 3], 1.0, [0.0, 1.0]),
    ] {
        graph.nodes[2].op = IrOp::NativeOutline {
            thickness,
            color,
            opacity,
            pixel_scale,
        };
        assert!(graph.validate_working_color_domain().is_err());
    }
    graph = outline_graph(8_000_008, None);
    graph.nodes[1].op = IrOp::NativeAcescct {
        direction: AcescctDirection::Encode,
    };
    assert!(graph.validate_working_color_domain().is_err());
    graph = outline_graph(8_000_016, None);
    graph.nodes[2].inputs.clear();
    assert!(graph.validate_working_color_domain().is_err());
    graph.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(graph.validate_working_color_domain().is_err());
}

/// Independent binary64 piecewise HSL/envelope reference. Input/output are AP1.
fn linear_hsl_reference(pixel: [f32; 4], vibrance: bool, c: [f64; 3]) -> [f64; 4] {
    let alpha = pixel[3] as f64;
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    if c == [0.0; 3] {
        return pixel.map(f64::from);
    }
    let rgb = photonic_video::color::native::ap1_to_linear_srgb().transform([
        pixel[0] as f64 / alpha,
        pixel[1] as f64 / alpha,
        pixel[2] as f64 / alpha,
    ]);
    let lo = rgb.into_iter().fold(f64::INFINITY, f64::min);
    let hi = rgb.into_iter().fold(f64::NEG_INFINITY, f64::max);
    let spread = (hi - lo) / lo.abs().max(hi.abs()).max(1.0);
    if vibrance && spread <= 1e-6 {
        return pixel.map(f64::from);
    }
    let low = 2.0 * lo.min(0.0);
    let high = 1.0 + 2.0 * (hi - 1.0).max(0.0);
    let u = rgb.map(|v| (v - low) / (high - low));
    let min = u.into_iter().fold(f64::INFINITY, f64::min);
    let max = u.into_iter().fold(f64::NEG_INFINITY, f64::max);
    let d = max - min;
    let l = (max + min) / 2.0;
    let (mut hue, mut s) = if d.abs() < 1e-9 {
        (0.0, 0.0)
    } else {
        let h = if max == u[0] {
            ((u[1] - u[2]) / d).rem_euclid(6.0)
        } else if max == u[1] {
            (u[2] - u[0]) / d + 2.0
        } else {
            (u[0] - u[1]) / d + 4.0
        };
        (
            h * 60.0,
            (d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-9)).clamp(0.0, 1.0),
        )
    };
    let light = if vibrance {
        let t = ((spread - 1e-6) / (1e-4 - 1e-6)).clamp(0.0, 1.0);
        s = (s + c[0] * (1.0 - s) * t * t * (3.0 - 2.0 * t)).clamp(0.0, 1.0);
        0.0
    } else {
        hue = (hue + c[0]).rem_euclid(360.0);
        s = (s * (1.0 + c[1])).clamp(0.0, 1.0);
        c[2]
    };
    let t = light.abs();
    let keep = 1.0 - t;
    let new_low = keep * low;
    let new_high = 1.0 + keep * (high - 1.0);
    let physical_l = keep * (hi + lo) * 0.5 + t * if light >= 0.0 { 1.0 } else { 0.0 };
    let l = ((physical_l - new_low) / (new_high - new_low)).clamp(0.0, 1.0);
    let chroma = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = hue.rem_euclid(360.0) / 60.0;
    let x = chroma * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let base = match hp.floor() as u8 {
        0 => [chroma, x, 0.0],
        1 => [x, chroma, 0.0],
        2 => [0.0, chroma, x],
        3 => [0.0, x, chroma],
        4 => [x, 0.0, chroma],
        _ => [chroma, 0.0, x],
    };
    let output = base.map(|v| new_low + (v + l - chroma / 2.0) * (new_high - new_low));
    let ap1 = photonic_video::color::native::linear_srgb_to_ap1().transform(output);
    [
        (ap1[0] * alpha).clamp(-65504.0, 65504.0),
        (ap1[1] * alpha).clamp(-65504.0, 65504.0),
        (ap1[2] * alpha).clamp(-65504.0, 65504.0),
        alpha,
    ]
}

fn linear_hsl_graph(
    rgba: [f32; 4],
    vibrance: bool,
    controls: [f32; 3],
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut g = directional_graph(rgba, [0.0; 2], hash, canvas);
    g.nodes[2].op = IrOp::NativeLinearHsl { vibrance, controls };
    g
}
#[test]
fn native_linear_hsl_signed_hdr_alpha_and_independent_reference() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 8_100_000;
    for (vibrance, c) in [
        (false, [0.0, 0.0, 0.0]),
        (false, [180.0, 0.0, 0.0]),
        (false, [-180.0, 1.0, 0.0]),
        (false, [60.0, -1.0, 0.0]),
        (false, [-60.0, 0.5, 0.4]),
        (false, [30.0, 0.3, -0.6]),
        (false, [0.0, 0.0, 1.0]),
        (false, [0.0, 0.0, -1.0]),
        (true, [0.0; 3]),
        (true, [1.0, 0.0, 0.0]),
        (true, [-1.0, 0.0, 0.0]),
        (true, [0.5, 0.0, 0.0]),
    ] {
        for rgb in [
            [-0.5, 3.0, 0.25],
            [-3.0, -2.9, -2.95],
            [2.0, 2.1, 2.05],
            [0.2, 0.4, 0.6],
            [0.001, 0.00102, 0.00101],
            [0.18; 3],
            [8.0; 3],
            [0.7, 0.1, 0.4],
        ] {
            for alpha in [0.0f32, 0.0009765625, 0.5, 1.0] {
                for (w, h) in [(1, 1), (7, 5), (16, 12)] {
                    hash += 8;
                    let rgba = [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha];
                    let g = linear_hsl_graph(rgba, vibrance, c, hash, None);
                    assert!(g.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&g, (w, h), &mut EmptyProvider);
                    let mut sg = g.clone();
                    sg.output = Some(IrNodeId(1));
                    let src = eval_cpu::evaluate(&sg, (w, h), &mut EmptyProvider);
                    let (out, tap) = evaluator.evaluate_with_tap_frame(
                        &g,
                        (w, h),
                        &mut NullFrameSource,
                        Some(IrNodeId(1)),
                    );
                    let out = out.unwrap();
                    let tap = tap.unwrap();
                    let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
                    let pixels = read_texture_rgba16f(&gpu, &out.texture, w, h);
                    for (i, p) in pixels.iter().enumerate() {
                        let expected = linear_hsl_reference(input[i], vibrance, c.map(f64::from));
                        let ce = linear_hsl_reference(src.pixels[i], vibrance, c.map(f64::from));
                        for ch in 0..4 {
                            assert!(
                                (cpu.pixels[i][ch] as f64 - ce[ch]).abs()
                                    < 0.00005 * ce[ch].abs().max(1.0),
                                "CPU{vibrance}/{c:?}/{rgb:?}/{alpha}: {:?} vs{ce:?}",
                                cpu.pixels[i]
                            );
                            assert!(p[ch].is_finite()&&(p[ch]as f64-expected[ch]).abs()<0.008*expected[ch].abs().max(1.0),"GPU{vibrance}/{c:?}/{rgb:?}/{alpha}/{w}x{h} at{i}/{ch}: {p:?} vs{expected:?}");
                        }
                        assert_eq!(p[3], input[i][3]);
                        if vibrance && input[i][0] == input[i][1] && input[i][1] == input[i][2] {
                            assert_eq!(*p, input[i]);
                        }
                        if c == [0.0; 3] {
                            assert_eq!(*p, input[i]);
                        }
                    }
                    let (pw, ph) = (out.texture.width(), out.texture.height());
                    let pad = read_texture_rgba16f(&gpu, &out.texture, pw, ph);
                    for y in 0..ph {
                        for x in 0..pw {
                            if x >= w || y >= h {
                                assert_eq!(pad[(y * pw + x) as usize], [0.0; 4]);
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_linear_hsl_draft_cache_and_storage() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    for (vib, c) in [(false, [75.0, 0.6, -0.2]), (true, [0.8, 0.0, 0.0])] {
        let graph = linear_hsl_graph(
            [-0.25, 1.5, 0.125, 0.5],
            vib,
            c,
            8_200_000 + vib as u128 * 8,
            Some((16, 12)),
        );
        for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
            let (out, tap) = evaluator.evaluate_with_tap_frame(
                &graph,
                (w, h),
                &mut NullFrameSource,
                Some(IrNodeId(1)),
            );
            let out = out.unwrap();
            let tap = tap.unwrap();
            let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
            for (i, p) in read_texture_rgba16f(&gpu, &out.texture, w, h)
                .iter()
                .enumerate()
            {
                let expected = linear_hsl_reference(input[i], vib, c.map(f64::from));
                for ch in 0..4 {
                    assert!(
                        (p[ch] as f64 - expected[ch]).abs() < 0.008 * expected[ch].abs().max(1.0)
                    );
                }
            }
        }
    }
    for (i, (vib, c)) in [
        (false, [120.0, 1.0, 0.0]),
        (false, [-30.0, -1.0, 0.0]),
        (true, [1.0, 0.0, 0.0]),
    ]
    .into_iter()
    .enumerate()
    {
        let g = linear_hsl_graph(
            [32752.0, -32752.0, 1.0, 0.0009765625],
            vib,
            c,
            8_210_000 + i as u128 * 8,
            None,
        );
        let (out, tap) =
            evaluator.evaluate_with_tap_frame(&g, (7, 5), &mut NullFrameSource, Some(IrNodeId(1)));
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, 7, 5);
        for (i, p) in read_texture_rgba16f(&gpu, &out.texture, 7, 5)
            .iter()
            .enumerate()
        {
            let expected = linear_hsl_reference(input[i], vib, c.map(f64::from));
            for ch in 0..4 {
                assert!(
                    p[ch].is_finite()
                        && (p[ch] as f64 - expected[ch]).abs()
                            < 0.008 * expected[ch].abs().max(1.0)
                );
            }
        }
    }
}
#[test]
fn native_linear_hsl_refuses_domain_arity_and_controls() {
    let mut g = linear_hsl_graph([0.1, 0.2, 0.3, 1.0], false, [0.0; 3], 8_300_000, None);
    for (vibrance, controls) in [
        (false, [180.01, 0.0, 0.0]),
        (false, [0.0, -1.01, 0.0]),
        (false, [0.0, 0.0, f32::NAN]),
        (true, [1.01, 0.0, 0.0]),
        (true, [0.5, 0.1, 0.0]),
    ] {
        g.nodes[2].op = IrOp::NativeLinearHsl { vibrance, controls };
        assert!(g.validate_working_color_domain().is_err());
    }
    g.nodes[2].op = IrOp::NativeLinearHsl {
        vibrance: false,
        controls: [0.0; 3],
    };
    g.nodes[1].op = IrOp::NativeAcescct {
        direction: AcescctDirection::Encode,
    };
    assert!(g.validate_working_color_domain().is_err());
    g.nodes[2].inputs.clear();
    assert!(g.validate_working_color_domain().is_err());
    g.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    g.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(g.validate_working_color_domain().is_err());
}

fn transfer_invert_reference(p: [f32; 4]) -> [f64; 4] {
    if p[3] <= 0.0 {
        return [0.0; 4];
    }
    let a = [
        [1.705050992657983, -0.6217921206570056, -0.08325887200097853],
        [
            -0.13025641750704342,
            1.1408047365754022,
            -0.010548319068358056,
        ],
        [
            -0.024003356804618032,
            -0.12896897606497063,
            1.1529723328695884,
        ],
    ];
    let b = [
        [0.6130974024011878, 0.33952314618410606, 0.04737945141470665],
        [
            0.07019372246958168,
            0.9163538790573436,
            0.013452398473074122,
        ],
        [
            0.020615592882226925,
            0.10956977293813544,
            0.8698146341796377,
        ],
    ];
    let rgb = [
        p[0] as f64 / p[3] as f64,
        p[1] as f64 / p[3] as f64,
        p[2] as f64 / p[3] as f64,
    ];
    let linear = a.map(|r| r[0] * rgb[0] + r[1] * rgb[1] + r[2] * rgb[2]);
    let decoded = linear.map(|v| {
        let s = if v.abs() <= 0.0031308 {
            12.92 * v
        } else {
            v.signum() * (1.055 * v.abs().powf(1.0 / 2.4) - 0.055)
        };
        let n = 1.0 - s;
        if n.abs() <= 0.04045 {
            n / 12.92
        } else {
            n.signum() * ((n.abs() + 0.055) / 1.055).powf(2.4)
        }
    });
    let out = b.map(|r| {
        ((r[0] * decoded[0] + r[1] * decoded[1] + r[2] * decoded[2]) * p[3] as f64)
            .clamp(-65504.0, 65504.0)
    });
    [out[0], out[1], out[2], p[3] as f64]
}

fn transfer_invert_graph(rgba: [f32; 4], hash: u128, canvas: Option<(u32, u32)>) -> FrameGraph {
    let mut g = directional_graph(rgba, [0.0; 2], hash, canvas);
    g.nodes[2].op = IrOp::NativeTransferArtistic {
        kind: photonic_video::graph::ir::NativeTransferArtisticKind::Invert,
        parameter: 0.0,
    };
    g
}
#[test]
fn native_transfer_invert_signed_hdr_alpha_and_reference() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 8_400_000;
    for rgb in [
        [-0.5, 3.0, 0.25],
        [-2.0, -1.0, -4.0],
        [8.0, 16.0, 4.0],
        [0.7, 0.1, 0.4],
        [0.18; 3],
        [1.0; 3],
        [0.0; 3],
        [-1.0; 3],
        [4.0; 3],
        [0.003129959, 0.003131866, 0.04],
        [-0.003129959, -0.003131866, -0.04],
        [0.5, -0.2, 1.5],
    ] {
        for alpha in [0.0f32, 0.0009765625, 0.5, 1.0] {
            for (w, h) in [(1, 1), (7, 5), (16, 12)] {
                hash += 8;
                let g = transfer_invert_graph(
                    [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha],
                    hash,
                    None,
                );
                assert!(g.validate_working_color_domain().is_ok());
                let cpu = eval_cpu::evaluate(&g, (w, h), &mut EmptyProvider);
                let mut sg = g.clone();
                sg.output = Some(IrNodeId(1));
                let src = eval_cpu::evaluate(&sg, (w, h), &mut EmptyProvider);
                let (out, tap) = evaluator.evaluate_with_tap_frame(
                    &g,
                    (w, h),
                    &mut NullFrameSource,
                    Some(IrNodeId(1)),
                );
                let out = out.unwrap();
                let tap = tap.unwrap();
                let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
                let pixels = read_texture_rgba16f(&gpu, &out.texture, w, h);
                for (i, p) in pixels.iter().enumerate() {
                    let e = transfer_invert_reference(input[i]);
                    let ce = transfer_invert_reference(src.pixels[i]);
                    for c in 0..4 {
                        assert!(
                            (cpu.pixels[i][c] as f64 - ce[c]).abs()
                                < 0.00003 * ce[c].abs().max(1.0)
                        );
                        assert!(
                            p[c].is_finite()
                                && (p[c] as f64 - e[c]).abs() < 0.008 * e[c].abs().max(1.0),
                            "{rgb:?}/{alpha}/{w}x{h} {p:?} vs{e:?}"
                        );
                    }
                    assert_eq!(p[3], input[i][3]);
                }
                let (pw, ph) = (out.texture.width(), out.texture.height());
                let padded = read_texture_rgba16f(&gpu, &out.texture, pw, ph);
                for y in 0..ph {
                    for x in 0..pw {
                        if x >= w || y >= h {
                            assert_eq!(padded[(y * pw + x) as usize], [0.0; 4]);
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_transfer_invert_draft_double_inverse_and_storage() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let g = transfer_invert_graph([-0.25, 1.5, 0.125, 0.5], 8_500_000, Some((16, 12)));
    for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
        let (out, tap) =
            evaluator.evaluate_with_tap_frame(&g, (w, h), &mut NullFrameSource, Some(IrNodeId(1)));
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
        for (i, p) in read_texture_rgba16f(&gpu, &out.texture, w, h)
            .iter()
            .enumerate()
        {
            let e = transfer_invert_reference(input[i]);
            for c in 0..4 {
                assert!((p[c] as f64 - e[c]).abs() < 0.008 * e[c].abs().max(1.0));
            }
        }
    }
    for (i, rgba) in [
        [-0.25, 1.5, 0.125, 0.5],
        [0.18, 0.18, 0.18, 1.0],
        [4.0, 8.0, 2.0, 1.0],
        [0.0, 0.0, 0.0, 1.0],
        [1.0; 4],
    ]
    .into_iter()
    .enumerate()
    {
        let mut g = transfer_invert_graph(rgba, 8_510_000 + i as u128 * 8, None);
        g.nodes.push(IrNode {
            op: g.nodes[2].op.clone(),
            inputs: vec![(IrNodeId(2), OutPort(0))],
            content_hash: ContentHash(8_510_003 + i as u128 * 8),
        });
        g.output = Some(IrNodeId(3));
        let (out, tap) =
            evaluator.evaluate_with_tap_frame(&g, (7, 5), &mut NullFrameSource, Some(IrNodeId(1)));
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, 7, 5);
        for (i, p) in read_texture_rgba16f(&gpu, &out.texture, 7, 5)
            .iter()
            .enumerate()
        {
            for c in 0..4 {
                assert!((p[c] - input[i][c]).abs() < 0.015 * input[i][c].abs().max(1.0));
            }
        }
    }
    for (i, rgba) in [
        [-65504.0, -65504.0, -65504.0, 1.0],
        [65504.0, -65504.0, 32752.0, 0.0009765625],
    ]
    .into_iter()
    .enumerate()
    {
        let g = transfer_invert_graph(rgba, 8_520_000 + i as u128 * 8, None);
        let (out, tap) =
            evaluator.evaluate_with_tap_frame(&g, (7, 5), &mut NullFrameSource, Some(IrNodeId(1)));
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, 7, 5);
        for (i, p) in read_texture_rgba16f(&gpu, &out.texture, 7, 5)
            .iter()
            .enumerate()
        {
            let e = transfer_invert_reference(input[i]);
            for c in 0..4 {
                assert!(
                    p[c].is_finite() && (p[c] as f64 - e[c]).abs() < 0.008 * e[c].abs().max(1.0)
                );
            }
        }
    }
}
#[test]
fn native_transfer_invert_refuses_domain_arity_and_unused_parameter() {
    let mut g = transfer_invert_graph([0.1, 0.2, 0.3, 1.0], 8_600_000, None);
    for parameter in [1.0, f32::NAN, f32::INFINITY] {
        g.nodes[2].op = IrOp::NativeTransferArtistic {
            kind: photonic_video::graph::ir::NativeTransferArtisticKind::Invert,
            parameter,
        };
        assert!(g.validate_working_color_domain().is_err());
    }
    g.nodes[2].op = IrOp::NativeTransferArtistic {
        kind: photonic_video::graph::ir::NativeTransferArtisticKind::Invert,
        parameter: 0.0,
    };
    g.nodes[1].op = IrOp::NativeAcescct {
        direction: AcescctDirection::Encode,
    };
    assert!(g.validate_working_color_domain().is_err());
    g.nodes[2].inputs.clear();
    assert!(g.validate_working_color_domain().is_err());
    g.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    g.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(g.validate_working_color_domain().is_err());
}

/// Independent binary64 authored-tap bilateral reference, with explicit bilinear
/// sampling and fixed independently recorded AP1-to-Rec709 coefficients.
fn surface_blur_reference(
    input: &[[f32; 4]],
    w: u32,
    h: u32,
    radius: u32,
    threshold: f64,
    scale: [f64; 2],
) -> Vec<[f64; 4]> {
    if radius == 0 || threshold == 0.0 {
        return input.iter().map(|p| p.map(f64::from)).collect();
    }
    let matrix = [
        [1.705050992657983, -0.6217921206570056, -0.08325887200097853],
        [
            -0.13025641750704342,
            1.1408047365754022,
            -0.010548319068358056,
        ],
        [
            -0.024003356804618032,
            -0.12896897606497063,
            1.1529723328695884,
        ],
    ];
    let sample = |px: f64, py: f64| {
        let x = px.clamp(0.0, w as f64 - 1.0);
        let y = py.clamp(0.0, h as f64 - 1.0);
        let x0 = x.floor() as u32;
        let y0 = y.floor() as u32;
        let fx = x - x0 as f64;
        let fy = y - y0 as f64;
        let mut p = [0.0; 4];
        for (xx, yy, weight) in [
            (x0, y0, (1.0 - fx) * (1.0 - fy)),
            ((x0 + 1).min(w - 1), y0, fx * (1.0 - fy)),
            (x0, (y0 + 1).min(h - 1), (1.0 - fx) * fy),
            ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1), fx * fy),
        ] {
            for c in 0..4 {
                p[c] += input[(yy * w + xx) as usize][c] as f64 * weight;
            }
        }
        p
    };
    let sigma_s = (radius as f64).max(1.0);
    let sigma_r = threshold.max(1.0 / 255.0);
    let mut output = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let center = input[(y * w + x) as usize].map(f64::from);
            if center[3] <= 0.0 {
                output.push([0.0; 4]);
                continue;
            }
            let mut sum = [0.0; 4];
            for dy in -(radius as i32)..=radius as i32 {
                for dx in -(radius as i32)..=radius as i32 {
                    let p = sample(
                        x as f64 + dx as f64 * scale[0],
                        y as f64 + dy as f64 * scale[1],
                    );
                    let d = [p[0] - center[0], p[1] - center[1], p[2] - center[2]];
                    let distance = matrix
                        .map(|m| m[0] * d[0] + m[1] * d[1] + m[2] * d[2])
                        .into_iter()
                        .map(|v| v * v)
                        .sum::<f64>();
                    let weight = (-(dx * dx + dy * dy) as f64 / (2.0 * sigma_s * sigma_s)).exp()
                        * (-distance / (2.0 * sigma_r * sigma_r)).exp();
                    for c in 0..4 {
                        sum[c] += p[c] * weight;
                    }
                }
            }
            if sum[3] > 0.0 {
                output.push([
                    (sum[0] * center[3] / sum[3]).clamp(-65504.0, 65504.0),
                    (sum[1] * center[3] / sum[3]).clamp(-65504.0, 65504.0),
                    (sum[2] * center[3] / sum[3]).clamp(-65504.0, 65504.0),
                    center[3],
                ]);
            } else {
                output.push(center);
            }
        }
    }
    output
}

fn surface_blur_graph(
    rgba: [f32; 4],
    radius: u32,
    threshold: f32,
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut g = directional_graph(rgba, [0.0; 2], hash, canvas);
    g.nodes[2].op = IrOp::NativeSurfaceBlur {
        radius,
        threshold,
        pixel_scale: [1.0; 2],
    };
    g
}
#[test]
fn native_surface_blur_independent_reference_edges_alpha_and_padding() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 8_700_000;
    for rgb in [
        [-0.5, 3.0, 0.25],
        [0.7, 0.1, 0.4],
        [0.18; 3],
        [8.0, 16.0, 4.0],
    ] {
        for alpha in [0.0f32, 0.0009765625, 0.5, 1.0] {
            for (radius, threshold) in [
                (0, 0.25),
                (2, 0.0),
                (1, 0.003),
                (2, 0.25),
                (3, 0.6),
                (4, 1.0),
                (8, 0.25),
            ] {
                for (w, h) in [(1, 1), (7, 5), (16, 12)] {
                    hash += 8;
                    let g = surface_blur_graph(
                        [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha],
                        radius,
                        threshold,
                        hash,
                        None,
                    );
                    assert!(g.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&g, (w, h), &mut EmptyProvider);
                    let mut sg = g.clone();
                    sg.output = Some(IrNodeId(1));
                    let cs = eval_cpu::evaluate(&sg, (w, h), &mut EmptyProvider);
                    let ce = surface_blur_reference(
                        &cs.pixels,
                        w,
                        h,
                        radius,
                        threshold as f64,
                        [1.0; 2],
                    );
                    let (out, tap) = evaluator.evaluate_with_tap_frame(
                        &g,
                        (w, h),
                        &mut NullFrameSource,
                        Some(IrNodeId(1)),
                    );
                    let out = out.unwrap();
                    let tap = tap.unwrap();
                    let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
                    let e =
                        surface_blur_reference(&input, w, h, radius, threshold as f64, [1.0; 2]);
                    let actual = read_texture_rgba16f(&gpu, &out.texture, w, h);
                    for (i, p) in actual.iter().enumerate() {
                        for c in 0..4 {
                            assert!(
                                (cpu.pixels[i][c] as f64 - ce[i][c]).abs()
                                    < 0.0001 * ce[i][c].abs().max(1.0),
                                "CPU{rgb:?}/{alpha}/r{radius}/t{threshold}: {:?} vs{:?}",
                                cpu.pixels[i],
                                ce[i]
                            );
                            assert!(
                                p[c].is_finite()
                                    && (p[c] as f64 - e[i][c]).abs()
                                        < 0.006 * e[i][c].abs().max(1.0),
                                "GPU{rgb:?}/{alpha}/r{radius}/t{threshold}: {p:?} vs{:?}",
                                e[i]
                            );
                        }
                        assert_eq!(p[3], input[i][3]);
                        if radius == 0 || threshold == 0.0 {
                            assert_eq!(*p, input[i]);
                        }
                    }
                    let (pw, ph) = (out.texture.width(), out.texture.height());
                    let pad = read_texture_rgba16f(&gpu, &out.texture, pw, ph);
                    for y in 0..ph {
                        for x in 0..pw {
                            if x >= w || y >= h {
                                assert_eq!(pad[(y * pw + x) as usize], [0.0; 4]);
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_surface_blur_draft_cache_radius_limit_constant_and_storage() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let graph = surface_blur_graph([-0.25, 1.5, 0.125, 0.5], 4, 0.7, 8_800_000, Some((16, 12)));
    for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
        let (out, tap) = evaluator.evaluate_with_tap_frame(
            &graph,
            (w, h),
            &mut NullFrameSource,
            Some(IrNodeId(1)),
        );
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
        let e = surface_blur_reference(&input, w, h, 4, 0.7, [w as f64 / 16.0, h as f64 / 12.0]);
        let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
        let mut sg = graph.clone();
        sg.output = Some(IrNodeId(1));
        let cs = eval_cpu::evaluate(&sg, (w, h), &mut EmptyProvider);
        let ce =
            surface_blur_reference(&cs.pixels, w, h, 4, 0.7, [w as f64 / 16.0, h as f64 / 12.0]);
        for (i, p) in read_texture_rgba16f(&gpu, &out.texture, w, h)
            .iter()
            .enumerate()
        {
            for c in 0..4 {
                assert!(
                    (cpu.pixels[i][c] as f64 - ce[i][c]).abs() < 0.0001 * ce[i][c].abs().max(1.0)
                );
                assert!((p[c] as f64 - e[i][c]).abs() < 0.006 * e[i][c].abs().max(1.0));
            }
        }
    }
    for (i, (rgba, radius, threshold, constant)) in [
        ([-0.15, 2.1, 0.09, 0.5], 64, 1.0, true),
        ([-0.25, 1.5, 0.125, 0.5], 64, 0.25, false),
        ([32752.0, -32752.0, 1.0, 0.0009765625], 8, 1.0, false),
    ]
    .into_iter()
    .enumerate()
    {
        let mut g = surface_blur_graph(rgba, radius, threshold, 8_810_000 + i as u128 * 8, None);
        if constant {
            g.nodes[1].op = IrOp::Crop {
                left: 0.0,
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
            };
        }
        let (w, h) = (3, 3);
        let (out, tap) =
            evaluator.evaluate_with_tap_frame(&g, (w, h), &mut NullFrameSource, Some(IrNodeId(1)));
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
        let e = surface_blur_reference(&input, w, h, radius, threshold as f64, [1.0; 2]);
        for (i, p) in read_texture_rgba16f(&gpu, &out.texture, w, h)
            .iter()
            .enumerate()
        {
            for c in 0..4 {
                assert!(
                    p[c].is_finite()
                        && (p[c] as f64 - e[i][c]).abs() < 0.006 * e[i][c].abs().max(1.0)
                );
            }
        }
    }
}
#[test]
fn native_surface_blur_budget_refusal_pruned_tap_and_recovery() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu);
    let mut g = surface_blur_graph([0.1, 0.2, 0.3, 1.0], 64, 0.5, 8_900_000, None);
    assert!(evaluator
        .evaluate_with_tap_frame(&g, (129, 120), &mut NullFrameSource, None)
        .0
        .is_none());
    assert!(evaluator
        .computation_errors()
        .iter()
        .any(|e| e.contains("Surface Blur") && e.contains("256M")));
    g.output = Some(IrNodeId(0));
    assert!(evaluator
        .evaluate_with_tap_frame(&g, (129, 120), &mut NullFrameSource, None)
        .0
        .is_some());
    assert!(evaluator.computation_errors().is_empty());
    assert!(evaluator
        .evaluate_with_tap_frame(&g, (129, 120), &mut NullFrameSource, Some(IrNodeId(2)))
        .0
        .is_none());
    let g = surface_blur_graph([0.1, 0.2, 0.3, 1.0], 2, 0.5, 8_900_016, None);
    assert!(evaluator
        .evaluate_with_tap_frame(&g, (7, 5), &mut NullFrameSource, None)
        .0
        .is_some());
    assert!(evaluator.computation_errors().is_empty());
}
#[test]
fn native_surface_blur_refuses_domain_arity_and_parameters() {
    let mut g = surface_blur_graph([0.1, 0.2, 0.3, 1.0], 2, 0.5, 8_910_000, None);
    for (radius, threshold, pixel_scale) in [
        (65, 0.5, [1.0; 2]),
        (2, -0.1, [1.0; 2]),
        (2, f32::NAN, [1.0; 2]),
        (2, 1.01, [1.0; 2]),
        (2, 0.5, [0.0, 1.0]),
    ] {
        g.nodes[2].op = IrOp::NativeSurfaceBlur {
            radius,
            threshold,
            pixel_scale,
        };
        assert!(g.validate_working_color_domain().is_err());
    }
    g.nodes[2].op = IrOp::NativeSurfaceBlur {
        radius: 2,
        threshold: 0.5,
        pixel_scale: [1.0; 2],
    };
    g.nodes[1].op = IrOp::NativeAcescct {
        direction: AcescctDirection::Encode,
    };
    assert!(g.validate_working_color_domain().is_err());
    g.nodes[2].inputs.clear();
    assert!(g.validate_working_color_domain().is_err());
    g.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    g.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(g.validate_working_color_domain().is_err());
}

/// Independent binary64 discrete-aperture reference with clamp-edge bilinear taps.
fn lens_blur_reference(
    input: &[[f32; 4]],
    w: u32,
    h: u32,
    radius: f64,
    scale: [f64; 2],
) -> Vec<[f64; 4]> {
    if radius < 1.0 {
        return input.iter().map(|p| p.map(f64::from)).collect();
    }
    let sample = |px: f64, py: f64| {
        let x = px.clamp(0.0, w as f64 - 1.0);
        let y = py.clamp(0.0, h as f64 - 1.0);
        let x0 = x.floor() as u32;
        let y0 = y.floor() as u32;
        let fx = x - x0 as f64;
        let fy = y - y0 as f64;
        let mut p = [0.0; 4];
        for (xx, yy, weight) in [
            (x0, y0, (1.0 - fx) * (1.0 - fy)),
            ((x0 + 1).min(w - 1), y0, fx * (1.0 - fy)),
            (x0, (y0 + 1).min(h - 1), (1.0 - fx) * fy),
            ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1), fx * fy),
        ] {
            for c in 0..4 {
                p[c] += input[(yy * w + xx) as usize][c] as f64 * weight;
            }
        }
        p
    };

    let r = radius.ceil() as i32;
    let radius2 = ((radius as f32) * (radius as f32)) as f64;
    let mut output = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0.0; 4];
            let mut count = 0u32;
            for dy in -r..=r {
                for dx in -r..=r {
                    if (dx * dx + dy * dy) as f64 > radius2 {
                        continue;
                    }
                    let p = sample(
                        x as f64 + dx as f64 * scale[0],
                        y as f64 + dy as f64 * scale[1],
                    );
                    for c in 0..4 {
                        sum[c] += p[c];
                    }
                    count += 1;
                }
            }
            output.push([
                (sum[0] / count as f64).clamp(-65504.0, 65504.0),
                (sum[1] / count as f64).clamp(-65504.0, 65504.0),
                (sum[2] / count as f64).clamp(-65504.0, 65504.0),
                sum[3] / count as f64,
            ]);
        }
    }
    output
}

fn lens_blur_graph(
    rgba: [f32; 4],
    radius: f32,
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut g = directional_graph(rgba, [0.0; 2], hash, canvas);
    g.nodes[2].op = IrOp::NativeLensBlur {
        radius,
        pixel_scale: [1.0; 2],
    };
    g
}
#[test]
fn native_lens_blur_independent_reference_edges_alpha_and_padding() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 9_000_000;
    for rgb in [
        [-0.5, 3.0, 0.25],
        [0.7, 0.1, 0.4],
        [0.18; 3],
        [8.0, 16.0, 4.0],
    ] {
        for alpha in [0.0f32, 0.0009765625, 0.5, 1.0] {
            for radius in [0.0, 0.99, 1.0, 1.4142137, 2.0, 3.25, 8.0] {
                for (w, h) in [(1, 1), (7, 5), (16, 12)] {
                    hash += 8;
                    let g = lens_blur_graph(
                        [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha],
                        radius,
                        hash,
                        None,
                    );
                    assert!(g.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&g, (w, h), &mut EmptyProvider);
                    let mut sg = g.clone();
                    sg.output = Some(IrNodeId(1));
                    let cs = eval_cpu::evaluate(&sg, (w, h), &mut EmptyProvider);
                    let ce = lens_blur_reference(&cs.pixels, w, h, radius as f64, [1.0; 2]);
                    let (out, tap) = evaluator.evaluate_with_tap_frame(
                        &g,
                        (w, h),
                        &mut NullFrameSource,
                        Some(IrNodeId(1)),
                    );
                    let out = out.unwrap();
                    let tap = tap.unwrap();
                    let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
                    let e = lens_blur_reference(&input, w, h, radius as f64, [1.0; 2]);
                    let actual = read_texture_rgba16f(&gpu, &out.texture, w, h);
                    for (i, p) in actual.iter().enumerate() {
                        for c in 0..4 {
                            assert!(
                                (cpu.pixels[i][c] as f64 - ce[i][c]).abs()
                                    < 0.0001 * ce[i][c].abs().max(1.0),
                                "CPU{rgb:?}/{alpha}/r{radius}: {:?} vs{:?}",
                                cpu.pixels[i],
                                ce[i]
                            );
                            assert!(
                                p[c].is_finite()
                                    && (p[c] as f64 - e[i][c]).abs()
                                        < 0.006 * e[i][c].abs().max(1.0),
                                "GPU{rgb:?}/{alpha}/r{radius}: {p:?} vs{:?}",
                                e[i]
                            );
                        }
                        if radius < 1.0 {
                            assert_eq!(*p, input[i]);
                        }
                    }
                    let (pw, ph) = (out.texture.width(), out.texture.height());
                    let pad = read_texture_rgba16f(&gpu, &out.texture, pw, ph);
                    for y in 0..ph {
                        for x in 0..pw {
                            if x >= w || y >= h {
                                assert_eq!(pad[(y * pw + x) as usize], [0.0; 4]);
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_lens_blur_draft_cache_constant_and_storage() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let graph = lens_blur_graph([-0.25, 1.5, 0.125, 0.5], 4.25, 9_100_000, Some((16, 12)));
    for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
        let (out, tap) = evaluator.evaluate_with_tap_frame(
            &graph,
            (w, h),
            &mut NullFrameSource,
            Some(IrNodeId(1)),
        );
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
        let e = lens_blur_reference(&input, w, h, 4.25, [w as f64 / 16.0, h as f64 / 12.0]);
        let cpu = eval_cpu::evaluate(&graph, (w, h), &mut EmptyProvider);
        let mut sg = graph.clone();
        sg.output = Some(IrNodeId(1));
        let cs = eval_cpu::evaluate(&sg, (w, h), &mut EmptyProvider);
        let ce = lens_blur_reference(&cs.pixels, w, h, 4.25, [w as f64 / 16.0, h as f64 / 12.0]);
        for (i, p) in read_texture_rgba16f(&gpu, &out.texture, w, h)
            .iter()
            .enumerate()
        {
            for c in 0..4 {
                assert!(
                    (cpu.pixels[i][c] as f64 - ce[i][c]).abs() < 0.0001 * ce[i][c].abs().max(1.0)
                );
                assert!((p[c] as f64 - e[i][c]).abs() < 0.006 * e[i][c].abs().max(1.0));
            }
        }
    }
    for (i, rgba) in [[0.125, 0.25, 0.375, 0.5], [65504.0, -65504.0, 32752.0, 1.0]]
        .into_iter()
        .enumerate()
    {
        let mut g = lens_blur_graph(rgba, 500.0, 9_110_000 + i as u128 * 8, None);
        g.nodes[1].op = IrOp::Crop {
            left: 0.0,
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
        };
        let out = evaluator
            .evaluate_with_tap_frame(&g, (3, 3), &mut NullFrameSource, None)
            .0
            .unwrap();
        for p in read_texture_rgba16f(&gpu, &out.texture, 3, 3) {
            for c in 0..4 {
                assert!(
                    (p[c] - rgba[c]).abs() < 0.002 * rgba[c].abs().max(1.0),
                    "constant{p:?}/{rgba:?}"
                );
            }
        }
    }
}
#[test]
fn native_lens_blur_budget_refusal_pruned_tap_and_recovery() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu);
    let mut g = lens_blur_graph([0.1, 0.2, 0.3, 1.0], 500.0, 9_200_000, None);
    assert!(evaluator
        .evaluate_with_tap_frame(&g, (16, 16), &mut NullFrameSource, None)
        .0
        .is_none());
    assert!(evaluator
        .computation_errors()
        .iter()
        .any(|e| e.contains("Lens Blur") && e.contains("256M")));
    g.output = Some(IrNodeId(0));
    assert!(evaluator
        .evaluate_with_tap_frame(&g, (16, 16), &mut NullFrameSource, None)
        .0
        .is_some());
    assert!(evaluator.computation_errors().is_empty());
    assert!(evaluator
        .evaluate_with_tap_frame(&g, (16, 16), &mut NullFrameSource, Some(IrNodeId(2)))
        .0
        .is_none());
    let g = lens_blur_graph([0.1, 0.2, 0.3, 1.0], 2.0, 9_200_016, None);
    assert!(evaluator
        .evaluate_with_tap_frame(&g, (7, 5), &mut NullFrameSource, None)
        .0
        .is_some());
    assert!(evaluator.computation_errors().is_empty());
}
#[test]
fn native_lens_blur_refuses_domain_arity_and_parameters() {
    let mut g = lens_blur_graph([0.1, 0.2, 0.3, 1.0], 2.0, 9_210_000, None);
    for (radius, pixel_scale) in [
        (500.01, [1.0; 2]),
        (-0.1, [1.0; 2]),
        (f32::NAN, [1.0; 2]),
        (2.0, [0.0, 1.0]),
    ] {
        g.nodes[2].op = IrOp::NativeLensBlur {
            radius,
            pixel_scale,
        };
        assert!(g.validate_working_color_domain().is_err());
    }
    g.nodes[2].op = IrOp::NativeLensBlur {
        radius: 2.0,
        pixel_scale: [1.0; 2],
    };
    g.nodes[1].op = IrOp::NativeAcescct {
        direction: AcescctDirection::Encode,
    };
    assert!(g.validate_working_color_domain().is_err());
    g.nodes[2].inputs.clear();
    assert!(g.validate_working_color_domain().is_err());
    g.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    g.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(g.validate_working_color_domain().is_err());
}

/// Independent binary64 reference for the established diagnostic numeric effect.
fn unpremultiply_reference(p: [f32; 4]) -> [f64; 4] {
    let a = (p[3] as f64).clamp(0.0, 1.0);
    if a > 0.000001 {
        [
            (p[0] as f64 / a).clamp(-65504.0, 65504.0),
            (p[1] as f64 / a).clamp(-65504.0, 65504.0),
            (p[2] as f64 / a).clamp(-65504.0, 65504.0),
            a,
        ]
    } else {
        [0.0; 4]
    }
}

fn unpremultiply_graph(rgba: [f32; 4], hash: u128, canvas: Option<(u32, u32)>) -> FrameGraph {
    let mut g = directional_graph(rgba, [0.0; 2], hash, canvas);
    g.nodes[2].op = IrOp::NativeUnpremultiply;
    g
}
#[test]
fn native_unpremultiply_signed_hdr_alpha_independent_reference() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 9_300_000;
    for rgb in [
        [-0.5, 3.0, 0.25],
        [-2.0, -1.0, -4.0],
        [8.0, 16.0, 4.0],
        [0.7, 0.1, 0.4],
        [0.000001; 3],
        [0.18; 3],
    ] {
        for alpha in [0.0f32, 0.0009765625, 0.25, 1.0] {
            for (w, h) in [(1, 1), (7, 5), (16, 12)] {
                hash += 8;
                let g = unpremultiply_graph(
                    [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha],
                    hash,
                    None,
                );
                assert!(g.validate_working_color_domain().is_ok());
                let cpu = eval_cpu::evaluate(&g, (w, h), &mut EmptyProvider);
                let mut sg = g.clone();
                sg.output = Some(IrNodeId(1));
                let cs = eval_cpu::evaluate(&sg, (w, h), &mut EmptyProvider);
                let (out, tap) = evaluator.evaluate_with_tap_frame(
                    &g,
                    (w, h),
                    &mut NullFrameSource,
                    Some(IrNodeId(1)),
                );
                let out = out.unwrap();
                let tap = tap.unwrap();
                let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
                let actual = read_texture_rgba16f(&gpu, &out.texture, w, h);
                for (i, p) in actual.iter().enumerate() {
                    let e = unpremultiply_reference(input[i]);
                    let ce = unpremultiply_reference(cs.pixels[i]);
                    for c in 0..4 {
                        assert!(
                            (cpu.pixels[i][c] as f64 - ce[c]).abs()
                                < 0.000002 * ce[c].abs().max(1.0)
                        );
                        assert!(
                            p[c].is_finite()
                                && (p[c] as f64 - e[c]).abs() < 0.002 * e[c].abs().max(1.0),
                            "{rgb:?}/{alpha}/{w}x{h} {p:?} vs{e:?}"
                        );
                    }
                    assert_eq!(p[3], e[3] as f32);
                }
                let (pw, ph) = (out.texture.width(), out.texture.height());
                let pad = read_texture_rgba16f(&gpu, &out.texture, pw, ph);
                for y in 0..ph {
                    for x in 0..pw {
                        if x >= w || y >= h {
                            assert_eq!(pad[(y * pw + x) as usize], [0.0; 4]);
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_unpremultiply_draft_cache() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let g = unpremultiply_graph([-0.25, 1.5, 0.125, 0.5], 9_400_000, Some((16, 12)));
    for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
        let (out, tap) =
            evaluator.evaluate_with_tap_frame(&g, (w, h), &mut NullFrameSource, Some(IrNodeId(1)));
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
        for (i, p) in read_texture_rgba16f(&gpu, &out.texture, w, h)
            .iter()
            .enumerate()
        {
            let e = unpremultiply_reference(input[i]);
            for c in 0..4 {
                assert!((p[c] as f64 - e[c]).abs() < 0.002 * e[c].abs().max(1.0));
            }
        }
    }
}
#[test]
fn native_unpremultiply_refuses_domain_and_arity() {
    let mut g = unpremultiply_graph([0.1, 0.2, 0.3, 1.0], 9_500_000, None);
    g.nodes[1].op = IrOp::NativeAcescct {
        direction: AcescctDirection::Encode,
    };
    assert!(g.validate_working_color_domain().is_err());
    g.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    g.nodes[2].inputs.clear();
    assert!(g.validate_working_color_domain().is_err());
    g.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(g.validate_working_color_domain().is_err());
}
#[test]
fn native_unpremultiply_half_cutoff_and_storage() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut values = Vec::new();
    for (i, pixel) in [
        [0.000001f32, 0.000002, -0.000003, 0.00000095367431640625],
        [0.000001, 0.000002, -0.000003, 0.0000010132789611816406],
        [32752.0, -32752.0, 1.0, 0.0009765625],
    ]
    .into_iter()
    .enumerate()
    {
        let g = unpremultiply_graph(pixel, 9_600_000 + i as u128 * 8, None);
        let (out, tap) =
            evaluator.evaluate_with_tap_frame(&g, (7, 5), &mut NullFrameSource, Some(IrNodeId(1)));
        let out = out.unwrap();
        let tap = tap.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.texture, 7, 5);
        let output = read_texture_rgba16f(&gpu, &out.texture, 7, 5);
        let mut covered = None;
        for (j, p) in output.iter().enumerate() {
            let e = unpremultiply_reference(input[j]);
            for c in 0..4 {
                assert!(
                    p[c].is_finite() && (p[c] as f64 - e[c]).abs() < 0.002 * e[c].abs().max(1.0)
                );
            }
            if input[j][3] > 0.0 {
                assert_eq!(input[j][3], pixel[3]);
                covered = Some(*p);
            }
        }
        values.push(covered.unwrap());
    }
    assert_eq!(values[0], [0.0; 4]);
    assert!(values[1][0] > 0.0 && values[1][2] < 0.0);
    assert_eq!(values[1][3], 0.0000010132789611816406);
    assert_eq!(values[2][0], 65504.0);
    assert_eq!(values[2][1], -65504.0);
    assert_eq!(values[2][3], 0.0009765625);
}

/// Independent binary64 sorted-window reference; no implementation helpers.
fn rank_reference(
    input: &[[f32; 4]],
    w: u32,
    h: u32,
    dust: bool,
    radius: u32,
    threshold: u8,
    scale: [f64; 2],
) -> Vec<[f64; 4]> {
    if radius == 0 {
        return input.iter().map(|p| p.map(f64::from)).collect();
    }
    let a = [
        [1.705050992657983, -0.6217921206570056, -0.08325887200097853],
        [
            -0.13025641750704342,
            1.1408047365754022,
            -0.010548319068358056,
        ],
        [
            -0.024003356804618032,
            -0.12896897606497063,
            1.1529723328695884,
        ],
    ];
    let b = [
        [0.6130974024011878, 0.33952314618410606, 0.04737945141470665],
        [
            0.07019372246958168,
            0.9163538790573436,
            0.013452398473074122,
        ],
        [
            0.020615592882226925,
            0.10956977293813544,
            0.8698146341796377,
        ],
    ];
    let operand = |v: [f64; 4]| {
        let alpha = v[3].clamp(0.0, 1.0);
        let rgb = if alpha > 1e-6 {
            a.map(|m| v[1] / alpha + m[0] * (v[0] - v[1]) / alpha + m[2] * (v[2] - v[1]) / alpha)
        } else {
            [0.0; 3]
        };
        [rgb[0], rgb[1], rgb[2], alpha]
    };
    let sample = |px: f64, py: f64| {
        let x = px.clamp(0.0, w as f64 - 1.0);
        let y = py.clamp(0.0, h as f64 - 1.0);
        let x0 = x.floor() as u32;
        let y0 = y.floor() as u32;
        let fx = x - x0 as f64;
        let fy = y - y0 as f64;
        let mut p = [0.0; 4];
        for (xx, yy, weight) in [
            (x0, y0, (1.0 - fx) * (1.0 - fy)),
            ((x0 + 1).min(w - 1), y0, fx * (1.0 - fy)),
            (x0, (y0 + 1).min(h - 1), (1.0 - fx) * fy),
            ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1), fx * fy),
        ] {
            for c in 0..4 {
                p[c] += input[(yy * w + xx) as usize][c] as f64 * weight;
            }
        }
        operand(p)
    };
    let r =
        radius.min(((w as f64 / scale[0]).max(h as f64 / scale[1]).ceil() as u32).max(1)) as i32;
    let mut output = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let mut channels: [Vec<f64>; 4] = std::array::from_fn(|_| Vec::new());
            for dy in -r..=r {
                for dx in -r..=r {
                    let p = sample(
                        x as f64 + dx as f64 * scale[0],
                        y as f64 + dy as f64 * scale[1],
                    );
                    for c in 0..4 {
                        channels[c].push(p[c]);
                    }
                }
            }
            let mut med = channels.map(|mut c| {
                c.sort_by(f64::total_cmp);
                c[c.len() / 2]
            });
            if dust {
                let orig = operand(input[(y * w + x) as usize].map(f64::from));
                for c in 0..3 {
                    if (orig[c] - med[c]).abs() <= threshold as f64 / 255.0 {
                        med[c] = orig[c];
                    }
                }
                med[3] = orig[3];
            }
            let rgb = b.map(|m| {
                ((med[1] + m[0] * (med[0] - med[1]) + m[2] * (med[2] - med[1])) * med[3])
                    .clamp(-65504.0, 65504.0)
            });
            output.push([rgb[0], rgb[1], rgb[2], med[3]]);
        }
    }
    output
}

fn rank_graph(
    rgba: [f32; 4],
    dust: bool,
    radius: u32,
    threshold: u8,
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut g = directional_graph(rgba, [0.0; 2], hash, canvas);
    g.nodes[2].op = IrOp::NativeRankFilter {
        kind: if dust {
            photonic_video::graph::ir::NativeRankKind::DustScratches
        } else {
            photonic_video::graph::ir::NativeRankKind::Median
        },
        radius,
        threshold: if dust { threshold } else { 0 },
        pixel_scale: [1.0; 2],
    };
    g
}
#[test]
fn native_rank_independent_reference_edges_alpha_and_padding() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 9_700_000;
    for dust in [false, true] {
        for rgb in [
            [-0.5, 3.0, 0.25],
            [0.7, 0.1, 0.4],
            [0.18; 3],
            [8.0, 16.0, 4.0],
        ] {
            for alpha in [0.0f32, 0.0009765625, 0.5, 1.0] {
                for (radius, threshold) in [(0, 0), (1, 16), (2, 0), (3, 128), (7, 255), (8, 16)] {
                    for (w, h) in [(1, 1), (7, 5), (16, 12)] {
                        hash += 8;
                        let threshold = if dust { threshold } else { 0 };
                        let g = rank_graph(
                            [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha],
                            dust,
                            radius,
                            threshold,
                            hash,
                            None,
                        );
                        assert!(g.validate_working_color_domain().is_ok());
                        let cpu = eval_cpu::evaluate(&g, (w, h), &mut EmptyProvider);
                        let mut sg = g.clone();
                        sg.output = Some(IrNodeId(1));
                        let cs = eval_cpu::evaluate(&sg, (w, h), &mut EmptyProvider);
                        let ce =
                            rank_reference(&cs.pixels, w, h, dust, radius, threshold, [1.0; 2]);
                        let (out, tap) = evaluator.evaluate_with_tap_frame(
                            &g,
                            (w, h),
                            &mut NullFrameSource,
                            Some(IrNodeId(1)),
                        );
                        let out = out.unwrap();
                        let tap = tap.unwrap();
                        let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
                        let e = rank_reference(&input, w, h, dust, radius, threshold, [1.0; 2]);
                        let actual = read_texture_rgba16f(&gpu, &out.texture, w, h);
                        for (i, p) in actual.iter().enumerate() {
                            for c in 0..4 {
                                assert!((cpu.pixels[i][c]as f64-ce[i][c]).abs()<0.0001*ce[i][c].abs().max(1.0),"CPU dust{dust}/{rgb:?}/{alpha}/r{radius}/t{threshold}: {:?} vs{:?}",cpu.pixels[i],ce[i]);
                                assert!(p[c].is_finite()&&(p[c]as f64-e[i][c]).abs()<0.006*e[i][c].abs().max(1.0),"GPU dust{dust}/{rgb:?}/{alpha}/r{radius}/t{threshold}: {p:?} vs{:?}",e[i]);
                            }
                            if radius == 0 {
                                assert_eq!(*p, input[i]);
                            }
                            if dust {
                                assert_eq!(p[3], input[i][3]);
                            }
                        }
                        let (pw, ph) = (out.texture.width(), out.texture.height());
                        let pad = read_texture_rgba16f(&gpu, &out.texture, pw, ph);
                        for y in 0..ph {
                            for x in 0..pw {
                                if x >= w || y >= h {
                                    assert_eq!(pad[(y * pw + x) as usize], [0.0; 4]);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_rank_draft_cache_and_extent_limits() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    for dust in [false, true] {
        let g = rank_graph(
            [-0.25, 1.5, 0.125, 0.5],
            dust,
            8,
            16,
            9_800_000 + dust as u128 * 8,
            Some((16, 12)),
        );
        for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
            let (out, tap) = evaluator.evaluate_with_tap_frame(
                &g,
                (w, h),
                &mut NullFrameSource,
                Some(IrNodeId(1)),
            );
            let out = out.unwrap();
            let tap = tap.unwrap();
            let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
            let e = rank_reference(
                &input,
                w,
                h,
                dust,
                8,
                if dust { 16 } else { 0 },
                [w as f64 / 16.0, h as f64 / 12.0],
            );
            let cpu = eval_cpu::evaluate(&g, (w, h), &mut EmptyProvider);
            let mut sg = g.clone();
            sg.output = Some(IrNodeId(1));
            let cs = eval_cpu::evaluate(&sg, (w, h), &mut EmptyProvider);
            let ce = rank_reference(
                &cs.pixels,
                w,
                h,
                dust,
                8,
                if dust { 16 } else { 0 },
                [w as f64 / 16.0, h as f64 / 12.0],
            );
            for (i, p) in read_texture_rgba16f(&gpu, &out.texture, w, h)
                .iter()
                .enumerate()
            {
                for c in 0..4 {
                    assert!(
                        (cpu.pixels[i][c] as f64 - ce[i][c]).abs()
                            < 0.0001 * ce[i][c].abs().max(1.0)
                    );
                    assert!((p[c] as f64 - e[i][c]).abs() < 0.006 * e[i][c].abs().max(1.0));
                }
            }
        }
        for (i, rgba) in [[0.125, 0.25, 0.375, 0.5], [65504.0, -65504.0, 32752.0, 1.0]]
            .into_iter()
            .enumerate()
        {
            let mut g = rank_graph(
                rgba,
                dust,
                if dust { 16 } else { 500 },
                16,
                9_810_000 + dust as u128 * 64 + i as u128 * 8,
                None,
            );
            g.nodes[1].op = IrOp::Crop {
                left: 0.0,
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
            };
            let out = evaluator
                .evaluate_with_tap_frame(&g, (3, 3), &mut NullFrameSource, None)
                .0
                .unwrap();
            for p in read_texture_rgba16f(&gpu, &out.texture, 3, 3) {
                for c in 0..4 {
                    assert!(
                        (p[c] - rgba[c]).abs() < 0.002 * rgba[c].abs().max(1.0),
                        "constant dust{dust}: {p:?}/{rgba:?}"
                    );
                }
            }
        }
    }
}
#[test]
fn native_rank_budget_refusal_pruned_tap_and_recovery() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu);
    for dust in [false, true] {
        let mut g = rank_graph(
            [0.1, 0.2, 0.3, 1.0],
            dust,
            if dust { 16 } else { 500 },
            16,
            9_900_000 + dust as u128 * 32,
            None,
        );
        assert!(evaluator
            .evaluate_with_tap_frame(&g, (320, 240), &mut NullFrameSource, None)
            .0
            .is_none());
        assert!(evaluator
            .computation_errors()
            .iter()
            .any(|e| e.contains("Median") && e.contains("budget")));
        g.output = Some(IrNodeId(0));
        assert!(evaluator
            .evaluate_with_tap_frame(&g, (320, 240), &mut NullFrameSource, None)
            .0
            .is_some());
        assert!(evaluator.computation_errors().is_empty());
        assert!(evaluator
            .evaluate_with_tap_frame(&g, (320, 240), &mut NullFrameSource, Some(IrNodeId(2)))
            .0
            .is_none());
        let g = rank_graph(
            [0.1, 0.2, 0.3, 1.0],
            dust,
            2,
            16,
            9_900_016 + dust as u128 * 32,
            None,
        );
        assert!(evaluator
            .evaluate_with_tap_frame(&g, (7, 5), &mut NullFrameSource, None)
            .0
            .is_some());
        assert!(evaluator.computation_errors().is_empty());
    }
}
#[test]
fn native_rank_refuses_domain_arity_and_parameters() {
    use photonic_video::graph::ir::NativeRankKind::*;
    let mut g = rank_graph([0.1, 0.2, 0.3, 1.0], false, 2, 0, 9_910_000, None);
    for (kind, radius, threshold, pixel_scale) in [
        (Median, 501, 0, [1.0; 2]),
        (DustScratches, 17, 0, [1.0; 2]),
        (Median, 2, 1, [1.0; 2]),
        (Median, 2, 0, [0.0, 1.0]),
        (Median, 2, 0, [f32::NAN, 1.0]),
    ] {
        g.nodes[2].op = IrOp::NativeRankFilter {
            kind,
            radius,
            threshold,
            pixel_scale,
        };
        assert!(g.validate_working_color_domain().is_err());
    }
    g.nodes[2].op = IrOp::NativeRankFilter {
        kind: Median,
        radius: 2,
        threshold: 0,
        pixel_scale: [1.0; 2],
    };
    g.nodes[1].op = IrOp::NativeAcescct {
        direction: AcescctDirection::Encode,
    };
    assert!(g.validate_working_color_domain().is_err());
    g.nodes[2].inputs.clear();
    assert!(g.validate_working_color_domain().is_err());
    g.nodes[1].op = IrOp::Crop {
        left: 0.25,
        top: 0.25,
        right: 0.125,
        bottom: 0.25,
    };
    g.nodes[2].inputs = vec![(IrNodeId(0), OutPort(0)), (IrNodeId(1), OutPort(0))];
    assert!(g.validate_working_color_domain().is_err());
}

fn deflicker_reference(p: [f32; 4], gain: [f32; 3], band: [f32; 3], y: u32, h: u32) -> [f64; 4] {
    if p[3] <= 1e-6 || (gain == [1.0; 3] && band[1].abs() <= 1e-6) {
        return p.map(f64::from);
    }
    let a = [
        [1.705050992657983, -0.6217921206570056, -0.08325887200097853],
        [
            -0.13025641750704342,
            1.1408047365754022,
            -0.010548319068358056,
        ],
        [
            -0.024003356804618032,
            -0.12896897606497063,
            1.1529723328695884,
        ],
    ];
    let b = [
        [0.6130974024011878, 0.33952314618410606, 0.04737945141470665],
        [
            0.07019372246958168,
            0.9163538790573436,
            0.013452398473074122,
        ],
        [
            0.020615592882226925,
            0.10956977293813544,
            0.8698146341796377,
        ],
    ];

    let mul = |m: [[f64; 3]; 3], v: [f64; 3]| {
        m.map(|r| r.into_iter().zip(v).map(|(a, b)| a * b).sum::<f64>())
    };
    let bandgain = if band[1].abs() > 1e-6 {
        (1.0 / (1.0
            + band[1] as f64
                * (std::f64::consts::TAU * band[0] as f64 * y as f64 / h as f64 + band[2] as f64)
                    .cos()))
        .clamp(0.5, 2.0)
    } else {
        1.0
    };
    let mut rgb = mul(
        a,
        [
            p[0] as f64 * bandgain / p[3] as f64,
            p[1] as f64 * bandgain / p[3] as f64,
            p[2] as f64 * bandgain / p[3] as f64,
        ],
    );
    for c in 0..3 {
        let bounded = rgb[c].clamp(0.0, 1.0);
        let e = if bounded <= 0.0031308 {
            12.92 * bounded
        } else {
            1.055 * bounded.powf(1.0 / 2.4) - 0.055
        };
        let t = ((e - 0.75) / 0.25).clamp(0.0, 1.0);
        let g = if gain[c] > 1.0 {
            1.0 + (gain[c] as f64 - 1.0) * (1.0 - t * t)
        } else {
            gain[c] as f64
        };
        let value = (e * g).clamp(0.0, 1.0);
        let decoded = if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        };
        rgb[c] = rgb[c] - bounded + decoded;
    }
    let rgb = mul(b, rgb).map(|v| (v * p[3] as f64).clamp(-65504.0, 65504.0));
    [rgb[0], rgb[1], rgb[2], p[3] as f64]
}
fn deflicker_graph(
    rgba: [f32; 4],
    gain: [f32; 3],
    band: [f32; 3],
    hash: u128,
    canvas: Option<(u32, u32)>,
) -> FrameGraph {
    let mut g = directional_graph(rgba, [0.0; 2], hash, canvas);
    g.nodes[2].op = IrOp::NativeDeflicker { gain, band };
    g
}
#[test]
fn native_deflicker_resolved_operator_signed_hdr_bands_and_padding() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut hash = 10_500_000;
    for rgb in [
        [-0.5, 3.0, 0.25],
        [-2.0, -1.0, -4.0],
        [8.0, 16.0, 4.0],
        [0.7, 0.1, 0.4],
        [0.18; 3],
        [1.0; 3],
        [65504.0; 3],
        [-65504.0; 3],
    ] {
        for alpha in [0.0f32, 0.0009765625, 0.25, 1.0] {
            for (gain, band) in [
                ([1.0; 3], [0.0; 3]),
                ([1.25; 3], [0.0; 3]),
                ([0.75, 1.4, 1.0], [0.0; 3]),
                ([1.0; 3], [3.0, 0.25, 0.3]),
                ([0.85, 1.2, 1.1], [2.0, 0.8, 2.1]),
            ] {
                for (w, h) in [(1, 1), (7, 5), (16, 12)] {
                    hash += 8;
                    let g = deflicker_graph(
                        [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha],
                        gain,
                        band,
                        hash,
                        None,
                    );
                    assert!(g.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&g, (w, h), &mut EmptyProvider);
                    let mut sg = g.clone();
                    sg.output = Some(IrNodeId(1));
                    let cs = eval_cpu::evaluate(&sg, (w, h), &mut EmptyProvider);
                    let (out, tap) = evaluator.evaluate_with_tap_frame(
                        &g,
                        (w, h),
                        &mut NullFrameSource,
                        Some(IrNodeId(1)),
                    );
                    let out = out.unwrap();
                    let tap = tap.unwrap();
                    let input = read_texture_rgba16f(&gpu, &tap.texture, w, h);
                    let actual = read_texture_rgba16f(&gpu, &out.texture, w, h);
                    for (i, p) in actual.iter().enumerate() {
                        let e = deflicker_reference(input[i], gain, band, i as u32 / w, h);
                        let ce = deflicker_reference(cs.pixels[i], gain, band, i as u32 / w, h);
                        for c in 0..4 {
                            assert!(
                                (cpu.pixels[i][c] as f64 - ce[c]).abs()
                                    < 0.000003 * ce[c].abs().max(1.0),
                                "CPU {rgb:?}/{gain:?}/{band:?} {:?} {ce:?}",
                                cpu.pixels[i]
                            );
                            assert!(
                                p[c].is_finite()
                                    && (p[c] as f64 - e[c]).abs() < 0.003 * e[c].abs().max(1.0),
                                "GPU{rgb:?}/{alpha}/{gain:?}/{band:?}/{w}x{h} {p:?} {e:?}"
                            );
                        }
                        assert_eq!(p[3], input[i][3]);
                    }
                    let (pw, ph) = (out.texture.width(), out.texture.height());
                    let padding = read_texture_rgba16f(&gpu, &out.texture, pw, ph);
                    for y in 0..ph {
                        for x in 0..pw {
                            if x >= w || y >= h {
                                assert_eq!(padding[(y * pw + x) as usize], [0.0; 4]);
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn native_deflicker_resolved_operator_rejects_invalid_contract() {
    let good = deflicker_graph([0.1, 0.2, 0.3, 1.0], [1.0; 3], [0.0; 3], 10_600_000, None);
    for (gain, band) in [
        ([f32::NAN; 3], [0.0; 3]),
        ([0.0; 3], [0.0; 3]),
        ([11.0; 3], [0.0; 3]),
        ([1.0; 3], [0.5, 0.1, 0.0]),
        ([1.0; 3], [513.0, 0.1, 0.0]),
        ([1.0; 3], [2.0, 1.0, 0.0]),
        ([1.0; 3], [2.0, 0.1, f32::INFINITY]),
    ] {
        let mut g = good.clone();
        g.nodes[2].op = IrOp::NativeDeflicker { gain, band };
        assert!(g.validate_working_color_domain().is_err());
    }
    let mut g = good.clone();
    g.nodes[1].op = IrOp::NativeAcescct {
        direction: AcescctDirection::Encode,
    };
    assert!(g.validate_working_color_domain().is_err());
    let mut g = good;
    g.nodes[2].inputs.clear();
    assert!(g.validate_working_color_domain().is_err());
}

#[test]
fn native_deflicker_resolved_draft_cache() {
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu.clone());
    let gain = [0.8, 1.2, 1.0];
    let band = [2.0, 0.2, 0.3];
    let graph = deflicker_graph(
        [-0.25, 1.5, 0.125, 0.5],
        gain,
        band,
        10_700_000,
        Some((16, 12)),
    );
    for (w, h) in [(16, 12), (8, 3), (4, 6), (1, 1), (16, 12)] {
        let (out, tap) = evaluator.evaluate_with_tap_frame(
            &graph,
            (w, h),
            &mut NullFrameSource,
            Some(IrNodeId(1)),
        );
        let out = out.unwrap();
        let input = read_texture_rgba16f(&gpu, &tap.unwrap().texture, w, h);
        for (i, p) in read_texture_rgba16f(&gpu, &out.texture, w, h)
            .iter()
            .enumerate()
        {
            let e = deflicker_reference(input[i], gain, band, i as u32 / w, h);
            for c in 0..4 {
                assert!((p[c] as f64 - e[c]).abs() < 0.003 * e[c].abs().max(1.0));
            }
        }
    }
}

#[test]
fn bezier_matte_cpu_gpu_geometry_domains_alpha_and_boolean_composition() {
    use photonic_core::timeline::{BezierKnot, BezierWindow, GradeKeyMixMode};
    let gpu = GpuContext::request_blocking().expect("GPU required for Bezier qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let rectangle = BezierWindow {
        knots: vec![[0.25, 0.25], [0.75, 0.25], [0.75, 0.75], [0.25, 0.75]]
            .into_iter()
            .map(|anchor| BezierKnot {
                anchor,
                incoming: anchor,
                outgoing: anchor,
            })
            .collect(),
        feather: 0.0,
        invert: false,
    };
    let mut crossing = rectangle.clone();
    crossing.knots.swap(1, 2);
    let mut offset = BezierWindow::default();
    for knot in &mut offset.knots {
        for point in [&mut knot.anchor, &mut knot.incoming, &mut knot.outgoing] {
            point[0] -= 0.4;
        }
    }
    let mut serial = 2_000_000_u128;
    for domain in [
        WorkingColorDomain::LegacyLinearRec709,
        WorkingColorDomain::SceneLinearAcescg,
    ] {
        for (shape, base) in [
            rectangle.clone(),
            BezierWindow::default(),
            crossing.clone(),
            offset.clone(),
        ]
        .into_iter()
        .enumerate()
        {
            for alpha in [0.0, 0.5, 1.0] {
                for invert in [false, true] {
                    for feather in [0.0, 0.03] {
                        let mut window = base.clone();
                        window.invert = invert;
                        window.feather = feather;
                        let mut graph = FrameGraph {
                            working_color_domain: domain,
                            nodes: vec![
                                IrNode {
                                    op: IrOp::SolidColor {
                                        color: LinearColor {
                                            r: -2.0 * alpha,
                                            g: 8.0 * alpha,
                                            b: 0.3 * alpha,
                                            a: alpha,
                                        },
                                    },
                                    inputs: vec![],
                                    content_hash: ContentHash(serial),
                                },
                                IrNode {
                                    op: IrOp::BezierMatte {
                                        window: window.clone(),
                                    },
                                    inputs: vec![(IrNodeId(0), OutPort::default())],
                                    content_hash: ContentHash(serial + 1),
                                },
                            ],
                            output: Some(IrNodeId(0)),
                        };
                        serial += 10;
                        assert_eq!(graph.validate_working_color_domain(), Ok(()));
                        for (w, h) in [(33, 19), (17, 9), (33, 19)] {
                            let cpu = eval_cpu::evaluate_at(
                                &graph,
                                (w, h),
                                &mut EmptyProvider,
                                Some(IrNodeId(1)),
                            );
                            let texture = evaluator
                                .evaluate_with_tap(
                                    &graph,
                                    (w, h),
                                    &mut NullFrameSource,
                                    Some(IrNodeId(1)),
                                )
                                .1
                                .expect("Bezier matte tap")
                                .texture;
                            let actual = read_texture_rgba16f(&gpu, &texture, w, h);
                            for (index, (a, b)) in actual.iter().zip(&cpu.pixels).enumerate() {
                                for c in 0..4 {
                                    assert!(
                                        (a[c] - b[c]).abs() < 0.002,
                                        "shape {shape}, {w}x{h}, {index}: {a:?} vs {b:?}"
                                    );
                                }
                                assert_eq!(a[3], 1.0);
                                assert!(a.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
                                if shape == 0 {
                                    let x = (index as u32 % w) as f64 + 0.5;
                                    let y = (index as u32 / w) as f64 + 0.5;
                                    let dx = (x - w as f64 * 0.5).abs() - w as f64 * 0.25;
                                    let dy = (y - h as f64 * 0.5).abs() - h as f64 * 0.25;
                                    let distance =
                                        dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0);
                                    let f = (feather as f64 * w.min(h) as f64).max(0.5);
                                    let t = ((-distance + f) / (2.0 * f)).clamp(0.0, 1.0);
                                    let value = t * t * (3.0 - 2.0 * t);
                                    let value = if invert { 1.0 - value } else { value };
                                    assert!(
                                        (a[0] as f64 - value).abs() < 0.002,
                                        "analytic rectangle: {a:?} vs {value}"
                                    );
                                }
                            }
                        }
                        let mut other = window.clone();
                        other.invert = !other.invert;
                        graph.nodes.push(IrNode {
                            op: IrOp::BezierMatte { window: other },
                            inputs: vec![(IrNodeId(0), OutPort::default())],
                            content_hash: ContentHash(serial + 2),
                        });
                        for (mi, mode) in [
                            GradeKeyMixMode::Union,
                            GradeKeyMixMode::Intersect,
                            GradeKeyMixMode::Subtract,
                            GradeKeyMixMode::Multiply,
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            let mix = IrNode {
                                op: IrOp::GradeKeyMix { mode },
                                inputs: vec![
                                    (IrNodeId(1), OutPort::default()),
                                    (IrNodeId(2), OutPort::default()),
                                ],
                                content_hash: ContentHash(serial + 3 + mi as u128),
                            };
                            if graph.nodes.len() == 3 {
                                graph.nodes.push(mix);
                            } else {
                                graph.nodes[3] = mix;
                            }
                            let cpu = eval_cpu::evaluate_at(
                                &graph,
                                (17, 9),
                                &mut EmptyProvider,
                                Some(IrNodeId(3)),
                            );
                            let texture = evaluator
                                .evaluate_with_tap(
                                    &graph,
                                    (17, 9),
                                    &mut NullFrameSource,
                                    Some(IrNodeId(3)),
                                )
                                .1
                                .unwrap()
                                .texture;
                            for (a, b) in read_texture_rgba16f(&gpu, &texture, 17, 9)
                                .iter()
                                .zip(&cpu.pixels)
                            {
                                for c in 0..4 {
                                    assert!((a[c] - b[c]).abs() < 0.002);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn bezier_matte_budget_failure_is_visible_and_recovers() {
    use photonic_core::timeline::BezierWindow;
    let gpu = GpuContext::request_blocking().expect("GPU required");
    let mut evaluator = Evaluator::new(gpu);
    let graph = FrameGraph {
        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
        nodes: vec![
            IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: 0.1,
                        g: 0.2,
                        b: 0.3,
                        a: 1.0,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(2_100_000),
            },
            IrNode {
                op: IrOp::BezierMatte {
                    window: BezierWindow::default(),
                },
                inputs: vec![(IrNodeId(0), OutPort::default())],
                content_hash: ContentHash(2_100_001),
            },
        ],
        output: Some(IrNodeId(0)),
    };
    assert!(evaluator
        .evaluate_with_tap(
            &graph,
            (4096, 2160),
            &mut NullFrameSource,
            Some(IrNodeId(1))
        )
        .1
        .is_none());
    assert!(evaluator
        .computation_errors()
        .iter()
        .any(|e| e.contains("work budget")));
    assert!(evaluator
        .evaluate_with_tap(&graph, (64, 32), &mut NullFrameSource, Some(IrNodeId(1)))
        .1
        .is_some());
    assert!(evaluator.computation_errors().is_empty());
}

#[test]
fn native_generic_merge_saturates_finite_storage_extremes() {
    use photonic_core::layer::BlendMode::*;
    let gpu = GpuContext::request_blocking().expect("GPU required for Native merge storage check");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut failures = Vec::new();
    let mut id = 990000u128;
    for mode in [
        Normal, Multiply, Screen, Overlay, Darken, Lighten, HardLight, SoftLight, Difference,
        Exclusion, PinLight,
    ] {
        for backdrop_sign in [-1.0f32, 1.0] {
            for alpha in [1.0f32, 0.5, 0.000002] {
                for opacity in [0.0f32, 0.5, 1.0] {
                    let solid = |hash, sign: f32| IrNode {
                        op: IrOp::SolidColor {
                            color: LinearColor {
                                r: 65504.0 * sign,
                                g: -65504.0 * sign,
                                b: 32752.0,
                                a: alpha,
                            },
                        },
                        inputs: vec![],
                        content_hash: ContentHash(hash),
                    };
                    let graph = FrameGraph {
                        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                        nodes: vec![
                            solid(id, 1.0),
                            solid(id + 1, backdrop_sign),
                            IrNode {
                                op: IrOp::Merge { mode, opacity },
                                inputs: vec![
                                    (IrNodeId(0), OutPort::default()),
                                    (IrNodeId(1), OutPort::default()),
                                ],
                                content_hash: ContentHash(id + 2),
                            },
                        ],
                        output: Some(IrNodeId(2)),
                    };
                    id += 3;
                    assert!(graph.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&graph, (1, 1), &mut EmptyProvider).pixels[0];
                    let texture = evaluator
                        .evaluate(&graph, (1, 1), &mut NullFrameSource)
                        .unwrap();
                    let actual = read_texture_rgba16f(&gpu, &texture, 1, 1)[0];
                    for channel in 0..3 {
                        if !cpu[channel].is_finite()
                            || !actual[channel].is_finite()
                            || (cpu[channel] - actual[channel]).abs() > 64.0
                        {
                            failures.push(format!("{mode:?} alpha={alpha} opacity={opacity} channel={channel}: CPU={cpu:?}, GPU={actual:?}"));
                            break;
                        }
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} storage failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn native_soft_light_finite_overlap_matches_f64_with_quantized_sources() {
    fn reference(top: [f32; 4], bottom: [f32; 4], opacity: f32) -> [f64; 4] {
        let sa = top[3] as f64;
        let ba = bottom[3] as f64;
        let o = opacity as f64;
        if o == 0.0 {
            return bottom.map(f64::from);
        }
        let a = sa * o;
        let mut out = [0.0; 4];
        for c in 0..3 {
            let s = if top[3] > 1e-6 {
                top[c] as f64 / sa
            } else {
                0.0
            };
            let b = if bottom[3] > 1e-6 {
                bottom[c] as f64 / ba
            } else {
                0.0
            };
            let blend = if s <= 0.5 {
                b - (1.0 - 2.0 * s) * b * (1.0 - b)
            } else if b <= 0.25 {
                b + (2.0 * s - 1.0) * (16.0 * b.powi(3) - 12.0 * b.powi(2) + 3.0 * b)
            } else {
                b + (2.0 * s - 1.0) * (b.sqrt() - b)
            };
            out[c] = (a * (1.0 - ba) * s + a * ba * blend + (1.0 - a) * bottom[c] as f64)
                .clamp(-65504.0, 65504.0);
        }
        out[3] = a + ba * (1.0 - a);
        out
    }
    let gpu = GpuContext::request_blocking().expect("GPU required for soft-light stability oracle");
    let mut evaluator = Evaluator::new(gpu.clone());
    let mut id = 1000000u128;
    let mut cases = 0;
    for (source, backdrop) in [
        ([65504.0, -65504.0, 32752.0], [-65504.0, 65504.0, 32752.0]),
        ([-65504.0, 65504.0, -32752.0], [65504.0, -65504.0, -32752.0]),
        ([0.75, 0.25, 0.5], [0.25, 0.5, 0.75]),
        ([0.0, 0.5, 1.0], [0.0, 0.25, 1.0]),
        ([4.0, -2.0, 0.125], [-3.0, 2.0, 0.75]),
    ] {
        for (sa, ba) in [
            (1.0, 1.0),
            (0.5, 0.5),
            (0.000002, 0.000002),
            (0.000001, 0.000002),
            (0.000002, 0.000001),
            (0.0, 1.0),
            (1.0, 0.0),
            (0.0, 0.0),
        ] {
            for opacity in [0.0, 1e-30, 0.125, 0.5, 1.0] {
                let top = [source[0], source[1], source[2], sa];
                let bottom = [backdrop[0], backdrop[1], backdrop[2], ba];
                let solid = |p: [f32; 4], hash| IrNode {
                    op: IrOp::SolidColor {
                        color: LinearColor {
                            r: p[0],
                            g: p[1],
                            b: p[2],
                            a: p[3],
                        },
                    },
                    inputs: vec![],
                    content_hash: ContentHash(hash),
                };
                let mut graph = FrameGraph {
                    working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                    nodes: vec![
                        solid(top, id),
                        solid(bottom, id + 1),
                        IrNode {
                            op: IrOp::Merge {
                                mode: BlendMode::SoftLight,
                                opacity,
                            },
                            inputs: vec![
                                (IrNodeId(0), OutPort::default()),
                                (IrNodeId(1), OutPort::default()),
                            ],
                            content_hash: ContentHash(id + 2),
                        },
                    ],
                    output: Some(IrNodeId(2)),
                };
                id += 3;
                let cpu = eval_cpu::evaluate(&graph, (1, 1), &mut EmptyProvider).pixels[0];
                let expected_cpu = reference(top, bottom, opacity);
                let mut stored = [[0.0; 4]; 2];
                for (n, pixel) in stored.iter_mut().enumerate() {
                    graph.output = Some(IrNodeId(n as u32));
                    let tex = evaluator
                        .evaluate(&graph, (1, 1), &mut NullFrameSource)
                        .unwrap();
                    *pixel = read_texture_rgba16f(&gpu, &tex, 1, 1)[0];
                    let original = if n == 0 { top } else { bottom };
                    for c in 0..4 {
                        let tolerance = if c == 3 {
                            6e-8
                        } else {
                            (original[c].abs() * 0.001).max(6e-8)
                        };
                        assert!(
                            pixel[c].is_finite() && (pixel[c] - original[c]).abs() <= tolerance,
                            "source quantization: {pixel:?} vs {original:?}"
                        );
                    }
                }
                graph.output = Some(IrNodeId(2));
                let tex = evaluator
                    .evaluate(&graph, (1, 1), &mut NullFrameSource)
                    .unwrap();
                let actual = read_texture_rgba16f(&gpu, &tex, 1, 1)[0];
                let expected = reference(stored[0], stored[1], opacity);
                for c in 0..4 {
                    let cpu_tol = if c == 3 {
                        1e-7
                    } else {
                        (expected_cpu[c].abs() * 2e-7).max(2e-6)
                    };
                    let gpu_tol = if c == 3 {
                        0.0005
                    } else {
                        (expected[c].abs() * 0.001).max(0.01)
                    };
                    assert!(
                        cpu[c].is_finite() && (cpu[c] as f64 - expected_cpu[c]).abs() <= cpu_tol,
                        "CPU {top:?} {bottom:?} o={opacity} {cpu:?} vs {expected_cpu:?}"
                    );
                    assert!(
                        actual[c].is_finite() && (actual[c] as f64 - expected[c]).abs() <= gpu_tol,
                        "GPU {top:?} {bottom:?} o={opacity} {actual:?} vs {expected:?}"
                    );
                }
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 200);
}

#[test]
fn native_all_blends_agree_at_exact_half_alpha_cutoff_neighbors() {
    use photonic_core::layer::BlendMode::*;
    use photonic_video::graph::ir::{
        native_arithmetic_blend, native_bounded_blend, native_luminance_blend,
    };
    let gpu =
        GpuContext::request_blocking().expect("GPU required for Native alpha-cutoff qualification");
    let mut evaluator = Evaluator::new(gpu.clone());
    let low = 16.0f32 / 16777216.0;
    let high = 17.0f32 / 16777216.0;
    let mut id = 1100000u128;
    let mut failures = Vec::new();
    let mut cases = 0;
    for mode in [
        Normal,
        Multiply,
        Screen,
        Overlay,
        Darken,
        Lighten,
        ColorDodge,
        ColorBurn,
        HardLight,
        SoftLight,
        Difference,
        Exclusion,
        Hue,
        Saturation,
        Color,
        Luminosity,
        LinearDodge,
        LinearBurn,
        Subtract,
        Divide,
        VividLight,
        LinearLight,
        PinLight,
        HardMix,
        DarkerColor,
        LighterColor,
    ] {
        for (sa, ba) in [
            (0.0, 1.0),
            (1.0, 0.0),
            (low, low),
            (high, high),
            (low, high),
            (high, low),
            (low, 0.5),
            (0.5, low),
        ] {
            for opacity in [0.0, 0.5, 1.0] {
                for signed in [false, true] {
                    let top = if sa == 0.0 {
                        [0.0; 4]
                    } else {
                        [0.5, if signed { -0.25 } else { 0.25 }, 1.0, sa]
                    };
                    let bottom = if ba == 0.0 {
                        [0.0; 4]
                    } else {
                        [if signed { -0.125 } else { 0.125 }, 0.375, 0.75, ba]
                    };
                    let solid = |p: [f32; 4], hash| IrNode {
                        op: IrOp::SolidColor {
                            color: LinearColor {
                                r: p[0],
                                g: p[1],
                                b: p[2],
                                a: p[3],
                            },
                        },
                        inputs: vec![],
                        content_hash: ContentHash(hash),
                    };
                    let op = if native_arithmetic_blend(mode) {
                        IrOp::NativeArithmeticMerge { mode, opacity }
                    } else if native_bounded_blend(mode) {
                        IrOp::NativeBoundedMerge { mode, opacity }
                    } else if native_luminance_blend(mode) {
                        IrOp::NativeLuminanceMerge { mode, opacity }
                    } else {
                        IrOp::Merge { mode, opacity }
                    };
                    let mut graph = FrameGraph {
                        working_color_domain: WorkingColorDomain::SceneLinearAcescg,
                        nodes: vec![
                            solid(top, id),
                            solid(bottom, id + 1),
                            IrNode {
                                op,
                                inputs: vec![
                                    (IrNodeId(0), OutPort::default()),
                                    (IrNodeId(1), OutPort::default()),
                                ],
                                content_hash: ContentHash(id + 2),
                            },
                        ],
                        output: Some(IrNodeId(2)),
                    };
                    id += 3;
                    assert!(graph.validate_working_color_domain().is_ok());
                    let cpu = eval_cpu::evaluate(&graph, (1, 1), &mut EmptyProvider).pixels[0];
                    for (n, original) in [top, bottom].into_iter().enumerate() {
                        graph.output = Some(IrNodeId(n as u32));
                        let tex = evaluator
                            .evaluate(&graph, (1, 1), &mut NullFrameSource)
                            .unwrap();
                        assert_eq!(
                            read_texture_rgba16f(&gpu, &tex, 1, 1)[0],
                            original,
                            "fixture must be exactly representable"
                        );
                    }
                    graph.output = Some(IrNodeId(2));
                    let tex = evaluator
                        .evaluate(&graph, (1, 1), &mut NullFrameSource)
                        .unwrap();
                    let actual = read_texture_rgba16f(&gpu, &tex, 1, 1)[0];
                    for c in 0..4 {
                        let tolerance = if c == 3 {
                            (cpu[c].abs() * 0.001).max(6e-8)
                        } else {
                            (cpu[c].abs() * 0.001).max(0.003)
                        };
                        if !cpu[c].is_finite()
                            || !actual[c].is_finite()
                            || (cpu[c] - actual[c]).abs() > tolerance
                        {
                            failures.push(format!("{mode:?} sa={sa} ba={ba} opacity={opacity} signed={signed}: CPU={cpu:?} GPU={actual:?}"));
                            break;
                        }
                    }
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 1248);
    assert!(
        failures.is_empty(),
        "{} exact-source cutoff mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn mixed_caption_word_metrics_shape_render_and_scale_together() {
    use photonic_core::timeline::TextDirection;
    use photonic_render::caption::{CaptionCompositor, CaptionCueRun, CaptionWordRun};
    let gpu = GpuContext::request_blocking().expect("GPU required for mixed caption metrics");
    let compositor =
        CaptionCompositor::new(gpu.device(), gpu.queue(), wgpu::TextureFormat::Rgba16Float);
    let original = CaptionCueRun {
        direction: TextDirection::Ltr,
        words: vec![
            CaptionWordRun {
                text: "MMMM".into(),
                font_family: "sans-serif".into(),
                font_weight: 400,
                color: [255, 0, 0, 255],
                metrics: None,
            },
            CaptionWordRun {
                text: "MMMM".into(),
                font_family: "sans-serif".into(),
                font_weight: 400,
                color: [0, 255, 0, 255],
                metrics: Some([40.0, 48.0]),
            },
        ],
        font_size: 20.0,
        line_height_mul: 1.2,
        anchor: [0.5, 0.1],
        max_width: 0.95,
    };
    for scale in [1.0, 0.5] {
        let mut cue = original.clone();
        cue.scale_metrics(scale);
        let (w, h) = ((480.0 * scale) as u32, (160.0 * scale) as u32);
        let bounds = compositor.word_bounds_checked(&cue, w, h).unwrap();
        assert_eq!(bounds.len(), 2);
        let small_width = bounds[0].bounds[2] - bounds[0].bounds[0];
        let big_width = bounds[1].bounds[2] - bounds[1].bounds[0];
        assert!((big_width / small_width - 2.0).abs() < 0.05, "{bounds:?}");
        let target = gpu.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("mixed word metrics"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        gpu.queue().write_texture(
            target.as_image_copy(),
            &vec![0; (w * h * 8) as usize],
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(w * 8),
                rows_per_image: Some(h),
            },
            target.size(),
        );
        compositor
            .composite_checked(
                gpu.device(),
                gpu.queue(),
                &target,
                std::slice::from_ref(&cue),
                w,
                h,
            )
            .unwrap();
        let pixels = read_texture_rgba16f(&gpu, &target, w, h);
        let height = |channel: usize| {
            let ys: Vec<_> = pixels
                .iter()
                .enumerate()
                .filter(|(_, p)| p[channel] > 0.2)
                .map(|(i, _)| i as u32 / w)
                .collect();
            assert!(!ys.is_empty());
            ys.iter().max().unwrap() - ys.iter().min().unwrap() + 1
        };
        assert!(height(1) as f32 > height(0) as f32 * 1.7);
        for bad in [f32::NAN, f32::INFINITY, -1.0, 0.0, 5000.0] {
            let mut invalid = cue.clone();
            invalid.words[1].metrics = Some([bad, 48.0]);
            assert!(compositor.word_bounds_checked(&invalid, w, h).is_err());
            assert!(compositor
                .composite_checked(gpu.device(), gpu.queue(), &target, &[invalid], w, h)
                .is_err());
        }
    }
}
