//! Forward-compat inert lowering (39 §2.2 rule 2 + rule 3).
//!
//! A document written by a *newer* build can carry enum variants this build does
//! not understand (see the `photonic-core` `forward_compat` acceptance test for
//! the load/preserve/round-trip contract). This test pins the *render* side: when
//! such an unknown variant reaches the compiler it must lower **inertly** — never
//! guessing a similar known variant (rule 4) — and, where a diagnostic channel
//! exists, name itself exactly once:
//!   * an unknown `ClipSource` → a transparent placeholder frame (same inert
//!     path as a missing/offline asset);
//!   * an unknown clip `EffectKind` → an `IrOp::Effect` marker the evaluator
//!     passes through (no param UI, no substitution);
//!   * an unknown `TransitionKind` → a HARD CUT (the incoming clip directly, no
//!     blend), never a guessed cross-dissolve;
//!   * an unknown `GraphOp` → passthrough of its primary input.
//!
//! The unknown variants are constructed directly here; on a real load they arrive
//! from the serde `Unknown` fallback with the original tag/payload retained.

use photonic_core::timeline::*;
use photonic_core::Color;
use photonic_video::graph::compile::{compile, Quality};
use photonic_video::graph::ir::IrOp;

const UNKNOWN_SOURCE: &str = "holo_gen";
const UNKNOWN_EFFECT: &str = "film_look";
const UNKNOWN_TRANSITION: &str = "iris_wipe";
const UNKNOWN_GRAPHOP: &str = "caustics";

fn base_project() -> (TimelineProject, SequenceId) {
    let mut project = TimelineProject::new();
    let seq = Sequence::new("seq", FrameRate::FPS_30, 320, 180);
    let id = seq.id;
    project.insert_sequence(seq);
    (project, id)
}

fn add_video_track(project: &mut TimelineProject, seq_id: SequenceId) -> usize {
    let seq = project.sequences.get_mut(&seq_id).unwrap();
    seq.video_tracks.push(Track::new(TrackKind::Video, "V1"));
    seq.video_tracks.len() - 1
}

fn solid(color: Color, start: i64, dur: i64) -> Clip {
    Clip::new(ClipSource::SolidColor { color }, Tick(start), Tick(dur))
}

/// An `Unknown` source/graph-op object carrying a verbatim tag + payload — the
/// shape a serde untagged fallback produces on load.
fn unknown_object(tag_key: &str, tag: &str) -> serde_json::Map<String, serde_json::Value> {
    serde_json::json!({ tag_key: tag, "strength": 2.5, "nested": { "a": [1, 2] } })
        .as_object()
        .unwrap()
        .clone()
}

#[test]
fn unknown_clip_source_lowers_to_transparent_placeholder() {
    let (mut project, seq_id) = base_project();
    let tk = add_video_track(&mut project, seq_id);
    project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
        .clips
        .push(Clip::new(
            ClipSource::Unknown(unknown_object("source", UNKNOWN_SOURCE)),
            Tick(0),
            Tick(1000),
        ));

    let out = compile(&project, seq_id, 0, Tick(0), Quality::PREVIEW, None);

    // Placeholder = a fully transparent SolidColor (alpha 0) — the exact node
    // `Builder::transparent` emits for a missing/offline asset. A normal opaque
    // clip never produces a transparent source, so this is unambiguous.
    let has_placeholder = out
        .graph
        .nodes
        .iter()
        .any(|n| matches!(&n.op, IrOp::SolidColor { color } if color.a == 0.0));
    assert!(
        has_placeholder,
        "unknown source must render a transparent placeholder"
    );

    // The diagnostic names the verbatim tag exactly once — never guessed away.
    let hits: Vec<_> = out
        .diagnostics
        .iter()
        .filter(|d| d.message.contains("placeholder") && d.message.contains(UNKNOWN_SOURCE))
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "one placeholder diagnostic naming the tag, got {:?}",
        out.diagnostics
    );
}

#[test]
fn unknown_clip_effect_lowers_to_an_inert_effect_marker() {
    let (mut project, seq_id) = base_project();
    let tk = add_video_track(&mut project, seq_id);
    let mut clip = solid(Color::BLACK, 0, 1000);
    clip.effects
        .push(ClipEffect::new(EffectKind::Unknown(UnknownTag::intern(
            UNKNOWN_EFFECT,
        ))));
    project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
        .clips
        .push(clip);

    let out = compile(&project, seq_id, 0, Tick(0), Quality::PREVIEW, None);

    // The effect stays in the chain as an `Effect` node whose kind is the
    // preserved Unknown — the evaluator passes every non-`Invert` effect through,
    // so it renders inert. It is NOT substituted for a known effect.
    let unknown_effect = out.graph.nodes.iter().find_map(|n| match &n.op {
        IrOp::Effect { kind, .. } => Some(*kind),
        _ => None,
    });
    let kind = unknown_effect.expect("an Effect node is present for the unknown effect");
    assert!(
        kind.is_unknown(),
        "the effect kind is the preserved Unknown, not a guess"
    );
    assert_eq!(kind.unknown_tag().unwrap().as_str(), UNKNOWN_EFFECT);
}

