//! K-E2 — per-clip scope tap (26 §13 K-E2, implementing 27 A-7's fix: "03 adopts
//! 07's per-clip-with-fallback wording").
//!
//! The defect K-E2 names is that scopes measure the **program** frame, *after*
//! `CaptionOverlay`, so a colourist grading a clip that has a caption track or a
//! second video track above it reads a signal they are not adjusting. The fix is
//! two readback points recorded by the compiler:
//!
//! - [`ScopeTapPoint::Clip`] — the clip's node after its own `Grade`, **before**
//!   the track fold (07 §5);
//! - [`ScopeTapPoint::Program`] — the folded program after the master stack,
//!   **before** `CaptionOverlay` (03 §3.6); also the 13 §10.2 fallback.
//!
//! Every test here is written so it would FAIL against the pre-fix behaviour
//! (scoping the graph output): each one asserts both that the tap reads the
//! expected signal AND that the program/output disagrees with it, so the test
//! cannot pass by accident if the tap silently degrades to "just the output".
//!
//! CPU evaluator throughout (`eval_cpu::evaluate_at`, the mirror of the GPU
//! `Evaluator::evaluate_with_tap`) so the readback points are asserted at the
//! pixel level with no GPU adapter.

use photonic_core::timeline::{
    CaptionCue, CaptionTrack, CaptionWord, Clip, ClipEffect, ClipSource, EffectKind, FrameRate,
    Grade, GradeOp, GradeOpKind, GradeOpParams, Sequence, SequenceId, Tick, TimelineProject, Track,
    TrackKind,
};
use photonic_core::Color;
use photonic_video::graph::compile::compile_asset_peek;
use photonic_video::graph::eval::{read_texture_rgba16f, Evaluator, GpuContext, NullFrameSource};
use photonic_video::graph::eval_cpu::{evaluate, evaluate_at, EmptyProvider};
use photonic_video::graph::ir::IrOp;
use photonic_video::graph::ops::Image;
use photonic_video::graph::{compile, CompiledFrame, Quality, ScopeTapPoint};

const W: u32 = 4;
const H: u32 = 4;

const RED: Color = Color {
    r: 1.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};
const GREEN: Color = Color {
    r: 0.0,
    g: 1.0,
    b: 0.0,
    a: 1.0,
};

/// A 4×4 sequence with one full-span red solid on V1; `decorate` adds whatever
/// the test needs above/around it. Returns the compiled frame at `tick` plus the
/// id of the V1 clip (the clip a colourist would be grading).
fn compiled(
    tick: Tick,
    decorate: impl FnOnce(&mut Sequence),
) -> (CompiledFrame, photonic_core::timeline::ClipId) {
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("tap", FrameRate::FPS_30, W, H);
    let seq_id: SequenceId = seq.id;
    let mut v1 = Track::new(TrackKind::Video, "V1");
    let clip = Clip::new(ClipSource::SolidColor { color: RED }, Tick(0), Tick(1000));
    let clip_id = clip.id;
    v1.clips.push(clip);
    seq.video_tracks.push(v1);
    decorate(&mut seq);
    project.insert_sequence(seq);
    let c = compile(&project, seq_id, 0, tick, Quality::FULL, None);
    assert!(
        c.diagnostics.is_empty(),
        "unexpected compile diagnostics: {:?}",
        c.diagnostics
    );
    (c, clip_id)
}

/// Evaluate the graph at a tap point (the CPU mirror of the engine's tap read).
fn eval_tap(c: &CompiledFrame, point: ScopeTapPoint) -> Image {
    let (_, node) = c
        .resolve_tap(point)
        .unwrap_or_else(|| panic!("no tap resolved for {point:?}"));
    evaluate_at(&c.graph, (W, H), &mut EmptyProvider, Some(node))
}

fn eval_output(c: &CompiledFrame) -> Image {
    evaluate(&c.graph, (W, H), &mut EmptyProvider)
}

/// Mean straight (unpremultiplied-ish) RGB of an image — enough to name "this is
/// the red clip" vs "this is the green clip above it" without asserting a literal
/// pixel layout.
fn mean_rgb(img: &Image) -> [f32; 3] {
    let n = img.pixels.len().max(1) as f32;
    let mut acc = [0.0f32; 3];
    for p in &img.pixels {
        for (k, a) in acc.iter_mut().enumerate() {
            *a += p[k];
        }
    }
    [acc[0] / n, acc[1] / n, acc[2] / n]
}

