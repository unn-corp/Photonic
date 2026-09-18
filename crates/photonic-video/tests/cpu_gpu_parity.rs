//! E-9(b) — CPU/GPU IR enum-equivalence sweep (26 §8 / §20 definition-of-done).
//!
//! 26 §20 exempts the K-0 seams from the user-verb/undo/MCP definition of done and
//! replaces it with "contract-written-into-owning-doc + a CI test that enforces it".
//! For E-9 (CPU/GPU agreement) THIS FILE is that CI artefact: it mechanically walks
//! every variant of every enum the frame-graph IR carries and asserts the CPU
//! reference (`graph::eval_cpu::evaluate`) and the wgpu evaluator (`graph::eval`)
//! produce the same pixels.
//!
//! Coverage (each behind an exhaustive `match` on the enum, so a NEW variant added
//! without a parity case fails to COMPILE — the whole point of the harness):
//!   * `BlendMode`      — all 26 (K-0.3a; the row that was the live wrong-pixels bug)
//!   * `graph::ir::Sampling`  — Nearest / Bilinear (Transform2D)
//!   * `graph::ir::FitMode`   — Fit / Fill / Stretch (Resize)
//!   * `GradeOpKind`    — every resolved grade op
//!   * `LutInterp`      — Trilinear / Tetrahedral
//!   * `TransitionKind` — the 5 catalog kinds (compile-time → Merge/dip lowering)
//!
//! Tolerances: max-abs channel diff ≤ 1e-3 for algebraic ops; the looser 11 §1.2
//! GPU-vs-CPU tier (PSNR ≥ 35 dB) for the resampling (sampling) op.
//!
//! `#[non_exhaustive]` enums (`GradeOpKind`, `TransitionKind`) come from another
//! crate and CANNOT be matched without a wildcard — the compiler forces one — so
//! those matches carry a documented `_`/`Unknown` arm. Every non-`#[non_exhaustive]`
//! IR enum keeps a wildcard-free match.
//!
//! Self-skips (with an eprintln) when no GPU adapter is available, e.g. CI without a
//! GPU (`GpuContext::request_blocking()` → `None`).

use photonic_core::layer::BlendMode;
use photonic_core::timeline::grade::{
    CdlParams, Grade, GradeMask, GradeOp, GradeOpKind, GradeOpParams, LutInterp, WindowShape,
};
use photonic_core::timeline::{
    Clip, ClipSource, EffectKind, FrameRate, PropPath, PropValue, Sequence, TimelineProject, Track,
    TrackKind, Transition, TransitionKind, TransitionParams, UnknownTag,
};
use photonic_core::Color;

use photonic_video::contract::{AssetId, ResolvedParams, Tick, VectorRef, VectorStateKey};
use photonic_video::graph::compile::{compile, CompiledFrame, Quality};
use photonic_video::graph::eval::{
    read_texture_rgba16f, Evaluator, GpuContext, GpuFrame, GpuFrameSource, NullFrameSource,
};
use photonic_video::graph::eval_cpu::{self, EmptyProvider, FrameProvider};
use photonic_video::graph::ir::{
    ContentHash, FitMode, FrameGraph, IrNode, IrNodeId, IrOp, LinearColor, OutPort, Sampling,
};
use photonic_video::graph::ops::Image;
use photonic_video::testing::frame_compare::measure;

const CANVAS: (u32, u32) = (8, 8);

/// Shared GPU context, or a skip. All tests short-circuit through this.
fn gpu_or_skip(what: &str) -> Option<GpuContext> {
    match GpuContext::request_blocking() {
        Some(g) => Some(g),
        None => {
            eprintln!("no GPU adapter — skipping {what}");
            None
        }
    }
}

/// A deliberately awkward canvas: non-square, and neither axis is a multiple of
/// the pool's 64px bucket — 100 rounds to 128 (x1.28) and 40 rounds to 64
/// (x1.60), so the two axes scale by DIFFERENT ratios.
///
/// [`CANVAS`] cannot catch a physical-vs-logical confusion: 8x8 buckets to a
/// *square* 64x64, where both axes scale identically and anything normalized
/// over the target stays centred and symmetric. Two shipped wrong-pixels bugs
/// hid behind exactly that (grade power windows in `2670c2d`, and the GPU scope
/// kernels), so anything that consumes normalized spatial coordinates should be
/// swept here as well as at `CANVAS`.
const AWKWARD_CANVAS: (u32, u32) = (100, 40);

/// GPU-evaluate `graph` at `canvas` with the given source, reading back the output.
fn eval_gpu_at(
    gpu: &GpuContext,
    graph: &FrameGraph,
    source: &mut dyn GpuFrameSource,
    canvas: (u32, u32),
) -> Image {
    let mut eval = Evaluator::new(gpu.clone());
    let tex = eval
        .evaluate(graph, canvas, source)
        .expect("graph produced an output texture");
    let px = read_texture_rgba16f(gpu, &tex, canvas.0, canvas.1);
    Image {
        width: canvas.0,
        height: canvas.1,
        pixels: px,
    }
}

/// GPU-evaluate `graph` at `CANVAS` with the given source, reading back the output.
fn eval_gpu(gpu: &GpuContext, graph: &FrameGraph, source: &mut dyn GpuFrameSource) -> Image {
    eval_gpu_at(gpu, graph, source, CANVAS)
}

