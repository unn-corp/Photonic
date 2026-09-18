//! E-1 / 32 §1 — source-range contract for temporal access.
//!
//! Every IR op declares the upstream tick range it must read for a given output
//! tick. Default is the identity `[out, out]`. The compiler and prefetch can
//! union these ranges to derive decode windows without guessing.
//!
//! Photonic already expands `GraphOp::TimeOffset` by re-lowering at compile
//! time; this module makes the *declaration* explicit so future temporal nodes
//! (deinterlace, motion blur, optical flow) share one mechanism.

use photonic_core::timeline::Tick;

use super::ir::{FrameGraph, IrOp};

/// Inclusive upstream tick range a node must read to produce `out`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FrameRange {
    pub first: Tick,
    pub last: Tick,
    /// Optional sub-sample stride (motion blur, etc.). `None` = every tick.
    pub stride: Option<Tick>,
}

impl FrameRange {
    /// Identity range: only the output tick itself.
    pub fn identity(out: Tick) -> Self {
        Self {
            first: out,
            last: out,
            stride: None,
        }
    }

    /// Inclusive span from `a` to `b` (order-independent).
    pub fn span(a: Tick, b: Tick) -> Self {
        if a.0 <= b.0 {
            Self {
                first: a,
                last: b,
                stride: None,
            }
        } else {
            Self {
                first: b,
                last: a,
                stride: None,
            }
        }
    }

    /// Union of two ranges (widest first..last; stride is dropped when merging).
    pub fn union(self, other: Self) -> Self {
        Self {
            first: Tick(self.first.0.min(other.first.0)),
            last: Tick(self.last.0.max(other.last.0)),
            stride: None,
        }
    }

    /// Number of distinct ticks covered (inclusive), ignoring stride.
    pub fn tick_count(self) -> u64 {
        (self.last.0 - self.first.0).unsigned_abs() + 1
    }
}

/// Soft budget on expanded upstream ticks per node (extends the TimeOffset
/// four-offset soft cap from compile.rs). Past this, callers should emit a
/// `CompileDiagnostic` rather than expand unboundedly.
pub const SOURCE_RANGE_SOFT_CAP: u64 = 16;

/// Source range declared by one IR op for output tick `out`.
///
/// Pure functions of inputs (blur, grade, merge, solid, …) return the identity.
/// Temporal ops expand:
/// - `DecodeVideo` / stills / vectors — identity at their resolved `src_time`
///   is already baked into the op; they still report identity on the *output*
///   clock (the compile pass resolved source time).
/// - Future: deinterlace → `[out−1, out+1]`, motion blur → shutter window.
pub fn source_range_for_op(op: &IrOp, out: Tick) -> FrameRange {
    match op {
        // K-G6 deinterlace: neighbouring fields for temporal methods (32 §1).
        // Spatial methods still declare the full range so prefetch warms the
        // window and a future temporal path needs no contract change.
        IrOp::Deinterlace { .. } => {
            FrameRange::span(Tick(out.0.saturating_sub(1)), Tick(out.0.saturating_add(1)))
        }

        // Present-day pure ops and compile-expanded TimeOffset: identity.
        IrOp::DecodeVideo { .. }
        | IrOp::DecodeStill { .. }
        | IrOp::RasterVector { .. }
        | IrOp::SolidColor { .. }
        | IrOp::Transform2D { .. }
        // D-12 stabilization corrects *within* a frame — it never reaches for
        // a neighbour. Rolling-shutter correction would still not change this:
        // it samples the orientation curve at sub-frame times, not other
        // frames' pixels.
        | IrOp::StabilizeWarp { .. }
        | IrOp::Effect { .. }
        | IrOp::Grade { .. }
        | IrOp::Merge { .. }
        | IrOp::WipeMix { .. }
        | IrOp::PushMix { .. }
        | IrOp::LumaWipeMix { .. }
        | IrOp::CaptionOverlay { .. }
        | IrOp::Crop { .. }
        | IrOp::Resize { .. }
        | IrOp::MatteExtract { .. }
        | IrOp::TextGen { .. }
        | IrOp::ChannelSplit { .. }
        | IrOp::ChannelCombine
        | IrOp::Output { .. } => FrameRange::identity(out),
    }
}

/// Union of every node's source range in `graph` at output tick `out`.
/// This is the decode window the prefetch layer should warm.
pub fn graph_source_range(graph: &FrameGraph, out: Tick) -> FrameRange {
    let mut acc = FrameRange::identity(out);
    for node in &graph.nodes {
        acc = acc.union(source_range_for_op(&node.op, out));
    }
    acc
}

/// True when a declared range exceeds the soft expansion budget.
pub fn exceeds_soft_cap(range: FrameRange) -> bool {
    range.tick_count() > SOURCE_RANGE_SOFT_CAP
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::ir::{ContentHash, FrameGraph, IrNode, IrOp, LinearColor};

    #[test]
    fn identity_default_for_solid() {
        let out = Tick(1000);
        let r = source_range_for_op(
            &IrOp::SolidColor {
                color: LinearColor {
                    r: 1.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            },
            out,
        );
        assert_eq!(r, FrameRange::identity(out));
        assert_eq!(r.tick_count(), 1);
    }

    #[test]
    fn graph_of_pure_ops_stays_identity() {
        let out = Tick(42);
        let g = FrameGraph {
            nodes: vec![
                IrNode {
                    op: IrOp::SolidColor {
                        color: LinearColor {
                            r: 0.0,
                            g: 1.0,
                            b: 0.0,
                            a: 1.0,
                        },
                    },
                    inputs: vec![],
                    content_hash: ContentHash(1),
                },
                IrNode {
                    op: IrOp::Output { w: 8, h: 8 },
                    inputs: vec![(crate::graph::ir::IrNodeId(0), Default::default())],
                    content_hash: ContentHash(2),
                },
            ],
            output: Some(crate::graph::ir::IrNodeId(1)),
        };
        let r = graph_source_range(&g, out);
        assert_eq!(r, FrameRange::identity(out));
        assert!(!exceeds_soft_cap(r));
    }

    #[test]
    fn span_and_union_widen() {
        let a = FrameRange::span(Tick(0), Tick(10));
        let b = FrameRange::span(Tick(5), Tick(20));
        let u = a.union(b);
        assert_eq!(u.first, Tick(0));
        assert_eq!(u.last, Tick(20));
        assert_eq!(u.tick_count(), 21);
        assert!(exceeds_soft_cap(u));
    }

    #[test]
    fn soft_cap_is_sixteen() {
        assert_eq!(SOURCE_RANGE_SOFT_CAP, 16);
        assert!(!exceeds_soft_cap(FrameRange::span(Tick(0), Tick(15))));
        assert!(exceeds_soft_cap(FrameRange::span(Tick(0), Tick(16))));
    }

    #[test]
    fn deinterlace_declares_neighbour_fields() {
        use crate::graph::ir::{DeinterlaceMethod, FieldOrder};
        let out = Tick(100);
        let r = source_range_for_op(
            &IrOp::Deinterlace {
                method: DeinterlaceMethod::LinearBlend,
                field_order: FieldOrder::TopFirst,
            },
            out,
        );
        assert_eq!(r.first, Tick(99));
        assert_eq!(r.last, Tick(101));
        assert_eq!(r.tick_count(), 3);
    }
}
