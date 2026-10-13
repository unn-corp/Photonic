//! Exact rank-filter geometry and resource accounting, shared by CPU/GPU paths.
use super::ir::{FrameGraph, IrNodeId, IrOp};
pub(crate) const MAX_RANK_STEPS: u64 = 2_000_000_000;
pub(crate) fn effective_radius(radius: u32, w: u32, h: u32, scale: [f32; 2]) -> u32 {
    radius.min(((w as f32 / scale[0]).max(h as f32 / scale[1]).ceil() as u32).max(1))
}
pub(crate) fn layout(radius: u32) -> (usize, u32, u64, u64) {
    let side = 2 * radius as u64 + 1;
    let count = side.saturating_mul(side);
    if radius <= 7 {
        let n = (count as u32).next_power_of_two().max(16);
        let level = n.ilog2() as u64;
        (
            (n.ilog2() - 4) as usize,
            n,
            1,
            (n as u64 / 2) * level * (level + 1) / 2,
        )
    } else {
        (5, 0, 33, count.saturating_mul(33))
    }
}
pub(crate) fn validate_capacity(
    graph: &FrameGraph,
    canvas: (u32, u32),
    tap: Option<IrNodeId>,
) -> Result<(), &'static str> {
    let scale = graph.canvas_scale(canvas);
    let mut steps = 0u64;
    for (node, needed) in graph.nodes.iter().zip(graph.demanded_nodes(tap)) {
        if !needed {
            continue;
        }
        if let IrOp::NativeRankFilter {
            radius,
            pixel_scale,
            ..
        } = node.op
        {
            if radius == 0 {
                continue;
            }
            let r = effective_radius(
                radius,
                canvas.0,
                canvas.1,
                [pixel_scale[0] * scale.x, pixel_scale[1] * scale.y],
            );
            steps = steps.saturating_add(
                (canvas.0 as u64)
                    .saturating_mul(canvas.1 as u64)
                    .saturating_mul(layout(r).3),
            );
        }
    }
    if steps > MAX_RANK_STEPS {
        Err("native Median/Dust & Scratches exceeds the 2B comparison/rank-step budget per frame; reduce radius or render dimensions")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::ir::{
        ContentHash, IrNode, LinearColor, NativeRankKind, OutPort, WorkingColorDomain,
    };
    fn graph(radius: u32) -> FrameGraph {
        FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![
                IrNode {
                    op: IrOp::SolidColor {
                        color: LinearColor {
                            r: 0.2,
                            g: 0.3,
                            b: 0.4,
                            a: 1.0,
                        },
                    },
                    inputs: vec![],
                    content_hash: ContentHash(1),
                },
                IrNode {
                    op: IrOp::NativeRankFilter {
                        kind: NativeRankKind::Median,
                        radius,
                        threshold: 0,
                        pixel_scale: [1.0; 2],
                    },
                    inputs: vec![(IrNodeId(0), OutPort(0))],
                    content_hash: ContentHash(2),
                },
            ],
            output: Some(IrNodeId(1)),
        }
    }
    #[test]
    fn native_rank_layout_extent_and_resource_boundaries() {
        assert_eq!(layout(1), (0, 16, 1, 80));
        assert_eq!(layout(2), (1, 32, 1, 240));
        assert_eq!(layout(3), (2, 64, 1, 672));
        assert_eq!(layout(4), (3, 128, 1, 1792));
        assert_eq!(layout(5), (3, 128, 1, 1792));
        assert_eq!(layout(6), (4, 256, 1, 4608));
        assert_eq!(layout(7), (4, 256, 1, 4608));
        assert_eq!(layout(8), (5, 0, 33, 9537));
        assert_eq!(effective_radius(500, 3, 2, [1.0; 2]), 3);
        assert_eq!(effective_radius(500, 3, 2, [0.5, 0.25]), 8);
        assert!(validate_capacity(&graph(2), (3840, 2160), None).is_ok());
        assert!(validate_capacity(&graph(3), (3840, 2160), None).is_err());
        assert!(validate_capacity(&graph(0), (u32::MAX, u32::MAX), None).is_ok());
        assert!(validate_capacity(&graph(u32::MAX), (u32::MAX, u32::MAX), None).is_err());
    }
    #[test]
    fn native_rank_demanded_only_and_shared_fetch_accounting() {
        let mut g = graph(16);
        g.output = Some(IrNodeId(0));
        assert!(validate_capacity(&g, (320, 240), None).is_ok());
        assert!(validate_capacity(&g, (320, 240), Some(IrNodeId(1))).is_err());
        let mut g = graph(2);
        assert!(super::super::native_surface::validate_capacity(&g, (3840, 2160), None).is_ok());
        g.nodes.push(IrNode {
            op: IrOp::NativeLensBlur {
                radius: 1.0,
                pixel_scale: [1.0; 2],
            },
            inputs: vec![(IrNodeId(1), OutPort(0))],
            content_hash: ContentHash(3),
        });
        g.output = Some(IrNodeId(2));
        assert!(validate_capacity(&g, (3840, 2160), None).is_ok());
        assert!(super::super::native_surface::validate_capacity(&g, (3840, 2160), None).is_err());
    }
}