/// Assert the CPU and GPU evaluators agree within `tol` (max abs channel diff) at
/// `canvas`, for a media-free (`NullFrameSource` / `EmptyProvider`) graph.
fn assert_parity_solid_at(
    gpu: &GpuContext,
    label: &str,
    graph: &FrameGraph,
    tol: f32,
    canvas: (u32, u32),
) {
    let cpu = eval_cpu::evaluate(graph, canvas, &mut EmptyProvider);
    let g = eval_gpu_at(gpu, graph, &mut NullFrameSource, canvas);
    assert_pixels_within(label, &cpu, &g, tol);
}

/// Assert the CPU and GPU evaluators agree within `tol` (max abs channel diff) for
/// a media-free (`NullFrameSource` / `EmptyProvider`) graph.
fn assert_parity_solid(gpu: &GpuContext, label: &str, graph: &FrameGraph, tol: f32) {
    assert_parity_solid_at(gpu, label, graph, tol, CANVAS);
}

fn assert_pixels_within(label: &str, cpu: &Image, gpu: &Image, tol: f32) {
    for (i, (c, g)) in cpu.pixels.iter().zip(&gpu.pixels).enumerate() {
        for k in 0..4 {
            assert!(
                (c[k] - g[k]).abs() <= tol,
                "{label}: pixel {i} channel {k}: cpu {} vs gpu {} (tol {tol})",
                c[k],
                g[k]
            );
        }
    }
}

// ── BlendMode (26) — K-0.3a ────────────────────────────────────────────────────

/// Human label per mode, via a wildcard-free exhaustive `match`: a new `BlendMode`
/// variant makes this fail to compile until it is given a parity row.
fn blend_label(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Normal => "normal",
        BlendMode::Multiply => "multiply",
        BlendMode::Screen => "screen",
        BlendMode::Overlay => "overlay",
        BlendMode::Darken => "darken",
        BlendMode::Lighten => "lighten",
        BlendMode::ColorDodge => "color_dodge",
        BlendMode::ColorBurn => "color_burn",
        BlendMode::HardLight => "hard_light",
        BlendMode::SoftLight => "soft_light",
        BlendMode::Difference => "difference",
        BlendMode::Exclusion => "exclusion",
        BlendMode::Hue => "hue",
        BlendMode::Saturation => "saturation",
        BlendMode::Color => "color",
        BlendMode::Luminosity => "luminosity",
        BlendMode::LinearDodge => "linear_dodge",
        BlendMode::LinearBurn => "linear_burn",
        BlendMode::Subtract => "subtract",
        BlendMode::Divide => "divide",
        BlendMode::VividLight => "vivid_light",
        BlendMode::LinearLight => "linear_light",
        BlendMode::PinLight => "pin_light",
        BlendMode::HardMix => "hard_mix",
        BlendMode::DarkerColor => "darker_color",
        BlendMode::LighterColor => "lighter_color",
    }
}

const ALL_BLEND_MODES: [BlendMode; 26] = [
    BlendMode::Normal,
    BlendMode::Multiply,
    BlendMode::Screen,
    BlendMode::Overlay,
    BlendMode::Darken,
    BlendMode::Lighten,
    BlendMode::ColorDodge,
    BlendMode::ColorBurn,
    BlendMode::HardLight,
    BlendMode::SoftLight,
    BlendMode::Difference,
    BlendMode::Exclusion,
    BlendMode::Hue,
    BlendMode::Saturation,
    BlendMode::Color,
    BlendMode::Luminosity,
    BlendMode::LinearDodge,
    BlendMode::LinearBurn,
    BlendMode::Subtract,
    BlendMode::Divide,
    BlendMode::VividLight,
    BlendMode::LinearLight,
    BlendMode::PinLight,
    BlendMode::HardMix,
    BlendMode::DarkerColor,
    BlendMode::LighterColor,
];

fn merge_graph(top: LinearColor, bottom: LinearColor, mode: BlendMode) -> FrameGraph {
    FrameGraph {
        nodes: vec![
            IrNode {
                op: IrOp::SolidColor { color: top },
                inputs: vec![],
                content_hash: ContentHash(10),
            },
            IrNode {
                op: IrOp::SolidColor { color: bottom },
                inputs: vec![],
                content_hash: ContentHash(11),
            },
            IrNode {
                op: IrOp::Merge { mode, opacity: 1.0 },
                inputs: vec![
                    (IrNodeId(0), OutPort::default()),
                    (IrNodeId(1), OutPort::default()),
                ],
                content_hash: ContentHash(12),
            },
        ],
        output: Some(IrNodeId(2)),
    }
}

#[test]
fn blend_mode_cpu_gpu_parity() {
    let Some(gpu) = gpu_or_skip("BlendMode parity") else {
        return;
    };
    // Semi-transparent premultiplied top over an opaque and a transparent backdrop.
    // Colours sit clear of every mode's discontinuities (notably HardMix's 0.5
    // threshold, an inherent 0↔1 flip a sub-ULP CPU/GPU delta would trip).
    let top = LinearColor {
        r: 0.35,
        g: 0.175,
        b: 0.275,
        a: 0.5,
    };
    let opaque = LinearColor {
        r: 0.25,
        g: 0.6,
        b: 0.4,
        a: 1.0,
    };
    let transparent = LinearColor {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };
    for mode in ALL_BLEND_MODES {
        for bottom in [opaque, transparent] {
            assert_parity_solid(
                &gpu,
                &format!("blend/{}", blend_label(mode)),
                &merge_graph(top, bottom, mode),
                1e-3,
            );
        }
    }
}

// ── Sampling (Nearest / Bilinear) — Transform2D ─────────────────────────────────