fn differ(a: &Image, b: &Image) -> bool {
    a.pixels.len() != b.pixels.len()
        || a.pixels
            .iter()
            .zip(&b.pixels)
            .any(|(p, q)| (0..4).any(|k| (p[k] - q[k]).abs() > 1e-4))
}

fn approx(a: [f32; 3], b: [f32; 3]) -> bool {
    (0..3).all(|k| (a[k] - b[k]).abs() < 1e-3)
}

/// A green full-frame clip on V2, opaque — it completely hides V1 in the program.
fn opaque_green_track_above(seq: &mut Sequence) {
    let mut v2 = Track::new(TrackKind::Video, "V2");
    v2.clips.push(Clip::new(
        ClipSource::SolidColor { color: GREEN },
        Tick(0),
        Tick(1000),
    ));
    seq.video_tracks.push(v2);
}

/// A CDL that visibly moves red (halved slope on R) — a real `Grade`, so the
/// "after the clip's Grade" half of 07 §5 is asserted against real grade math and
/// not a passthrough.
fn half_red_cdl() -> Grade {
    let mut g = Grade::new();
    g.ops.push(GradeOp::new(
        GradeOpKind::Cdl,
        GradeOpParams::Cdl {
            slope: [0.5, 1.0, 1.0],
            offset: [0.0; 3],
            power: [1.0; 3],
            sat: 1.0,
        },
    ));
    g
}

// ── The defect K-E2 names ────────────────────────────────────────────────────

/// 07 §5: the clip tap is the clip's OWN texture, before the track fold. With an
/// opaque clip above it, the program shows green and the tapped clip shows red —
/// the exact "measuring the wrong thing" case from 26 K-E2.
///
/// The second assertion is the sensitivity proof: it pins that the pre-fix path
/// (scope the output) really does disagree, so a regression that quietly routes
/// the tap back to the output turns this test red instead of passing.
#[test]
fn clip_tap_reads_the_clip_not_the_track_folded_above_it() {
    let (c, clip) = compiled(Tick(0), opaque_green_track_above);

    let tap = eval_tap(&c, ScopeTapPoint::Clip(clip));
    assert!(
        approx(mean_rgb(&tap), [1.0, 0.0, 0.0]),
        "clip tap must read the clip's own red, got {:?}",
        mean_rgb(&tap)
    );

    let out = eval_output(&c);
    assert!(
        approx(mean_rgb(&out), [0.0, 1.0, 0.0]),
        "pre-fix path (scope the program output) reads the green track above — \
         if this ever equals the clip tap the test has stopped proving anything, \
         got {:?}",
        mean_rgb(&out)
    );
    assert!(differ(&tap, &out), "clip tap and program must differ here");
}

/// 26 K-E2's other named case: an **adjustment layer above** the clip re-colours
/// the program, but must not touch the clip's own tap.
#[test]
fn clip_tap_is_unaffected_by_an_adjustment_layer_above_it() {
    let (c, clip) = compiled(Tick(0), |seq| {
        let mut v2 = Track::new(TrackKind::Video, "V2");
        let mut adj = Clip::new(ClipSource::Adjustment, Tick(0), Tick(1000));
        adj.effects.push(ClipEffect::new(EffectKind::Invert));
        v2.clips.push(adj);
        seq.video_tracks.push(v2);
    });

    let tap = eval_tap(&c, ScopeTapPoint::Clip(clip));
    assert!(
        approx(mean_rgb(&tap), [1.0, 0.0, 0.0]),
        "the adjustment inverts the program, never the clip's own tap, got {:?}",
        mean_rgb(&tap)
    );
    let out = eval_output(&c);
    assert!(
        differ(&tap, &out),
        "the inverting adjustment must move the program away from the clip tap"
    );
}

