//! Crop/zoom solving for D-12 (22 §6.4 step 8).
//!
//! Rotating the frame to cancel camera shake swings parts of the output
//! rectangle outside the pixels the sensor actually captured. Those pixels do
//! not exist, so the choices are: zoom in until the output is covered, or show
//! the hole. This module decides how far to zoom.
//!
//! ## Why bisection rather than a closed form
//!
//! For a pinhole lens and a pure rotation the mapping is a homography, and the
//! required zoom has a closed form. For a fisheye it does not — the projection
//! is nonlinear, so the boundary of the covered region is a curve, not a
//! polygon. Bisection on "is the whole output boundary inside the source?" is
//! correct for *both*, costs a fixed ~24 evaluations per frame, and cannot be
//! wrong in the way a homography approximation applied to a fisheye silently
//! would be.
//!
//! ## Why the boundary is sampled, not just its corners
//!
//! Under a fisheye the image of a straight output edge bows. A corners-only
//! test passes while the bulge of an edge hangs outside the frame, which reads
//! as a black nick along one side — exactly the artefact `StaticSafe` promises
//! never to produce.

use glam::{DMat3, DVec3};

use super::lens::LensProfile;

/// Points sampled along each edge of the output rectangle, corners included.
///
/// Twelve per edge tracks the bow of a strongly-distorting fisheye to well
/// under a pixel at 4K, and the whole test is a few hundred floating-point
/// operations per bisection step.
const EDGE_SAMPLES: usize = 12;

/// Bisection steps. 24 halvings resolve the zoom to ~1e-7 over the search
/// range, far finer than a pixel.
const BISECTION_STEPS: u32 = 24;

/// Extra zoom applied on top of the exact solution.
///
/// The exact answer puts the boundary *on* the edge, where a bilinear tap still
/// reaches for the neighbouring texel and picks up the clamped border. A
/// fraction of a percent buys that margin back and is invisible.
const SAFETY_MARGIN: f64 = 1.002;

/// Smallest render scale, relative to the analysis resolution, that the solved
/// crop is guaranteed safe at.
///
/// The containment test runs once, at analysis resolution, but the warp runs at
/// whatever the evaluator is rendering — which for a proxy preview is smaller.
/// The valid source region is `[0, w-1]`, so that one-texel inset is a *larger
/// fraction of the frame* the smaller the render gets: at 640 px it is 0.16 %,
/// at 160 px it is 0.63 %. A crop solved to sit exactly on the boundary at
/// analysis resolution therefore hangs outside it on a downscaled render, and
/// an edge shows.
///
/// Guarding by one texel at this scale makes the solution safe for every render
/// down to it. A quarter covers the usual proxy ladder (½, ⅓, ¼) with room to
/// spare; below it, solve against the resolution you intend to render.
const MIN_RENDER_SCALE: f64 = 0.25;

/// What the solver decided.
#[derive(Clone, Debug, PartialEq)]
pub struct CropSolution {
    /// Per-frame zoom, `>= 1.0`. Length matches the input rotations.
    pub zoom: Vec<f32>,
    /// Largest zoom any frame *needed*, before clamping to `max_zoom`.
    pub max_required: f32,
    /// Contiguous frame range that could not be covered within `max_zoom`,
    /// as `(first, last)` inclusive.
    ///
    /// 22 §6.7: the impossible case "reports the range" — it does not silently
    /// clip, and it does not fail the whole analysis either, because the user
    /// may legitimately choose to accept a few exposed frames.
    pub infeasible: Option<(usize, usize)>,
}