/// A deterministic non-uniform pattern so the sampler actually interpolates.
fn patterned_image(width: u32, height: u32) -> Image {
    let mut image = Image::new(width, height);
    for y in 0..height {
        for x in 0..width {
            image.pixels[(y * width + x) as usize] = [
                (x % 2) as f32,
                (y % 2) as f32,
                ((x + 2 * y) % 3 == 0) as u8 as f32,
                1.0,
            ];
        }
    }
    image
}

fn upload_pattern(gpu: &GpuContext, image: &Image) -> std::sync::Arc<wgpu::Texture> {
    let texture = gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("parity_pattern"),
        size: wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut half = Vec::with_capacity(image.pixels.len() * 4);
    for pixel in &image.pixels {
        for channel in pixel {
            // Fixtures contain zero or positive normal half-float values.
            let bits = channel.to_bits();
            let exponent = ((bits >> 23) & 0xff) as i32 - 112;
            half.push(if exponent <= 0 {
                0
            } else {
                ((exponent as u16) << 10) | ((bits & 0x7fffff) >> 13) as u16
            });
        }
    }
    gpu.queue().write_texture(
        texture.as_image_copy(),
        bytemuck::cast_slice(&half),
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(image.width * 8),
            rows_per_image: Some(image.height),
        },
        wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        },
    );
    std::sync::Arc::new(texture)
}

struct PatternGpuSource {
    frame: GpuFrame,
}
impl GpuFrameSource for PatternGpuSource {
    fn video_texture(&mut self, _: &GpuContext, _: AssetId, _: Tick, _: bool) -> Option<GpuFrame> {
        Some(self.frame.clone())
    }
    fn still_texture(&mut self, _: &GpuContext, _: AssetId, _: u32, _: u32) -> Option<GpuFrame> {
        Some(self.frame.clone())
    }
    fn vector_texture(
        &mut self,
        _: &GpuContext,
        _: VectorRef,
        _: VectorStateKey,
        _: u32,
        _: u32,
    ) -> Option<GpuFrame> {
        Some(self.frame.clone())
    }
}

struct PatternCpuSource {
    image: Image,
}
impl FrameProvider for PatternCpuSource {
    fn decode_video(&mut self, _: AssetId, _: Tick, _: bool, _: u32, _: u32) -> Image {
        self.image.clone()
    }
    fn decode_still(&mut self, _: AssetId, _: u32, _: u32) -> Image {
        self.image.clone()
    }
    fn raster_vector(&mut self, _: VectorRef, _: VectorStateKey, _: u32, _: u32) -> Image {
        self.image.clone()
    }
}

const ALL_SAMPLING: [Sampling; 2] = [Sampling::Bilinear, Sampling::Nearest];

fn sampling_label(s: Sampling) -> &'static str {
    // Wildcard-free: a new Sampling variant fails to compile here.
    match s {
        Sampling::Bilinear => "bilinear",
        Sampling::Nearest => "nearest",
    }
}

#[test]
fn sampling_cpu_gpu_parity() {
    let Some(gpu) = gpu_or_skip("Sampling parity") else {
        return;
    };
    let image = patterned_image(CANVAS.0, CANVAS.1);
    let mat = glam::Mat3::from_translation(glam::Vec2::new(1.25, -0.75))
        * glam::Mat3::from_angle(0.23)
        * glam::Mat3::from_scale(glam::Vec2::new(1.2, 0.8));
    for sampling in ALL_SAMPLING {
        let graph = FrameGraph {
            nodes: vec![
                IrNode {
                    op: IrOp::DecodeStill {
                        asset: AssetId::new(),
                    },
                    inputs: vec![],
                    content_hash: ContentHash(20),
                },
                IrNode {
                    op: IrOp::Transform2D { mat, sampling },
                    inputs: vec![(IrNodeId(0), OutPort::default())],
                    content_hash: ContentHash(21),
                },
            ],
            output: Some(IrNodeId(1)),
        };
        let cpu = eval_cpu::evaluate(
            &graph,
            CANVAS,
            &mut PatternCpuSource {
                image: image.clone(),
            },
        );
        let mut src = PatternGpuSource {
            frame: GpuFrame::new(upload_pattern(&gpu, &image), image.width, image.height),
        };
        let g = eval_gpu(&gpu, &graph, &mut src);
        // Sampling tier (11 §1.2): the looser GPU-vs-CPU PSNR bound, not 1e-3.
        let metric = measure(&cpu, &g).expect("measurable");
        assert!(
            metric.psnr_db >= 35.0,
            "sampling/{}: PSNR {:.2} dB < 35 (max_abs {:.5})",
            sampling_label(sampling),
            metric.psnr_db,
            metric.max_abs
        );
    }
}

// ── StabilizeWarp (D-12) ────────────────────────────────────────────────────────