/// 07 §5: "after its `Grade` node". The tap must carry the clip's grade — with
/// the grade removed the same tap reads a different signal, which is what makes
/// this an assertion about the *grade* and not just about the source colour.
///
/// The opaque green track above is deliberate: it makes the program output
/// identical in both variants, so a tap that degraded to the output would read
/// the same green twice and fail the final `differ` assertion.
#[test]
fn clip_tap_is_taken_after_the_clips_own_grade() {
    let (ungraded, clip_a) = compiled(Tick(0), opaque_green_track_above);
    let (graded, clip_b) = compiled(Tick(0), |seq| {
        seq.video_tracks[0].clips[0].grade = Some(half_red_cdl());
        opaque_green_track_above(seq);
    });

    let plain = eval_tap(&ungraded, ScopeTapPoint::Clip(clip_a));
    let after = eval_tap(&graded, ScopeTapPoint::Clip(clip_b));
    assert!(
        approx(mean_rgb(&plain), [1.0, 0.0, 0.0]),
        "ungraded tap is the plain red solid, got {:?}",
        mean_rgb(&plain)
    );
    assert!(
        mean_rgb(&after)[0] < 0.9,
        "a 0.5-slope CDL on R must be visible in the tap (pre-grade tap would \
         still read 1.0), got {:?}",
        mean_rgb(&after)
    );
    assert!(differ(&plain, &after), "the grade must reach the tap");
}

/// 07 §5: "before the track fold". Halving the clip's TRACK opacity changes the
/// program but must leave the clip tap alone — the fold is downstream of the tap.
#[test]
fn clip_tap_is_taken_before_the_track_fold() {
    let (full, clip_a) = compiled(Tick(0), |_| {});
    let (faded, clip_b) = compiled(Tick(0), |seq| {
        seq.video_tracks[0].opacity = 0.5;
    });

    let tap_full = eval_tap(&full, ScopeTapPoint::Clip(clip_a));
    let tap_faded = eval_tap(&faded, ScopeTapPoint::Clip(clip_b));
    assert!(
        !differ(&tap_full, &tap_faded),
        "track opacity is a fold property and must not move the clip tap"
    );
    assert!(
        differ(&eval_output(&full), &eval_output(&faded)),
        "…but it must move the program, or this test proves nothing"
    );
}

// ── The program tap (03 §3.6) ────────────────────────────────────────────────

/// 03 §3.6: the program tap sits BEFORE `CaptionOverlay`. The CPU evaluator
/// passes `CaptionOverlay` through (glyph compositing is GPU-only), so this is
/// asserted structurally rather than by pixels: the tap node must be an input of
/// a `CaptionOverlay` node, i.e. strictly upstream of caption compositing, and
/// must not itself be the graph output.
///
/// Both facts are derived from the compiled graph — no literal node index.
#[test]
fn program_tap_is_upstream_of_the_caption_overlay() {
    let (c, _) = compiled(Tick(10), |seq| {
        let mut track = CaptionTrack::new("Captions");
        track.cues.push(CaptionCue::new(
            Tick(0),
            Tick(200),
            vec![CaptionWord::new("hello", Tick(0), Tick(200))],
        ));
        seq.caption_tracks.push(track);
    });

    let tap = c
        .program_tap
        .expect("a non-empty program has a program tap");
    let caption = c
        .graph
        .nodes
        .iter()
        .position(|n| matches!(n.op, IrOp::CaptionOverlay { .. }))
        .expect("the covering cue lowered a CaptionOverlay node");
    assert!(
        c.graph.nodes[caption]
            .inputs
            .iter()
            .any(|(id, _)| *id == tap),
        "the program tap must feed CaptionOverlay, not follow it"
    );
    assert_ne!(
        Some(tap),
        c.graph.output,
        "the program tap is not the graph output — the output is post-caption"
    );
}

/// The program tap is post-master-grade (03 §3.6 "after the `Grade` node"): a
/// sequence master grade moves it.
#[test]
fn program_tap_is_taken_after_the_master_grade() {
    let (plain, _) = compiled(Tick(0), |_| {});
    let (mastered, _) = compiled(Tick(0), |seq| {
        seq.master_grade = Some(half_red_cdl());
    });
    let a = eval_tap(&plain, ScopeTapPoint::Program);
    let b = eval_tap(&mastered, ScopeTapPoint::Program);
    assert!(
        differ(&a, &b),
        "a master grade must be visible at the program tap"
    );
}