/// Is the whole output boundary, at `zoom`, inside the source image?
///
/// Walks the output rectangle's perimeter, unprojects each point through the
/// lens, rotates it into the source camera's frame, projects it back, and
/// checks it landed within the source rectangle.
fn covered(rot: DMat3, lens: &LensProfile, w: f64, h: f64, zoom: f64) -> bool {
    let (cx, cy) = (w * 0.5, h * 0.5);
    let inv = 1.0 / zoom;
    let boundary = |px: f64, py: f64| -> bool {
        // Zoom is a scale about the frame centre applied in output pixels.
        let ox = cx + (px - cx) * inv;
        let oy = cy + (py - cy) * inv;
        let ray = lens.unproject(ox, oy, w, h);
        let src = rot * ray;
        match lens.project(DVec3::new(src.x, src.y, src.z), w, h) {
            Some((sx, sy)) => sx >= 0.0 && sy >= 0.0 && sx <= w - 1.0 && sy <= h - 1.0,
            // A ray that does not project forward is by definition not covered.
            None => false,
        }
    };
    for i in 0..=EDGE_SAMPLES {
        let f = i as f64 / EDGE_SAMPLES as f64;
        let (x, y) = (f * (w - 1.0), f * (h - 1.0));
        if !boundary(x, 0.0) || !boundary(x, h - 1.0) || !boundary(0.0, y) || !boundary(w - 1.0, y)
        {
            return false;
        }
    }
    true
}