/// D-12 gyro-stabilization warp parity (22 §6.7: "CPU/GPU warp goldens pass on
/// grid/lens fixtures").
///
/// Note for future maintainers: unlike the enum-driven cases above, adding an
/// `IrOp` variant does **not** fail this file to compile — the exhaustive
/// matches here are over *parameter* enums (`Sampling`, `BlendMode`, …), not
/// over `IrOp`. A new op needs its parity case added deliberately, as this one
/// was.
///
/// Both lens models are covered because they exercise genuinely different
/// shader paths: the pinhole branch is a few multiplies, the fisheye branch
/// runs a fixed-count Newton inversion whose `f32` behaviour is the most likely
/// place for CPU and GPU to diverge.
#[test]
fn stabilize_warp_cpu_gpu_parity() {
    use photonic_video::graph::ir::StabilizeWarp;

    let Some(gpu) = gpu_or_skip("StabilizeWarp parity") else {
        return;
    };
    let image = patterned_image(CANVAS.0, CANVAS.1);
    let (w, h) = (CANVAS.0 as f32, CANVAS.1 as f32);

    // A rotation big enough to move real pixels around, about all three axes so
    // no term of the matrix is left untested.
    let q = glam::Quat::from_euler(glam::EulerRot::XYZ, 0.04, 0.07, 0.03);
    let m = glam::Mat3::from_quat(q);
    let rotation = [
        m.x_axis.x, m.y_axis.x, m.z_axis.x, m.x_axis.y, m.y_axis.y, m.z_axis.y, m.x_axis.z,
        m.y_axis.z, m.z_axis.z,
    ];

    let cases = [
        (
            "pinhole",
            StabilizeWarp {
                rotation,
                zoom: 1.15,
                intrinsics: [0.5, 0.5 * w / h, 0.5, 0.5],
                k: [0.0; 4],
                fisheye: false,
                transparent_edges: false,
            },
        ),
        (
            "fisheye",
            StabilizeWarp {
                rotation,
                zoom: 1.15,
                intrinsics: [0.45, 0.45 * w / h, 0.5, 0.5],
                k: [0.02, -0.004, 0.0007, -0.00005],
                fisheye: true,
                transparent_edges: false,
            },
        ),
        (
            "transparent-edges",
            StabilizeWarp {
                rotation,
                zoom: 1.0, // no crop, so edges really are exposed
                intrinsics: [0.5, 0.5 * w / h, 0.5, 0.5],
                k: [0.0; 4],
                fisheye: false,
                transparent_edges: true,
            },
        ),
    ];

    for (label, warp) in cases {
        for sampling in ALL_SAMPLING {
            let graph = FrameGraph {
                nodes: vec![
                    IrNode {
                        op: IrOp::DecodeStill {
                            asset: AssetId::new(),
                        },
                        inputs: vec![],
                        content_hash: ContentHash(40),
                    },
                    IrNode {
                        op: IrOp::StabilizeWarp { warp, sampling },
                        inputs: vec![(IrNodeId(0), OutPort::default())],
                        content_hash: ContentHash(41),
                    },
                ],
                output: Some(IrNodeId(1)),
            };
            let cpu = eval_cpu::evaluate(
                &graph,
                CANVAS,
                &mut PatternCpuSource {
                    image: image.clone(),
                },
            );
            let mut src = PatternGpuSource {
                frame: GpuFrame::new(upload_pattern(&gpu, &image), image.width, image.height),
            };
            let g = eval_gpu(&gpu, &graph, &mut src);
            // Resampling tier (11 §1.2): PSNR, not the 1e-3 algebraic bound.
            let metric = measure(&cpu, &g).expect("measurable");
            assert!(
                metric.psnr_db >= 35.0,
                "stabilize/{}/{}: PSNR {:.2} dB < 35 (max_abs {:.5})",
                label,
                sampling_label(sampling),
                metric.psnr_db,
                metric.max_abs
            );
        }
    }
}

// ── FitMode (Resize) ────────────────────────────────────────────────────────────

const ALL_FIT_MODES: [FitMode; 3] = [FitMode::Fit, FitMode::Fill, FitMode::Stretch];

fn fit_label(f: FitMode) -> &'static str {
    // Wildcard-free: a new FitMode variant fails to compile here.
    match f {
        FitMode::Fit => "fit",
        FitMode::Fill => "fill",
        FitMode::Stretch => "stretch",
    }
}

#[test]
fn fit_mode_cpu_gpu_parity() {
    let Some(gpu) = gpu_or_skip("FitMode parity") else {
        return;
    };
    // A non-square, non-uniform source is intentional: a solid would make Fit,
    // Fill, and Stretch indistinguishable and would let a GPU blit fallback pass.
    let image = patterned_image(11, 7);
    for (index, fit) in ALL_FIT_MODES.into_iter().enumerate() {
        let graph = FrameGraph {
            nodes: vec![
                IrNode {
                    op: IrOp::DecodeStill {
                        asset: AssetId::new(),
                    },
                    inputs: vec![],
                    content_hash: ContentHash(30 + index as u128),
                },
                IrNode {
                    op: IrOp::Resize {
                        w: CANVAS.0,
                        h: CANVAS.1,
                        fit,
                    },
                    inputs: vec![(IrNodeId(0), OutPort::default())],
                    content_hash: ContentHash(40 + index as u128),
                },
            ],
            output: Some(IrNodeId(1)),
        };
        let cpu = eval_cpu::evaluate(
            &graph,
            CANVAS,
            &mut PatternCpuSource {
                image: image.clone(),
            },
        );
        let mut source = PatternGpuSource {
            frame: GpuFrame::new(upload_pattern(&gpu, &image), image.width, image.height),
        };
        let gpu_image = eval_gpu(&gpu, &graph, &mut source);
        let metric = measure(&cpu, &gpu_image).expect("measurable");
        assert!(
            metric.psnr_db >= 35.0,
            "fit/{}: PSNR {:.2} dB < 35 (max_abs {:.5})",
            fit_label(fit),
            metric.psnr_db,
            metric.max_abs
        );
    }
}

// ── GradeOpKind + LutInterp ─────────────────────────────────────────────────────

