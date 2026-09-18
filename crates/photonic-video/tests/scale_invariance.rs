//! 32 §7 / E-6 — scale-invariance regression guard.
//!
//! Geometry-carrying ops must produce Draft and Full results that agree when
//! Full is downsampled to Draft size. This lands **before** the effect
//! catalogue grows so every new kernel inherits the guard rather than the bug.
//!
//! Self-skips (eprintln) when no GPU adapter is available.

use photonic_core::timeline::{EffectKind, PropPath, PropValue};
use photonic_video::contract::ResolvedParams;
use photonic_video::graph::compile::{fit_long_edge, DRAFT_MAX_LONG_EDGE};
use photonic_video::graph::eval::{read_texture_rgba16f, Evaluator, GpuContext, NullFrameSource};
use photonic_video::graph::eval_cpu::{self, EmptyProvider};
use photonic_video::graph::ir::{
    ContentHash, FrameGraph, IrNode, IrNodeId, IrOp, LinearColor, OutPort,
};

fn gpu_or_skip(what: &str) -> Option<GpuContext> {
    match GpuContext::request_blocking() {
        Some(g) => Some(g),
        None => {
            eprintln!("no GPU adapter — skipping {what}");
            None
        }
    }
}

/// Area-average downsample of a linear-premult RGBA buffer.
fn box_downscale(src: &[[f32; 4]], w: u32, h: u32, ow: u32, oh: u32) -> Vec<[f32; 4]> {
    let mut out = Vec::with_capacity((ow * oh) as usize);
    for oy in 0..oh {
        let y0 = (oy as u64 * h as u64 / oh as u64) as u32;
        let y1 = (((oy as u64 + 1) * h as u64).div_ceil(oh as u64) as u32).clamp(y0 + 1, h);
        for ox in 0..ow {
            let x0 = (ox as u64 * w as u64 / ow as u64) as u32;
            let x1 = (((ox as u64 + 1) * w as u64).div_ceil(ow as u64) as u32).clamp(x0 + 1, w);
            let mut acc = [0f64; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = src[(y * w + x) as usize];
                    for (a, c) in acc.iter_mut().zip(p.iter()) {
                        *a += *c as f64;
                    }
                }
            }
            let n = ((y1 - y0) as f64) * ((x1 - x0) as f64);
            out.push([
                (acc[0] / n) as f32,
                (acc[1] / n) as f32,
                (acc[2] / n) as f32,
                (acc[3] / n) as f32,
            ]);
        }
    }
    out
}

fn max_abs_diff(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    a.iter()
        .zip(b.iter())
        .flat_map(|(pa, pb)| pa.iter().zip(pb.iter()).map(|(x, y)| (x - y).abs()))
        .fold(0.0f32, f32::max)
}

/// Geometry-heavy graph: solid + blur with scale-proportional radius.
fn geometry_graph(w: u32, h: u32) -> FrameGraph {
    // Radius scales with canvas: 1% of long edge → scale-free intent (32 §7).
    let r = (w.max(h) as f64) * 0.01;
    let radius = r.max(0.5);
    FrameGraph {
        nodes: vec![
            IrNode {
                op: IrOp::SolidColor {
                    color: LinearColor {
                        r: 0.8,
                        g: 0.2,
                        b: 0.1,
                        a: 1.0,
                    },
                },
                inputs: vec![],
                content_hash: ContentHash(1),
            },
            IrNode {
                op: IrOp::Effect {
                    kind: EffectKind::Blur,
                    params: ResolvedParams {
                        entries: vec![(PropPath::new("params.radius"), PropValue::Float(radius))],
                    },
                },
                inputs: vec![(IrNodeId(0), OutPort::default())],
                content_hash: ContentHash(2),
            },
            IrNode {
                op: IrOp::Output { w, h },
                inputs: vec![(IrNodeId(1), OutPort::default())],
                content_hash: ContentHash(3),
            },
        ],
        output: Some(IrNodeId(2)),
    }
}

#[test]
fn draft_vs_downsampled_full_within_tolerance_cpu() {
    let full = (64u32, 64u32);
    let draft = fit_long_edge(full.0, full.1, 32);
    assert!(draft.0 <= 32 && draft.1 <= 32);

    let g_full = geometry_graph(full.0, full.1);
    let g_draft = geometry_graph(draft.0, draft.1);

    let img_full = eval_cpu::evaluate(&g_full, full, &mut EmptyProvider);
    let img_draft = eval_cpu::evaluate(&g_draft, draft, &mut EmptyProvider);

    let down = box_downscale(&img_full.pixels, full.0, full.1, draft.0, draft.1);
    let diff = max_abs_diff(&down, &img_draft.pixels);
    assert!(
        diff < 0.15,
        "Draft vs downsampled Full max-abs diff {diff} exceeds tolerance"
    );
}

#[test]
fn draft_vs_downsampled_full_within_tolerance_gpu() {
    let Some(gpu) = gpu_or_skip("scale_invariance gpu") else {
        return;
    };
    let full = (64u32, 64u32);
    let draft = fit_long_edge(full.0, full.1, 32);

    let g_full = geometry_graph(full.0, full.1);
    let g_draft = geometry_graph(draft.0, draft.1);

    let mut eval = Evaluator::new(gpu.clone());
    let mut src = NullFrameSource;
    let tex_full = eval
        .evaluate(&g_full, full, &mut src)
        .expect("full evaluate");
    let px_full = read_texture_rgba16f(&gpu, &tex_full, full.0, full.1);

    let mut eval2 = Evaluator::new(gpu.clone());
    let tex_draft = eval2
        .evaluate(&g_draft, draft, &mut src)
        .expect("draft evaluate");
    let px_draft = read_texture_rgba16f(&gpu, &tex_draft, draft.0, draft.1);

    let down = box_downscale(&px_full, full.0, full.1, draft.0, draft.1);
    let diff = max_abs_diff(&down, &px_draft);
    assert!(
        diff < 0.2,
        "GPU Draft vs downsampled Full max-abs diff {diff} exceeds tolerance"
    );
}

#[test]
fn draft_max_long_edge_is_documented() {
    assert_eq!(DRAFT_MAX_LONG_EDGE, 960);
    assert_eq!(fit_long_edge(1920, 1080, DRAFT_MAX_LONG_EDGE), (960, 540));
}