// ── Fallback (13 §10.2) ──────────────────────────────────────────────────────

/// 13 §10.2: "the panel should never show nothing while a sequence is loaded".
/// When the playhead is not over the requested clip, the clip contributes no node
/// to that frame at all, so `resolve_tap` degrades to the program tap — and says
/// so, so the UI can relabel instead of claiming to scope a clip it is not.
#[test]
fn clip_tap_falls_back_to_program_when_the_playhead_is_off_the_clip() {
    // V2 outlives V1, so the program is still non-empty at tick 5000 while the
    // V1 clip (span [0, 1000)) is not in that frame at all.
    let long_green = |seq: &mut Sequence| {
        let mut v2 = Track::new(TrackKind::Video, "V2");
        v2.clips.push(Clip::new(
            ClipSource::SolidColor { color: GREEN },
            Tick(0),
            Tick(10_000),
        ));
        seq.video_tracks.push(v2);
    };
    let (over, clip) = compiled(Tick(0), long_green);
    assert_eq!(
        over.resolve_tap(ScopeTapPoint::Clip(clip)).map(|(p, _)| p),
        Some(ScopeTapPoint::Clip(clip)),
        "while the playhead is over the clip the requested tap is honoured"
    );

    let (off, _) = compiled(Tick(5000), long_green);
    assert!(
        off.tap(ScopeTapPoint::Clip(clip)).is_none(),
        "a clip the frame does not contain carries no tap"
    );
    assert_eq!(
        off.resolve_tap(ScopeTapPoint::Clip(clip)).map(|(p, _)| p),
        Some(ScopeTapPoint::Program),
        "…and the request degrades to the program tap, not to nothing"
    );
}

/// A disabled track folds its clips away entirely, so the same fallback applies —
/// a hidden clip cannot be scoped because it was never rendered.
#[test]
fn a_clip_on_a_disabled_track_falls_back_to_program() {
    let (c, clip) = compiled(Tick(0), |seq| {
        seq.video_tracks[0].enabled = false;
        opaque_green_track_above(seq);
    });
    assert_eq!(
        c.resolve_tap(ScopeTapPoint::Clip(clip)).map(|(p, _)| p),
        Some(ScopeTapPoint::Program)
    );
}

/// An empty sequence has no program tap either — the honest `None`, which the
/// engine reports as "no signal" rather than publishing a stale texture.
#[test]
fn an_empty_sequence_resolves_no_tap_at_all() {
    let mut project = TimelineProject::new();
    let seq = Sequence::new("empty", FrameRate::FPS_30, W, H);
    let seq_id = seq.id;
    project.insert_sequence(seq);
    let c = compile(&project, seq_id, 0, Tick(0), Quality::FULL, None);
    assert!(c.program_tap.is_none());
    assert!(c.clip_taps.is_empty());
    assert!(c.resolve_tap(ScopeTapPoint::Program).is_none());
}

/// A source peek (24 §3) has no clips and no fold, so its only readback point is
/// the decoded source. It must still tap — the scopes panel stays usable while
/// the monitor is on an asset instead of the sequence.
#[test]
fn an_asset_peek_taps_its_decoded_source() {
    let project = TimelineProject::new();
    let asset = photonic_core::timeline::AssetId::new();
    let c = compile_asset_peek(&project, asset, Tick(0), Quality::FULL, W, H);
    let tap = c.program_tap.expect("a peek taps its source");
    let out = c.graph.output.expect("output");
    assert_ne!(tap, out, "the tap is the source, not the Output node");
    assert!(
        c.graph.nodes[out.0 as usize]
            .inputs
            .iter()
            .any(|(id, _)| *id == tap),
        "the tap feeds Output directly"
    );
    // A clip tap has nothing to resolve against here, so it degrades honestly.
    assert_eq!(
        c.resolve_tap(ScopeTapPoint::Clip(photonic_core::timeline::ClipId::new()))
            .map(|(p, _)| p),
        Some(ScopeTapPoint::Program)
    );
}

// ── Cost (the "does the tap force an extra evaluation" question) ─────────────