#[test]
fn unknown_transition_renders_as_a_hard_cut() {
    let (mut project, seq_id) = base_project();
    let tk = add_video_track(&mut project, seq_id);
    let clips = &mut project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk].clips;
    // Two adjacent clips; the second transitions IN from the first.
    clips.push(solid(Color::BLACK, 0, 100));
    let mut incoming = solid(
        Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        },
        100,
        100,
    );
    incoming.transition_in = Some(Transition::new(
        TransitionKind::Unknown(UnknownTag::intern(UNKNOWN_TRANSITION)),
        Tick(40),
    ));
    clips.push(incoming);

    // t=120 sits inside the transition window [100, 140).
    let out = compile(&project, seq_id, 0, Tick(120), Quality::FULL, None);

    // The unknown transition arm names itself as a hard cut — never the "renders
    // as a cross-dissolve" fallback used for the known-but-unimplemented
    // Wipe/Push kinds.
    let cut = out
        .diagnostics
        .iter()
        .filter(|d| d.message.contains("hard cut"))
        .count();
    assert_eq!(cut, 1, "one hard-cut diagnostic, got {:?}", out.diagnostics);
    assert!(
        !out.diagnostics
            .iter()
            .any(|d| d.message.contains("cross-dissolve")),
        "an unknown transition must not be guessed into a dissolve",
    );

    // The cut shows the INCOMING clip directly (white), not a blended midpoint.
    let img = photonic_video::graph::eval_cpu::evaluate(
        &out.graph,
        (4, 4),
        &mut photonic_video::graph::eval_cpu::EmptyProvider,
    );
    for p in &img.pixels {
        for (c, &v) in p[..3].iter().enumerate() {
            assert!(
                (v - 1.0).abs() < 1e-4,
                "hard cut shows incoming white, channel {c} = {v}"
            );
        }
    }
}

#[test]
fn unknown_graph_op_lowers_to_passthrough_of_its_primary_input() {
    let (mut project, seq_id) = base_project();
    let tk = add_video_track(&mut project, seq_id);

    // Composition graph: ClipIn → Unknown → Output. The unknown op passes its
    // primary input (the host clip's source, bound via ClipIn) straight through.
    let clip_in = GraphNode::new(GraphOp::ClipIn);
    let unknown = GraphNode::new(GraphOp::Unknown(unknown_object("op", UNKNOWN_GRAPHOP)));
    let output = GraphNode::new(GraphOp::Output);
    let (ci, un, ou) = (clip_in.id, unknown.id, output.id);
    let mut nodes = std::collections::HashMap::new();
    for n in [clip_in, unknown, output] {
        nodes.insert(n.id, n);
    }
    let graph = NodeGraph {
        id: GraphId::new(),
        name: "comp".into(),
        nodes,
        edges: vec![
            GraphEdge {
                from: (ci, OutPort::PRIMARY),
                to: (un, InPort::PRIMARY),
            },
            GraphEdge {
                from: (un, OutPort::PRIMARY),
                to: (ou, InPort::PRIMARY),
            },
        ],
        output: ou,
        ui: std::collections::HashMap::new(),
    };
    let gid = graph.id;
    project.graphs.insert(gid, graph);

    let mut clip = solid(
        Color {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        },
        0,
        1000,
    );
    clip.composition = Some(gid);
    project.sequences.get_mut(&seq_id).unwrap().video_tracks[tk]
        .clips
        .push(clip);

    let out = compile(&project, seq_id, 0, Tick(0), Quality::PREVIEW, None);

    // The unknown op names itself as passthrough, anchored to its graph node so
    // the node editor can badge the exact node (39 §2.2 rule 3).
    let passthrough = out
        .diagnostics
        .iter()
        .filter(|d| d.message.contains("passthrough") && d.node == Some(un))
        .count();
    assert_eq!(
        passthrough, 1,
        "one node-anchored passthrough diagnostic, got {:?}",
        out.diagnostics
    );

    // Passthrough means the clip's own source survives to the Output — no Effect
    // node was fabricated for the unknown op (it was NOT guessed into a filter).
    assert!(
        out.graph
            .nodes
            .iter()
            .any(|n| matches!(&n.op, IrOp::SolidColor { color } if color.r > 0.5)),
        "the host clip's source passes through the unknown op unchanged",
    );
    assert!(
        !out.graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, IrOp::Effect { .. })),
        "an unknown graph op must not be lowered to a guessed Effect pass",
    );
    assert!(
        out.graph.output.is_some(),
        "the composition still produces an Output"
    );
}
