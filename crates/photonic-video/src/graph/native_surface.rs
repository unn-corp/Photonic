//! Shared native bilateral/disc work bound. Refuse before allocating/dispatching;
//! never silently reduce the requested radius or use an approximate kernel.
use super::ir::{FrameGraph, IrNodeId, IrOp};
pub(crate) const MAX_TEXTURE_FETCHES: u64 = 256_000_000;
pub(crate) fn validate_capacity(
    graph: &FrameGraph,
    canvas: (u32, u32),
    tap: Option<IrNodeId>,
) -> Result<(), &'static str> {
    let demanded = graph.demanded_nodes(tap);
    let canvas_scale = graph.canvas_scale(canvas);
    let mut work = 0u64;
    for (node, needed) in graph.nodes.iter().zip(demanded) {
        if !needed {
            continue;
        }
        let (radius, pixel_scale, scans) = match node.op {
            IrOp::NativeSurfaceBlur {
                radius,
                threshold,
                pixel_scale,
            } if radius > 0 && threshold > 0.0 => (radius as u64, pixel_scale, 1),
            IrOp::NativeLensBlur {
                radius,
                pixel_scale,
            } if radius >= 1.0 => (radius.ceil() as u64, pixel_scale, 1),
            IrOp::NativeRankFilter {
                radius,
                pixel_scale,
                ..
            } if radius > 0 => {
                let r = super::native_rank::effective_radius(
                    radius,
                    canvas.0,
                    canvas.1,
                    [
                        pixel_scale[0] * canvas_scale.x,
                        pixel_scale[1] * canvas_scale.y,
                    ],
                );
                (r as u64, pixel_scale, super::native_rank::layout(r).2)
            }
            _ => continue,
        };
        {
            let scale = [
                pixel_scale[0] * canvas_scale.x,
                pixel_scale[1] * canvas_scale.y,
            ];
            let fetches = scale
                .iter()
                .map(|v| if v.fract() == 0.0 { 1u64 } else { 2u64 })
                .product::<u64>();
            let side = radius.saturating_mul(2).saturating_add(1);
            let mut node_work = (canvas.0 as u64)
                .saturating_mul(canvas.1 as u64)
                .saturating_mul(side)
                .saturating_mul(side)
                .saturating_mul(fetches)
                .saturating_mul(scans);
            if matches!(node.op, IrOp::NativeRankFilter { .. }) {
                node_work = node_work.saturating_add(
                    (canvas.0 as u64)
                        .saturating_mul(canvas.1 as u64)
                        .saturating_mul(fetches),
                );
            }
            work = work.saturating_add(node_work);
        }
    }
    if work > MAX_TEXTURE_FETCHES {
        return Err("native Surface Blur/Lens Blur/Median/Dust & Scratches exceeds the 256M estimated texture-fetch budget per frame; reduce radius or render dimensions");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::ir::{ContentHash, IrNode, LinearColor, OutPort, WorkingColorDomain};
    fn graph(radius: u32, threshold: f32) -> FrameGraph {
        FrameGraph {
            working_color_domain: WorkingColorDomain::SceneLinearAcescg,
            nodes: vec![
                IrNode {
                    op: IrOp::SolidColor {
                        color: LinearColor {
                            r: 0.2,
                            g: 0.4,
                            b: 0.6,
                            a: 1.0,
                        },
                    },
                    inputs: vec![],
                    content_hash: ContentHash(1),
                },
                IrNode {
                    op: IrOp::NativeSurfaceBlur {
                        radius,
                        threshold,
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
    fn native_lens_blur_capacity_combines_with_surface_and_preserves_identities() {
        let mut g = graph(0, 0.0);
        g.nodes[1].op = IrOp::NativeLensBlur {
            radius: 500.0,
            pixel_scale: [1.0; 2],
        };
        assert!(validate_capacity(&g, (15, 16), None).is_ok());
        assert!(validate_capacity(&g, (16, 16), None).is_err());
        g.nodes[1].op = IrOp::NativeLensBlur {
            radius: 0.99,
            pixel_scale: [1.0; 2],
        };
        assert!(validate_capacity(&g, (u32::MAX, u32::MAX), None).is_ok());
        let mut g = graph(64, 0.5);
        assert!(validate_capacity(&g, (128, 120), None).is_ok());
        g.nodes.push(IrNode {
            op: IrOp::NativeLensBlur {
                radius: 3.0,
                pixel_scale: [1.0; 2],
            },
            inputs: vec![(IrNodeId(1), OutPort(0))],
            content_hash: ContentHash(3),
        });
        g.output = Some(IrNodeId(2));
        assert!(validate_capacity(&g, (128, 120), None).is_err());
        g.nodes[2].op = IrOp::NativeLensBlur {
            radius: 0.99,
            pixel_scale: [1.0; 2],
        };
        assert!(validate_capacity(&g, (128, 120), None).is_ok());
    }
    #[test]
    fn native_surface_blur_capacity_boundaries_and_identity() {
        assert!(validate_capacity(&graph(64, 0.5), (128, 120), None).is_ok());
        assert!(validate_capacity(&graph(64, 0.5), (129, 120), None).is_err());
        assert!(validate_capacity(&graph(2, 0.25), (3840, 2160), None).is_ok());
        assert!(validate_capacity(&graph(3, 0.25), (3840, 2160), None).is_err());
        assert!(validate_capacity(&graph(5, 0.25), (1920, 1080), None).is_ok());
        assert!(validate_capacity(&graph(6, 0.25), (1920, 1080), None).is_err());
        assert!(validate_capacity(&graph(0, 0.25), (u32::MAX, u32::MAX), None).is_ok());
        assert!(validate_capacity(&graph(64, 0.0), (u32::MAX, u32::MAX), None).is_ok());
        assert!(validate_capacity(&graph(u32::MAX, 1.0), (u32::MAX, u32::MAX), None).is_err());
    }
    #[test]
    fn native_surface_blur_capacity_demanded_scale_and_sum() {
        let mut g = graph(64, 0.5);
        g.output = Some(IrNodeId(0));
        assert!(validate_capacity(&g, (1024, 1024), None).is_ok());
        assert!(validate_capacity(&g, (1024, 1024), Some(IrNodeId(1))).is_err());
        g.output = Some(IrNodeId(1));
        g.nodes.push(IrNode {
            op: IrOp::Output { w: 256, h: 240 },
            inputs: vec![(IrNodeId(1), OutPort(0))],
            content_hash: ContentHash(3),
        });
        g.output = Some(IrNodeId(2));
        assert!(validate_capacity(&g, (128, 120), None).is_err());
        assert!(validate_capacity(&g, (64, 60), None).is_ok());
        let mut g = graph(64, 0.5);
        g.nodes.push(IrNode {
            op: IrOp::NativeSurfaceBlur {
                radius: 64,
                threshold: 0.5,
                pixel_scale: [1.0; 2],
            },
            inputs: vec![(IrNodeId(1), OutPort(0))],
            content_hash: ContentHash(3),
        });
        g.output = Some(IrNodeId(2));
        assert!(validate_capacity(&g, (128, 120), None).is_err());
        g.nodes[2].op = IrOp::NativeSurfaceBlur {
            radius: 0,
            threshold: 0.5,
            pixel_scale: [1.0; 2],
        };
        assert!(validate_capacity(&g, (128, 120), None).is_ok());
    }
}