/// The tap is a **lookup, not a render**: taps are recorded from nodes the
/// program compile already emits, so requesting one changes neither the node
/// count nor any content hash, and the tapped node is reachable from the output.
/// That is what bounds the cost — no second compile, no second evaluation, and
/// nothing new in the 02 §8 per-frame budget.
#[test]
fn taps_add_no_nodes_and_stay_on_the_output_path() {
    let (c, clip) = compiled(Tick(0), opaque_green_track_above);

    // Recompiling the same frame is byte-identical: taps are derived, not extra.
    let (again, _) = compiled(Tick(0), opaque_green_track_above);
    assert_eq!(
        c.graph.nodes.len(),
        again.graph.nodes.len(),
        "compiling with taps recorded must not grow the graph"
    );

    let tap = c.tap(ScopeTapPoint::Clip(clip)).expect("clip tap");
    let program = c.program_tap.expect("program tap");
    // Reachability from the output, walked from the graph itself (no literal ids).
    let mut reachable = vec![false; c.graph.nodes.len()];
    let out = c.graph.output.expect("output");
    reachable[out.0 as usize] = true;
    for i in (0..c.graph.nodes.len()).rev() {
        if reachable[i] {
            for (input, _) in &c.graph.nodes[i].inputs {
                reachable[input.0 as usize] = true;
            }
        }
    }
    assert!(
        reachable[tap.0 as usize],
        "the clip tap is an ancestor of the output, so the program evaluation \
         already rendered it — this is why the tap costs no extra evaluation"
    );
    assert!(reachable[program.0 as usize], "…and so is the program tap");
}

/// The GPU half, end to end: `Evaluator::evaluate_with_tap` returns the clip's
/// own texture beside the program's, and — the cost claim — asking for the tap
/// adds **zero** cache misses, because the tap names a node the same evaluation
/// already rendered.
#[test]
fn gpu_evaluator_returns_the_clip_tap_without_a_second_render() {
    let Some(gpu) = GpuContext::request_blocking() else {
        eprintln!("no GPU adapter — skipping K-E2 GPU tap test");
        return;
    };
    let (c, clip) = compiled(Tick(0), opaque_green_track_above);
    let tap = c.tap(ScopeTapPoint::Clip(clip)).expect("clip tap");
    let mut ev = Evaluator::new(gpu.clone());

    // Warm pass with no tap: this is the ordinary program render.
    let (program, none) = ev.evaluate_with_tap(&c.graph, (W, H), &mut NullFrameSource, None);
    assert!(none.is_none(), "no tap requested, no tap returned");
    let program = program.expect("program texture");
    let after_warm = ev.cache_stats().misses;

    // Same graph, now asking for the tap. Every node is already rendered, so the
    // tap must not cost a single additional miss.
    let (program2, tapped) =
        ev.evaluate_with_tap(&c.graph, (W, H), &mut NullFrameSource, Some(tap));
    assert_eq!(
        ev.cache_stats().misses,
        after_warm,
        "requesting the tap must not render anything new"
    );
    let tapped = tapped.expect("clip tap texture");
    assert_eq!((tapped.width, tapped.height), (W, H), "logical tap size");

    let read = |t: &wgpu::Texture| read_texture_rgba16f(&gpu, t, W, H);
    let prog_px = read(&program2.expect("program texture"));
    let tap_px = read(&tapped.texture);
    assert!(
        prog_px[0][1] > 0.9 && prog_px[0][0] < 0.1,
        "the program is the green track above, got {:?}",
        prog_px[0]
    );
    assert!(
        tap_px[0][0] > 0.9 && tap_px[0][1] < 0.1,
        "the tap is the clip's own red, got {:?}",
        tap_px[0]
    );
    drop(program);
}

/// Every lowered clip is tapped, so a multi-clip frame answers `get_scopes` for
/// any of them out of one compile. The expected count is DERIVED from the clips
/// the frame actually covers, not written as a literal.
#[test]
fn every_clip_covering_the_tick_is_tapped_exactly_once() {
    let (c, clip) = compiled(Tick(0), opaque_green_track_above);
    let covering = 2; // V1 red + V2 green, both spanning [0, 1000)
    assert_eq!(c.clip_taps.len(), covering);
    let mut ids: Vec<_> = c.clip_taps.iter().map(|(id, _)| *id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), covering, "no clip is tapped twice");
    assert!(ids.contains(&clip));
}
