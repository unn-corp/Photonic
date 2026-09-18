/// Path offsetting: expand or contract a path by a given distance.
/// Used for inset/outset operations.
use kurbo::{BezPath, Cap, Join, PathEl, Stroke as KurboStroke};

use crate::path::PathData;

/// Offset a path outward (positive distance) or inward (negative distance).
///
/// Uses kurbo's stroke expansion centered on the path boundary, then extracts
/// the outer contour (outset) or inner contour (inset) from the result.
///
/// - `distance > 0`: expand the path outward
/// - `distance < 0`: contract the path inward
/// - `distance == 0`: returns a clone of the original path unchanged
/// - `join`: corner join style — `Join::Miter`, `Join::Round`, or `Join::Bevel`
///
/// Returns `Err` if the offset produces no geometry (e.g. an inset that
/// collapses the path entirely).
pub fn offset_path(path: &PathData, distance: f64, join: Join) -> Result<PathData, String> {
    if distance == 0.0 {
        return Ok(path.clone());
    }

    let bez = path.to_bez_path();
    let is_closed = is_path_closed(&bez);

    let kurbo_style = KurboStroke {
        width: distance.abs() * 2.0,
        join,
        miter_limit: 4.0,
        start_cap: Cap::Butt,
        end_cap: Cap::Butt,
        dash_pattern: Default::default(),
        dash_offset: 0.0,
    };

    let expanded: BezPath = kurbo::stroke(&bez, &kurbo_style, &Default::default(), 0.1);
    let sub_paths = split_into_subpaths(&expanded);

    if sub_paths.is_empty() {
        return Err("Offset produced no output".into());
    }

    if is_closed && distance < 0.0 {
        // Inset on a closed path: the inner contour is the second sub-path.
        sub_paths
            .get(1)
            .map(PathData::from_bez_path)
            .ok_or_else(|| {
                format!(
                    "Path is too small to inset by {:.1} — inner contour collapsed",
                    distance.abs()
                )
            })
    } else {
        // Outset (closed), or any direction on an open path: use the first sub-path.
        Ok(PathData::from_bez_path(&sub_paths[0]))
    }
}

/// Returns true if the path ends with a `ClosePath` element.
fn is_path_closed(bez: &BezPath) -> bool {
    bez.elements()
        .last()
        .is_some_and(|el| matches!(el, PathEl::ClosePath))
}

/// Split a `BezPath` into individual sub-paths at every `MoveTo`.
fn split_into_subpaths(bez: &BezPath) -> Vec<BezPath> {
    let mut result: Vec<BezPath> = Vec::new();
    let mut current = BezPath::new();

    for el in bez.elements() {
        match el {
            PathEl::MoveTo(pt) => {
                if !current.elements().is_empty() {
                    result.push(current.clone());
                    current = BezPath::new();
                }
                current.move_to(*pt);
            }
            _ => current.push(*el),
        }
    }

    if !current.elements().is_empty() {
        result.push(current);
    }

    result
}