/// A single-clip solid project decorated with `grade`, compiled to a frame graph
/// carrying a real `IrOp::Grade` (resolved via the same path production uses).
fn graded_solid(grade: Grade) -> CompiledFrame {
    graded_solid_at(grade, CANVAS)
}

/// [`graded_solid`] at an explicit canvas, so a grade can be swept at a size
/// whose pool bucket differs from the picture (see [`AWKWARD_CANVAS`]).
fn graded_solid_at(grade: Grade, canvas: (u32, u32)) -> CompiledFrame {
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("seq", FrameRate::FPS_30, canvas.0, canvas.1);
    let seq_id = seq.id;
    let mut track = Track::new(TrackKind::Video, "V1");
    let mut clip = Clip::new(
        ClipSource::SolidColor {
            color: Color {
                r: 0.45,
                g: 0.35,
                b: 0.55,
                a: 1.0,
            },
        },
        Tick(0),
        Tick::from_seconds(2),
    );
    clip.grade = Some(grade);
    track.clips.push(clip);
    seq.video_tracks.push(track);
    project.insert_sequence(seq);
    compile(&project, seq_id, 0, Tick(0), Quality::FULL, None)
}

fn single_op_grade(op: GradeOp) -> Grade {
    let mut grade = Grade::new();
    grade.ops.push(op);
    grade
}

/// Sample non-identity params for a kind (so the grade actually transforms pixels).
/// `Lut3d` resolves inert without a LUT provider (§ K-0.5), which is exactly the
/// P3 contract — CPU and GPU agree on the resulting identity either way. The
/// exhaustive `match` (minus the `#[non_exhaustive]`-mandated `Unknown`/`_` arm)
/// forces a new `GradeOpKind` to be given a row here.
fn grade_params_for(kind: GradeOpKind) -> Option<GradeOpParams> {
    Some(match kind {
        GradeOpKind::Exposure => GradeOpParams::Exposure { stops: 0.5 },
        GradeOpKind::Contrast => GradeOpParams::Contrast {
            pivot: 0.5,
            amount: 0.2,
        },
        GradeOpKind::WhiteBalance => GradeOpParams::WhiteBalance {
            temp: 0.1,
            tint: 0.05,
        },
        GradeOpKind::Cdl => GradeOpParams::Cdl {
            slope: [1.1, 1.0, 0.9],
            offset: [0.02, 0.0, -0.01],
            power: [1.0, 1.05, 0.95],
            sat: 1.1,
        },
        GradeOpKind::Wheels => GradeOpParams::Wheels {
            lift: [0.02, 0.0, -0.01],
            gamma: [1.0, 1.05, 0.95],
            gain: [1.05, 1.0, 0.95],
            sat: 1.0,
        },
        GradeOpKind::Curves => GradeOpParams::Curves {
            master: vec![(0.0, 0.05), (1.0, 0.95)],
            red: vec![(0.0, 0.0), (1.0, 1.0)],
            green: vec![(0.0, 0.0), (1.0, 1.0)],
            blue: vec![(0.0, 0.0), (1.0, 1.0)],
            hue_vs_hue: vec![],
            hue_vs_sat: vec![],
        },
        GradeOpKind::HslQualifier => GradeOpParams::HslQualifier {
            hue: [0.0, 1.0],
            sat: [0.0, 1.0],
            lum: [0.0, 1.0],
            softness: 0.1,
            correction: CdlParams {
                slope: [1.05, 1.0, 0.95],
                ..CdlParams::identity()
            },
        },
        GradeOpKind::Lut3d => GradeOpParams::Lut3d {
            asset: AssetId::new(),
            intensity: 1.0,
            interp: LutInterp::Trilinear,
        },
        // `#[non_exhaustive]`: a forward-compat / future kind has no fixture.
        _ => return None,
    })
}

const KNOWN_GRADE_KINDS: [GradeOpKind; 8] = [
    GradeOpKind::Exposure,
    GradeOpKind::Contrast,
    GradeOpKind::WhiteBalance,
    GradeOpKind::Cdl,
    GradeOpKind::Wheels,
    GradeOpKind::Curves,
    GradeOpKind::HslQualifier,
    GradeOpKind::Lut3d,
];

#[test]
fn grade_op_kind_cpu_gpu_parity() {
    let Some(gpu) = gpu_or_skip("GradeOpKind parity") else {
        return;
    };
    for kind in KNOWN_GRADE_KINDS {
        let params = grade_params_for(kind).expect("known kind has a fixture");
        let compiled = graded_solid(single_op_grade(GradeOp::new(kind, params)));
        assert_parity_solid(&gpu, &format!("grade/{kind:?}"), &compiled.graph, 1e-3);
    }
}

/// A rotated, off-centre elliptical power window.
///
/// Rotation is deliberate: it is the case that rules out "just pre-scale the
/// centre and size on the CPU" as a fix for the physical-vs-logical mismatch.
/// With a per-axis scale, pre-scaling would SHEAR a rotated ellipse, so the
/// correction has to happen in the shader's coordinate space. Off-centre so a
/// mis-scaled coordinate translates the window rather than merely resizing it
/// symmetrically about the middle, which a centred window can hide.
fn rotated_power_window() -> GradeMask {
    GradeMask::PowerWindow {
        shape: WindowShape::Ellipse,
        center: [0.42, 0.55],
        size: [0.28, 0.36],
        rotation: 0.6,
        softness: 0.15,
        invert: false,
    }
}

