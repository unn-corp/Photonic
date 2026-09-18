//! Effect-scope application (35 §2.4) on the CPU reference evaluator — the
//! deferred scope-render coverage for task 35 (K-0.2 brief). These lock, at the
//! pixel level, that the four normative effect scopes actually reach the
//! evaluator and that the Adjustment special-case is honoured:
//!
//! - a **track**-level effect changes the track's own composited pixels;
//! - a **sequence master** effect changes the folded program's pixels;
//! - a track effect on an **Adjustment** track is IGNORED (an adjustment carries
//!   no own content, so the track stack does not apply — 35 §2.4(b) / the
//!   `fold_sequence` re-root path), while the adjustment's own **clip** stack
//!   DOES re-colour the accumulator below it.
//!
//! `Invert` is used throughout because it is a real CPU kernel (08 §3,
//! `ops::invert`) with an unmistakable, deterministic pixel change (straight
//! `1 − rgb`), so "did this scope run?" reduces to "did the pixel move?".

use photonic_core::timeline::{
    Clip, ClipEffect, ClipSource, EffectKind, FrameRate, Sequence, SequenceId, Tick,
    TimelineProject, Track, TrackKind,
};
use photonic_core::Color;
use photonic_video::graph::eval_cpu::{evaluate, EmptyProvider};
use photonic_video::graph::ops::Image;
use photonic_video::graph::{compile, Quality};

const W: u32 = 4;
const H: u32 = 4;
const RED: Color = Color {
    r: 1.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

/// A one-track red-solid sequence; `decorate` customises the project (track /
/// master / adjustment scopes) before compilation.
fn render(decorate: impl FnOnce(&mut Sequence)) -> Image {
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("scope", FrameRate::FPS_30, W, H);
    let seq_id: SequenceId = seq.id;
    let mut v1 = Track::new(TrackKind::Video, "V1");
    v1.clips.push(Clip::new(
        ClipSource::SolidColor { color: RED },
        Tick(0),
        Tick(1000),
    ));
    seq.video_tracks.push(v1);
    decorate(&mut seq);
    project.insert_sequence(seq);
    let compiled = compile(&project, seq_id, 0, Tick(0), Quality::FULL, None);
    assert!(
        compiled.diagnostics.is_empty(),
        "unexpected compile diagnostics: {:?}",
        compiled.diagnostics
    );
    evaluate(&compiled.graph, (W, H), &mut EmptyProvider)
}

fn pixels_differ(a: &Image, b: &Image) -> bool {
    a.pixels
        .iter()
        .zip(&b.pixels)
        .any(|(p, q)| (0..4).any(|k| (p[k] - q[k]).abs() > 1e-4))
}

/// A red solid renders premultiplied-linear red; the baseline others compare to.
fn is_red(img: &Image) -> bool {
    img.pixels
        .iter()
        .all(|p| (p[0] - 1.0).abs() < 1e-4 && p[1].abs() < 1e-4 && p[2].abs() < 1e-4)
}

/// 35 §2.4(a): a track-level effect acts on that track's own content, so it
/// moves the rendered pixels relative to the un-decorated baseline.
#[test]
fn track_effect_changes_pixels() {
    let baseline = render(|_| {});
    assert!(is_red(&baseline), "baseline is the plain red solid");
    let with_track_fx = render(|seq| {
        seq.video_tracks[0]
            .effects
            .push(ClipEffect::new(EffectKind::Invert));
    });
    assert!(
        pixels_differ(&baseline, &with_track_fx),
        "a track Invert must change the composited pixels"
    );
    assert!(
        !is_red(&with_track_fx),
        "the track-inverted frame is no longer red"
    );
}

/// 35 §2.4(d): a sequence master effect runs on the folded program, so it too
/// moves the rendered pixels relative to the baseline.
#[test]
fn master_effect_changes_pixels() {
    let baseline = render(|_| {});
    let with_master_fx = render(|seq| {
        seq.master_effects.push(ClipEffect::new(EffectKind::Invert));
    });
    assert!(
        pixels_differ(&baseline, &with_master_fx),
        "a master Invert must change the folded program's pixels"
    );
}

/// 35 §2.4(b): an Adjustment clip carries no own content, so the TRACK stack of
/// the adjustment track does NOT apply — a track effect there is inert. But the
/// adjustment's own CLIP stack re-roots (re-colours) the accumulator below. This
/// distinguishes "track stack correctly skipped" from "adjustment does nothing".
#[test]
fn adjustment_track_skips_its_track_stack_but_clip_stack_applies() {
    // Case A: an Adjustment clip on V2 with no effects at all → passes the red
    // fold below through unchanged.
    let plain = render(|seq| {
        let mut v2 = Track::new(TrackKind::Video, "V2");
        v2.clips
            .push(Clip::new(ClipSource::Adjustment, Tick(0), Tick(1000)));
        seq.video_tracks.push(v2);
    });
    assert!(
        is_red(&plain),
        "an effect-free Adjustment leaves the composite (red) untouched"
    );

    // Case B: the SAME Adjustment track now carries a TRACK-level effect. Per the
    // re-root rule the track stack is skipped for an adjustment, so the output is
    // identical to Case A (the Invert never runs).
    let track_fx_on_adjustment = render(|seq| {
        let mut v2 = Track::new(TrackKind::Video, "V2");
        v2.effects.push(ClipEffect::new(EffectKind::Invert));
        v2.clips
            .push(Clip::new(ClipSource::Adjustment, Tick(0), Tick(1000)));
        seq.video_tracks.push(v2);
    });
    assert!(
        !pixels_differ(&plain, &track_fx_on_adjustment),
        "a TRACK effect on an Adjustment track must NOT affect the accumulator"
    );

    // Case C: moving the SAME effect onto the adjustment's CLIP stack DOES re-root
    // the accumulator — proving the adjustment scope itself is wired, and that
    // Case B's no-op is the skipped track stack, not an inert adjustment.
    let clip_fx_on_adjustment = render(|seq| {
        let mut v2 = Track::new(TrackKind::Video, "V2");
        let mut adj = Clip::new(ClipSource::Adjustment, Tick(0), Tick(1000));
        adj.effects.push(ClipEffect::new(EffectKind::Invert));
        v2.clips.push(adj);
        seq.video_tracks.push(v2);
    });
    assert!(
        pixels_differ(&plain, &clip_fx_on_adjustment),
        "an Adjustment CLIP effect must re-colour the accumulator below it"
    );
    assert!(
        !is_red(&clip_fx_on_adjustment),
        "the adjustment-inverted frame is no longer red"
    );
}
