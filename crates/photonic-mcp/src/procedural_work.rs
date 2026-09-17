use crate::protocol::{ArrayMode, CreateArrayArgs, MAX_ARRAY_GRID_CELLS, MAX_GENERATED_WORK};
use std::convert::TryFrom;

fn enforce_limit(operation: &str, work: usize, unit: &str) -> Result<(), String> {
    if work > MAX_GENERATED_WORK {
        return Err(format!(
            "{operation} may generate at most {MAX_GENERATED_WORK} {unit}"
        ));
    }
    Ok(())
}

pub(crate) fn check_spiral_work(turns: f64, segments_per_turn: usize) -> Result<(), String> {
    if !turns.is_finite() || turns <= 0.0 {
        return Err("turns must be a finite number greater than 0".to_string());
    }

    let effective_turns = turns.max(0.01);
    let effective_segments_per_turn = segments_per_turn.max(4);
    let generated_segments = (effective_turns * effective_segments_per_turn as f64).round();
    if !generated_segments.is_finite() || generated_segments > MAX_GENERATED_WORK as f64 {
        return Err(format!(
            "create_spiral may generate at most {MAX_GENERATED_WORK} Bézier segments"
        ));
    }

    Ok(())
}

pub(crate) fn check_rectangular_grid_work(
    cols: Option<u32>,
    rows: Option<u32>,
) -> Result<(), String> {
    let cols = usize::try_from(cols.unwrap_or(4).max(1))
        .map_err(|_| "create_grid column count overflow".to_string())?;
    let rows = usize::try_from(rows.unwrap_or(4).max(1))
        .map_err(|_| "create_grid row count overflow".to_string())?;
    let generated_lines = cols
        .checked_add(rows)
        .and_then(|lines| lines.checked_add(2))
        .ok_or_else(|| "create_grid generated-line count overflow".to_string())?;

    enforce_limit("create_grid", generated_lines, "grid lines")
}

pub(crate) fn check_polar_grid_work(
    rings: Option<u32>,
    sectors: Option<u32>,
) -> Result<(), String> {
    let rings = usize::try_from(rings.unwrap_or(4).max(1))
        .map_err(|_| "create_polar_grid ring count overflow".to_string())?;
    let sectors = usize::try_from(sectors.unwrap_or(8).max(1))
        .map_err(|_| "create_polar_grid sector count overflow".to_string())?;
    let generated_parts = rings
        .checked_add(sectors)
        .and_then(|parts| parts.checked_add(1))
        .ok_or_else(|| "create_polar_grid generated-part count overflow".to_string())?;

    enforce_limit("create_polar_grid", generated_parts, "grid parts")
}

pub(crate) fn check_flare_work(ray_count: usize, ring_count: usize) -> Result<(), String> {
    let generated_nodes = 2usize
        .checked_add(ray_count)
        .and_then(|count| count.checked_add(ring_count))
        .ok_or_else(|| "Lens flare generated-node count overflow".to_string())?;

    enforce_limit(
        "create_flare",
        generated_nodes,
        "nodes, including the halo and group",
    )
}

pub(crate) fn check_array_work(args: &CreateArrayArgs) -> Result<(), String> {
    match &args.mode {
        ArrayMode::Grid => {
            let rows = args.rows.unwrap_or(2).max(1);
            let cols = args.cols.unwrap_or(2).max(1);
            let cell_count = rows.checked_mul(cols).ok_or_else(|| {
                "Grid dimensions overflow before array allocation (rows × cols)".to_string()
            })?;
            if cell_count > MAX_ARRAY_GRID_CELLS {
                return Err(format!(
                    "Grid must have at most {MAX_ARRAY_GRID_CELLS} cells (rows × cols)"
                ));
            }
            if cell_count < 2 {
                return Err("Grid must have at least 2 cells (rows × cols ≥ 2)".to_string());
            }
            Ok(())
        }
        ArrayMode::Radial => {
            let count = args.count.unwrap_or(6);
            if count < 2 {
                return Err("Radial count must be ≥ 2".to_string());
            }
            enforce_limit("create_array", count, "radial instances")
        }
    }
}

pub(crate) fn check_scatter_work(count: Option<usize>) -> Result<(), String> {
    let count = count.unwrap_or(20).max(1);
    enforce_limit("scatter_copies", count, "copies")
}

pub(crate) fn check_split_work(rows: usize, cols: usize) -> Result<(), String> {
    if rows == 0 {
        return Err("rows must be ≥ 1".to_string());
    }
    if cols == 0 {
        return Err("cols must be ≥ 1".to_string());
    }

    let cell_count = rows
        .checked_mul(cols)
        .ok_or_else(|| "rows × cols overflow before grid allocation".to_string())?;
    enforce_limit("split_into_grid", cell_count, "cells (rows × cols)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spiral_work_rejects_non_finite_and_huge_turns() {
        for turns in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX] {
            assert!(
                check_spiral_work(turns, 16).is_err(),
                "turns={turns:?} must be rejected"
            );
        }
    }
}