/// Every grade kind, MASKED, at a canvas whose pool bucket differs from the
/// picture on both axes by different ratios.
///
/// This is the case `grade_op_kind_cpu_gpu_parity` structurally cannot see: its
/// rows are all `mask: None`, and `CANVAS` (8x8) buckets to a square 64x64. A
/// power window is the only thing in the grade path that consumes normalized
/// spatial coordinates, so it is the only thing that can disagree between an
/// evaluator that normalizes over the picture (CPU) and one that normalizes over
/// the render target (GPU) — which is precisely the bug fixed in `2670c2d`.
#[test]
fn masked_grade_cpu_gpu_parity_at_an_awkward_canvas() {
    let Some(gpu) = gpu_or_skip("masked grade parity at an awkward canvas") else {
        return;
    };

    // Guard against a vacuous sweep: if the mask were inert at this canvas, CPU
    // and GPU would agree trivially and the test would prove nothing. Establish
    // first that masking actually changes the CPU result here.
    let probe_kind = GradeOpKind::Exposure;
    let params = grade_params_for(probe_kind).expect("known kind has a fixture");
    let unmasked = graded_solid_at(
        single_op_grade(GradeOp::new(probe_kind, params.clone())),
        AWKWARD_CANVAS,
    );
    let mut masked_op = GradeOp::new(probe_kind, params);
    masked_op.mask = Some(rotated_power_window());
    let masked = graded_solid_at(single_op_grade(masked_op), AWKWARD_CANVAS);

    let a = eval_cpu::evaluate(&unmasked.graph, AWKWARD_CANVAS, &mut EmptyProvider);
    let b = eval_cpu::evaluate(&masked.graph, AWKWARD_CANVAS, &mut EmptyProvider);
    let spread = a
        .pixels
        .iter()
        .zip(&b.pixels)
        .flat_map(|(x, y)| (0..4).map(move |k| (x[k] - y[k]).abs()))
        .fold(0.0f32, f32::max);
    assert!(
        spread > 1e-2,
        "the power window must actually gate pixels at {AWKWARD_CANVAS:?}, else \
         this sweep is vacuous (masked vs unmasked differ by only {spread})"
    );

    for kind in KNOWN_GRADE_KINDS {
        let params = grade_params_for(kind).expect("known kind has a fixture");
        let mut op = GradeOp::new(kind, params);
        op.mask = Some(rotated_power_window());
        let compiled = graded_solid_at(single_op_grade(op), AWKWARD_CANVAS);
        assert_parity_solid_at(
            &gpu,
            &format!("masked-grade/{kind:?}@{AWKWARD_CANVAS:?}"),
            &compiled.graph,
            1e-3,
            AWKWARD_CANVAS,
        );
    }
}

/// Opaque white square inset in a transparent canvas — silhouette for outline.
fn square_matte_image(w: u32, h: u32, inset: u32) -> Image {
    let mut img = Image::new(w, h);
    for y in inset..h.saturating_sub(inset) {
        for x in inset..w.saturating_sub(inset) {
            img.pixels[(y * w + x) as usize] = [1.0, 1.0, 1.0, 1.0];
        }
    }
    img
}

/// CPU↔GPU parity for `util.outline` (30 §5 / proposal 208): the GPU path must
/// be a real SDF-band twin of the CPU oracle, not a no-op blit or the old
/// linear-ramp box search. A solid exterior band + Hermite AA at thickness.
#[test]
fn util_outline_sdf_cpu_gpu_parity() {
    let Some(gpu) = gpu_or_skip("util.outline SDF parity") else {
        return;
    };
    // Large enough that thickness-4 has room; not a 64-multiple so logical dims
    // exercise the same path production uses.
    const W: u32 = 48;
    const H: u32 = 40;
    const THICK: f32 = 4.0;
    let image = square_matte_image(W, H, 12);

    let mut params = ResolvedParams::default();
    params.entries.push((
        PropPath::new("params.thickness"),
        PropValue::Float(THICK as f64),
    ));
    params
        .entries
        .push((PropPath::new("params.r"), PropValue::Float(1.0)));
    params
        .entries
        .push((PropPath::new("params.g"), PropValue::Float(0.0)));
    params
        .entries
        .push((PropPath::new("params.b"), PropValue::Float(0.0)));
    params
        .entries
        .push((PropPath::new("params.opacity"), PropValue::Float(1.0)));

    let graph = FrameGraph {
        nodes: vec![
            IrNode {
                op: IrOp::DecodeStill {
                    asset: AssetId::new(),
                },
                inputs: vec![],
                content_hash: ContentHash(2081),
            },
            IrNode {
                op: IrOp::Effect {
                    kind: EffectKind::Unknown(UnknownTag::intern("util.outline")),
                    params,
                },
                inputs: vec![(IrNodeId(0), OutPort::default())],
                content_hash: ContentHash(2082),
            },
        ],
        output: Some(IrNodeId(1)),
    };

    let cpu = eval_cpu::evaluate(
        &graph,
        (W, H),
        &mut PatternCpuSource {
            image: image.clone(),
        },
    );
    // Guard: the outline must actually paint exterior pixels (not a no-op blit).
    let mut exterior_alpha = 0.0f32;
    for y in 0..H {
        for x in 0..W {
            if image.pixel(x, y)[3] < 0.5 {
                exterior_alpha = exterior_alpha.max(cpu.pixel(x, y)[3]);
            }
        }
    }
    assert!(
        exterior_alpha > 0.5,
        "outline must paint exterior coverage (got max exterior alpha {exterior_alpha}); \
         otherwise the parity sweep is vacuous"
    );

    let mut src = PatternGpuSource {
        frame: GpuFrame::new(upload_pattern(&gpu, &image), W, H),
    };
    let g = eval_gpu_at(&gpu, &graph, &mut src, (W, H));
    // Algebraic SDF band: tight channel tolerance (same as grade/merge rows).
    assert_pixels_within("util.outline/sdf", &cpu, &g, 2e-2);
}

