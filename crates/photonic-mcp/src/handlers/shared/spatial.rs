use photonic_core::node::SceneNode;

/// Compute a node's world-space axis-aligned bounding box as `[x, y, width,
/// height]`. The node transform is applied to all four corners of its local
/// bounds so the result remains correct for rotation, shear, and reflection.
pub(crate) fn world_aabb(node: &SceneNode) -> Option<[f64; 4]> {
    let local = node.local_bounds()?;
    let affine = node.transform.to_kurbo();
    let corners = [
        affine * kurbo::Point::new(local.x0, local.y0),
        affine * kurbo::Point::new(local.x1, local.y0),
        affine * kurbo::Point::new(local.x1, local.y1),
        affine * kurbo::Point::new(local.x0, local.y1),
    ];
    let x0 = corners
        .iter()
        .map(|point| point.x)
        .fold(f64::INFINITY, f64::min);
    let y0 = corners
        .iter()
        .map(|point| point.y)
        .fold(f64::INFINITY, f64::min);
    let x1 = corners
        .iter()
        .map(|point| point.x)
        .fold(f64::NEG_INFINITY, f64::max);
    let y1 = corners
        .iter()
        .map(|point| point.y)
        .fold(f64::NEG_INFINITY, f64::max);
    Some([x0, y0, x1 - x0, y1 - y0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::{
        node::{GroupNode, PathNode, SceneNodeKind},
        path::PathData,
        transform::Transform,
    };

    fn rect_node(transform: Transform) -> SceneNode {
        SceneNode::new(
            "rect",
            uuid::Uuid::new_v4(),
            SceneNodeKind::Path(PathNode::new(PathData::rect(0.0, 0.0, 100.0, 50.0))),
        )
        .with_transform(transform)
    }

    fn assert_aabb(actual: [f64; 4], expected: [f64; 4]) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
        }
    }

    #[test]
    fn world_aabb_transforms_all_four_corners() {
        let angle = std::f64::consts::FRAC_PI_4;
        let node = rect_node(Transform::new(
            angle.cos(),
            angle.sin(),
            -angle.sin(),
            angle.cos(),
            20.0,
            30.0,
        ));
        let extent = 75.0 * 2.0_f64.sqrt();
        assert_aabb(
            world_aabb(&node).unwrap(),
            [20.0 - 25.0 * 2.0_f64.sqrt(), 30.0, extent, extent],
        );
    }

    #[test]
    fn world_aabb_handles_shear_reflection_and_missing_bounds() {
        let node = rect_node(Transform::new(-1.0, 0.5, 1.5, -1.0, 20.0, 30.0));
        assert_aabb(world_aabb(&node).unwrap(), [-80.0, -20.0, 175.0, 100.0]);

        let group = SceneNode::new(
            "group",
            uuid::Uuid::new_v4(),
            SceneNodeKind::Group(GroupNode::new()),
        );
        assert!(world_aabb(&group).is_none());
    }
}