/// Smallest zoom in `[1, max_zoom]` that covers the frame, or `None` if even
/// `max_zoom` leaves a hole.
pub fn required_zoom(rot: DMat3, lens: &LensProfile, w: f64, h: f64, max_zoom: f64) -> Option<f64> {
    if covered(rot, lens, w, h, 1.0) {
        return Some(1.0);
    }
    if !covered(rot, lens, w, h, max_zoom) {
        return None;
    }
    let (mut lo, mut hi) = (1.0, max_zoom);
    for _ in 0..BISECTION_STEPS {
        let mid = 0.5 * (lo + hi);
        if covered(rot, lens, w, h, mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    // Applied to the *solved* zoom rather than to the containment box: insetting
    // the box would make even an identity rotation infeasible, since at zoom 1
    // the output boundary maps exactly onto `[0, w-1]`. A still camera must
    // cost nothing.
    //
    // The extra covers one texel at `MIN_RENDER_SCALE`, expressed as a fraction
    // of the half-extent — that is the margin a downscaled render needs and the
    // analysis-resolution test cannot see.
    let render_guard = 1.0 + 2.0 * (1.0 / MIN_RENDER_SCALE) / w.min(h);
    Some((hi * SAFETY_MARGIN * render_guard).min(max_zoom))
}

/// Temporal smoothing window for [`StabilizationCropMode::Dynamic`], seconds.
///
/// [`StabilizationCropMode::Dynamic`]: photonic_core::timeline::StabilizationCropMode
const DYNAMIC_WINDOW_S: f64 = 2.0;

/// Solve the per-frame zoom for the whole clip.
///
/// `per_frame` is each frame's independently required zoom, `None` where even
/// `max_zoom` was insufficient.
pub fn solve(per_frame: &[Option<f64>], mode: CropMode, max_zoom: f64, fps: f64) -> CropSolution {
    let max_required = per_frame
        .iter()
        .filter_map(|z| *z)
        .fold(1.0_f64, f64::max)
        .max(1.0);

    // First and last frame the solver could not cover.
    let infeasible = {
        let first = per_frame.iter().position(|z| z.is_none());
        first.map(|f| {
            let last = per_frame
                .iter()
                .rposition(|z| z.is_none())
                .expect("a first implies a last");
            (f, last)
        })
    };

    let zoom = match mode {
        // Never zoom; uncovered pixels stay transparent by design.
        CropMode::TransparentEdges => vec![1.0f32; per_frame.len()],

        // One zoom for the whole clip, sized for its worst frame. An
        // infeasible frame contributes `max_zoom` rather than being skipped:
        // going as far as allowed minimises how much of it is exposed.
        CropMode::StaticSafe => {
            let z = per_frame
                .iter()
                .map(|z| z.unwrap_or(max_zoom))
                .fold(1.0_f64, f64::max)
                .clamp(1.0, max_zoom);
            vec![z as f32; per_frame.len()]
        }

        // Dilate-then-average. Both halves use the *same* half-window `h`, and
        // that is what makes the result provably safe:
        //
        //   W[i] = max over [i-h, i+h] of raw
        //   S[i] = mean over [i-h, i+h] of W
        //
        // For any j within h of i, W[j] is a max over a range that contains i,
        // so W[j] >= raw[i]. The mean of values all >= raw[i] is >= raw[i].
        // Therefore S[i] >= raw[i] for every frame, with no clamping needed.
        //
        // The earlier shape here — low-pass, then max against the raw
        // requirement — was self-defeating: forcing the output back up to the
        // unsmoothed signal reintroduced exactly the step edges the filter had
        // just removed, so the zoom still pumped. Averaging a dilated signal
        // gives a genuine ramp instead, because a boxcar mean of a step is
        // linear over its support.
        CropMode::Dynamic => {
            let half = ((DYNAMIC_WINDOW_S * fps * 0.5) as usize).max(1);
            let raw: Vec<f64> = per_frame
                .iter()
                .map(|z| z.unwrap_or(max_zoom).clamp(1.0, max_zoom))
                .collect();
            let window = |i: usize, len: usize| (i.saturating_sub(half), (i + half + 1).min(len));
            let dilated: Vec<f64> = (0..raw.len())
                .map(|i| {
                    let (lo, hi) = window(i, raw.len());
                    raw[lo..hi].iter().fold(1.0_f64, |a, b| a.max(*b))
                })
                .collect();
            (0..dilated.len())
                .map(|i| {
                    let (lo, hi) = window(i, dilated.len());
                    let mean = dilated[lo..hi].iter().sum::<f64>() / (hi - lo) as f64;
                    mean.clamp(1.0, max_zoom) as f32
                })
                .collect()
        }
    };

    CropSolution {
        zoom,
        max_required: max_required as f32,
        infeasible,
    }
}

/// Crop policy, mirroring the persisted
/// [`StabilizationCropMode`](photonic_core::timeline::StabilizationCropMode)
/// minus its forward-compat variant — an unknown mode never reaches the solver
/// because [`StabilizationSpec::validate`] rejects it first.
///
/// [`StabilizationSpec::validate`]: photonic_core::timeline::StabilizationSpec::validate
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CropMode {
    StaticSafe,
    Dynamic,
    TransparentEdges,
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::DQuat;

    fn lens() -> LensProfile {
        LensProfile::ideal_pinhole(1920.0, 1080.0, 90.0)
    }

    fn rot(yaw: f64) -> DMat3 {
        DMat3::from_quat(DQuat::from_rotation_y(yaw))
    }

    #[test]
    fn identity_rotation_needs_no_zoom() {
        let z = required_zoom(DMat3::IDENTITY, &lens(), 1920.0, 1080.0, 2.0).unwrap();
        assert_eq!(z, 1.0, "an unrotated frame is already covered");
    }

    #[test]
    fn larger_rotations_demand_more_zoom() {
        let l = lens();
        let mut last = 0.0;
        for deg in [1.0_f64, 3.0, 6.0, 10.0] {
            let z = required_zoom(rot(deg.to_radians()), &l, 1920.0, 1080.0, 4.0)
                .expect("should be feasible within 4x");
            assert!(z > last, "{deg}° gave {z}, not more than {last}");
            last = z;
        }
    }

    #[test]
    fn solved_zoom_actually_covers_the_frame() {
        // The property that matters: what the solver returns must pass the
        // very test it was solving.
        let l = lens();
        for deg in [2.0_f64, 5.0, 9.0] {
            let r = rot(deg.to_radians());
            let z = required_zoom(r, &l, 1920.0, 1080.0, 4.0).unwrap();
            assert!(
                covered(r, &l, 1920.0, 1080.0, z),
                "{deg}° solved to {z} but is not covered"
            );
        }
    }

    #[test]
    fn impossible_rotation_reports_infeasible() {
        // 22 §6.7: an impossible crop reports rather than silently clipping.
        let z = required_zoom(rot(80f64.to_radians()), &lens(), 1920.0, 1080.0, 1.2);
        assert_eq!(z, None);
    }

    #[test]
    fn fisheye_edge_bulge_is_caught() {
        // Corners-only testing would pass here while an edge hangs out.
        let fish = LensProfile {
            model: super::super::lens::DistortionModel::Fisheye,
            fx: 500.0,
            fy: 500.0,
            cx: 960.0,
            cy: 540.0,
            k: [0.05, -0.01, 0.002, -0.0001],
            calib_width: 1920.0,
            calib_height: 1080.0,
            frame_readout_time_s: None,
            global_shutter: false,
            name: "test".into(),
        };
        let r = rot(6f64.to_radians());
        let z = required_zoom(r, &fish, 1920.0, 1080.0, 4.0).unwrap();
        assert!(covered(r, &fish, 1920.0, 1080.0, z));
        assert!(z > 1.0);
    }

    // ── whole-clip solving ──────────────────────────────────────────────

    #[test]
    fn static_safe_uses_one_zoom_sized_for_the_worst_frame() {
        let per = vec![Some(1.05), Some(1.20), Some(1.02)];
        let s = solve(&per, CropMode::StaticSafe, 2.0, 30.0);
        assert!(s.zoom.iter().all(|z| (*z - 1.20).abs() < 1e-6));
        assert!((s.max_required - 1.20).abs() < 1e-6);
        assert_eq!(s.infeasible, None);
    }

    #[test]
    fn transparent_edges_never_zooms() {
        let per = vec![Some(1.5), Some(2.0), None];
        let s = solve(&per, CropMode::TransparentEdges, 2.0, 30.0);
        assert!(s.zoom.iter().all(|z| *z == 1.0));
    }

    #[test]
    fn infeasible_frames_are_reported_as_a_range() {
        let per = vec![Some(1.0), None, None, Some(1.1)];
        let s = solve(&per, CropMode::StaticSafe, 1.5, 30.0);
        assert_eq!(s.infeasible, Some((1, 2)));
        // Still produces usable output, clamped to the ceiling.
        assert!(s.zoom.iter().all(|z| *z <= 1.5));
    }

    #[test]
    fn dynamic_never_dips_below_the_requirement() {
        // The safety property: a plain low-pass would cut the corner off this
        // spike and expose an edge on frame 20.
        let mut per = vec![Some(1.0); 60];
        per[20] = Some(1.8);
        let s = solve(&per, CropMode::Dynamic, 2.0, 30.0);
        for (i, req) in per.iter().enumerate() {
            let need = req.unwrap();
            assert!(
                s.zoom[i] as f64 >= need - 1e-6,
                "frame {i} zoomed {} but needed {need}",
                s.zoom[i]
            );
        }
    }

    #[test]
    fn dynamic_keeps_more_field_of_view_than_static_on_calm_footage() {
        // The entire reason Dynamic exists: one bad moment should not cost the
        // whole clip its framing.
        let mut per = vec![Some(1.0); 300];
        per[150] = Some(1.9);
        let dyn_s = solve(&per, CropMode::Dynamic, 2.0, 30.0);
        let static_s = solve(&per, CropMode::StaticSafe, 2.0, 30.0);
        assert!(
            dyn_s.zoom[0] < static_s.zoom[0],
            "dynamic {} should beat static {} far from the spike",
            dyn_s.zoom[0],
            static_s.zoom[0]
        );
        assert!((dyn_s.zoom[0] - 1.0).abs() < 0.05);
    }

    #[test]
    fn dynamic_zoom_is_smooth_enough_not_to_pump() {
        let mut per = vec![Some(1.0); 120];
        for f in per.iter_mut().take(65).skip(60) {
            *f = Some(1.6);
        }
        let s = solve(&per, CropMode::Dynamic, 2.0, 30.0);
        let max_step = s
            .zoom
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        // A boxcar mean of a dilated step ramps over its whole support, so the
        // per-frame change is roughly (rise / 2h) — here about 0.01.
        assert!(
            max_step < 0.03,
            "largest per-frame zoom step was {max_step}"
        );
    }

    #[test]
    fn empty_input_is_handled() {
        let s = solve(&[], CropMode::StaticSafe, 2.0, 30.0);
        assert!(s.zoom.is_empty());
        assert_eq!(s.infeasible, None);
        assert_eq!(s.max_required, 1.0);
    }
}