const ALL_LUT_INTERP: [LutInterp; 2] = [LutInterp::Trilinear, LutInterp::Tetrahedral];

fn lut_interp_label(i: LutInterp) -> &'static str {
    // Wildcard-free: a new LutInterp variant fails to compile here.
    match i {
        LutInterp::Trilinear => "trilinear",
        LutInterp::Tetrahedral => "tetrahedral",
    }
}

#[test]
fn lut_interp_cpu_gpu_parity() {
    let Some(gpu) = gpu_or_skip("LutInterp parity") else {
        return;
    };
    // Without a LUT provider both interp modes resolve inert (identity) — the P3
    // contract (K-0.5) — so CPU and GPU agree. The exhaustive iteration keeps every
    // interp variant covered for when a provider is threaded through compile.
    for interp in ALL_LUT_INTERP {
        let grade = single_op_grade(GradeOp::new(
            GradeOpKind::Lut3d,
            GradeOpParams::Lut3d {
                asset: AssetId::new(),
                intensity: 1.0,
                interp,
            },
        ));
        let compiled = graded_solid(grade);
        assert_parity_solid(
            &gpu,
            &format!("lut_interp/{}", lut_interp_label(interp)),
            &compiled.graph,
            1e-3,
        );
    }
}

// ── TransitionKind (compile-time → Merge / dip lowering) ────────────────────────

const KNOWN_TRANSITIONS: [TransitionKind; 5] = [
    TransitionKind::CrossDissolve,
    TransitionKind::DipToBlack,
    TransitionKind::DipToColor,
    TransitionKind::Wipe,
    TransitionKind::Push,
];

fn transition_label(k: TransitionKind) -> &'static str {
    match k {
        TransitionKind::CrossDissolve => "cross_dissolve",
        TransitionKind::DipToBlack => "dip_to_black",
        TransitionKind::DipToColor => "dip_to_color",
        TransitionKind::Wipe => "wipe",
        TransitionKind::Push => "push",
        // `#[non_exhaustive]`: an unknown transition lowers to a hard cut.
        _ => "unknown",
    }
}

/// Two overlapping solid clips with a `transition_in` of `kind` on the second,
/// compiled mid-overlap so the transition is active. The lowering is pure
/// `Merge`/`SolidColor` (CrossDissolve/Wipe/Push → cross-dissolve, Dip* → dip
/// through colour), which both evaluators run identically.
fn transition_graph(kind: TransitionKind) -> CompiledFrame {
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("seq", FrameRate::FPS_30, CANVAS.0, CANVAS.1);
    let seq_id = seq.id;
    let mut track = Track::new(TrackKind::Video, "V1");

    let clip_a = Clip::new(
        ClipSource::SolidColor {
            color: Color {
                r: 0.2,
                g: 0.6,
                b: 0.4,
                a: 1.0,
            },
        },
        Tick(0),
        Tick(200),
    );
    let mut clip_b = Clip::new(
        ClipSource::SolidColor {
            color: Color {
                r: 0.7,
                g: 0.3,
                b: 0.5,
                a: 1.0,
            },
        },
        Tick(100),
        Tick(200),
    );
    let mut tr = Transition::new(kind, Tick(100));
    tr.params = TransitionParams {
        color: Some(Color {
            r: 0.1,
            g: 0.1,
            b: 0.1,
            a: 1.0,
        }),
        ..TransitionParams::default()
    };
    clip_b.transition_in = Some(tr);
    track.clips.push(clip_a);
    track.clips.push(clip_b);
    seq.video_tracks.push(track);
    project.insert_sequence(seq);
    // tick 150 ∈ [100, 200): the transition is active.
    compile(&project, seq_id, 0, Tick(150), Quality::FULL, None)
}

#[test]
fn transition_kind_cpu_gpu_parity() {
    let Some(gpu) = gpu_or_skip("TransitionKind parity") else {
        return;
    };
    for kind in KNOWN_TRANSITIONS {
        let compiled = transition_graph(kind);
        assert_parity_solid(
            &gpu,
            &format!("transition/{}", transition_label(kind)),
            &compiled.graph,
            1e-3,
        );
    }
}

