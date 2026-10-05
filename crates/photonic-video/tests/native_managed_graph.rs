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
        mode: BlendMode::Multiply,
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