// Portrait reframing must use the same center in Draft and Full previews.
#[test]
fn centered_reframe_matches_full_when_preview_canvas_is_smaller() {
    let full = (108, 192);
    let draft = (54, 96);
    let mut image = Image::new(full.0, full.1);
    for y in 0..full.1 {
        for x in 0..full.0 {
            image.pixels[(y * full.0 + x) as usize] = [
                (x as f32 + 0.5) / full.0 as f32,
                (y as f32 + 0.5) / full.1 as f32,
                0.0,
                1.0,
            ];
        }
    }
    let center = glam::Vec2::new(54.0, 96.0);
    let mat = glam::Mat3::from_translation(center + glam::Vec2::new(5.0, -8.0))
        * glam::Mat3::from_scale(glam::Vec2::new(3.16, 1.2))
        * glam::Mat3::from_translation(-center);
    let graph = FrameGraph {
        nodes: vec![
            IrNode {
                op: IrOp::DecodeStill {
                    asset: AssetId::new(),
                },
                inputs: vec![],
                content_hash: ContentHash(810),
            },
            IrNode {
                op: IrOp::Transform2D {
                    mat,
                    sampling: Sampling::Bilinear,
                },
                inputs: vec![(IrNodeId(0), OutPort::default())],
                content_hash: ContentHash(811),
            },
            IrNode {
                op: IrOp::Output {
                    w: full.0,
                    h: full.1,
                },
                inputs: vec![(IrNodeId(1), OutPort::default())],
                content_hash: ContentHash(812),
            },
        ],
        output: Some(IrNodeId(2)),
    };
    let expected = eval_cpu::evaluate(
        &graph,
        full,
        &mut PatternCpuSource {
            image: image.clone(),
        },
    );
    let actual = eval_cpu::evaluate(
        &graph,
        draft,
        &mut PatternCpuSource {
            image: image.clone(),
        },
    );
    assert_pixels_within("CPU draft reframe", &expected, &actual, 0.015);
    if let Some(gpu) = gpu_or_skip("Draft reframe") {
        let mut source = PatternGpuSource {
            frame: GpuFrame::new(upload_pattern(&gpu, &image), full.0, full.1),
        };
        let mut eval = Evaluator::new(gpu.clone());
        let tex = eval
            .evaluate(&graph, draft, &mut source)
            .expect("draft output");
        let actual = Image {
            width: full.0,
            height: full.1,
            pixels: read_texture_rgba16f(&gpu, &tex, full.0, full.1),
        };
        assert_pixels_within("GPU draft reframe", &expected, &actual, 0.015);
    }
}

#[test]
fn title_bounds_match_between_draft_and_full_on_padded_canvas() {
    let Some(gpu) = gpu_or_skip("title preview size") else {
        return;
    };
    let full = (216, 384);
    let cue = photonic_render::caption::CaptionCueRun {
        words: vec![photonic_render::caption::CaptionWordRun {
            text: "Hello".into(),
            font_family: "Inter".into(),
            font_weight: 700,
            color: [255; 4],
        }],
        font_size: 32.0,
        line_height_mul: 1.2,
        anchor: [0.5, 0.2],
        max_width: 0.8,
    };
    let graph = FrameGraph {
        nodes: vec![
            IrNode {
                op: IrOp::TextGen {
                    block: photonic_video::contract::ResolvedTextBlock { cue: Some(cue) },
                },
                inputs: vec![],
                content_hash: ContentHash(900),
            },
            IrNode {
                op: IrOp::Output {
                    w: full.0,
                    h: full.1,
                },
                inputs: vec![(IrNodeId(0), OutPort::default())],
                content_hash: ContentHash(901),
            },
        ],
        output: Some(IrNodeId(1)),
    };
    let bounds = |canvas| {
        let mut eval = Evaluator::new(gpu.clone());
        let tex = eval.evaluate(&graph, canvas, &mut NullFrameSource).unwrap();
        let pixels = read_texture_rgba16f(&gpu, &tex, full.0, full.1);
        let mut b = [full.0 as i32, full.1 as i32, 0, 0];
        for (i, p) in pixels.iter().enumerate().filter(|(_, p)| p[3] > 0.2) {
            let (x, y) = ((i as u32 % full.0) as i32, (i as u32 / full.0) as i32);
            let _ = p;
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
        }
        b
    };
    let a = bounds(full);
    let b = bounds((108, 192));
    assert!(
        a.iter().zip(b).all(|(x, y)| (*x - y).abs() <= 3),
        "Full {a:?}, Draft {b:?}"
    );
    assert!(
        (a[0] + a[2] - 216).abs() <= 5,
        "title must center on logical canvas: {a:?}"
    );
}

#[test]
fn portrait_crop_preserves_native_video_detail() {
    let canvas = (8, 16);
    let mut image = Image::new(32, 16);
    for y in 0..16 {
        for x in 0..32 {
            let v = (x % 2) as f32;
            image.pixels[(y * 32 + x) as usize] = [v, v, v, 1.0];
        }
    }
    let graph = FrameGraph {
        nodes: vec![
            IrNode {
                op: IrOp::DecodeVideo {
                    asset: AssetId::new(),
                    src_time: Tick(0),
                    proxy: false,
                },
                inputs: vec![],
                content_hash: ContentHash(9001),
            },
            IrNode {
                op: IrOp::Transform2D {
                    mat: glam::Mat3::from_translation(glam::Vec2::new(-12.0, 0.0))
                        * glam::Mat3::from_scale(glam::Vec2::new(4.0, 1.0)),
                    sampling: Sampling::Bilinear,
                },
                inputs: vec![(IrNodeId(0), OutPort::default())],
                content_hash: ContentHash(9002),
            },
        ],
        output: Some(IrNodeId(1)),
    };
    let mut expected = Image::new(8, 16);
    for y in 0..16 {
        for x in 0..8 {
            expected.pixels[(y * 8 + x) as usize] = image.pixel(x + 12, y);
        }
    }
    let cpu = eval_cpu::evaluate(
        &graph,
        canvas,
        &mut PatternCpuSource {
            image: image.clone(),
        },
    );
    assert_pixels_within("native crop CPU", &expected, &cpu, 0.001);
    if let Some(gpu) = gpu_or_skip("native crop") {
        let mut source = PatternGpuSource {
            frame: GpuFrame::new(upload_pattern(&gpu, &image), 32, 16),
        };
        let actual = eval_gpu_at(&gpu, &graph, &mut source, canvas);
        assert_pixels_within("native crop GPU", &expected, &actual, 0.001);
    }
}
