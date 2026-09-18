//! Acceptance test 8 for 39 §2.2 — forward-compatible unknown enum variants.
//!
//! Simulates a `.photon` file written by a *newer* build that carries variants
//! this build does not understand, across all six open-ended persisted enums
//! (`ClipSource`, `EffectKind`, `TransitionKind`, `GradeOpKind`/`GradeOpParams`,
//! `AudioFxKind`, `GraphOp`). The contract (39 §2.2):
//!   1. the document LOADS (a newer grade-op kind used to hard-fail the whole
//!      `Document::from_json` before this section — that is the regression
//!      anchor here);
//!   2. each unknown is diagnosed exactly once per load (`LoadReport`);
//!   3. every unknown subtree is re-emitted verbatim (preserve, never drop);
//!   4. load → save → load → save is idempotent;
//!   5. a KNOWN variant with a malformed field is NOT silently rewritten to a
//!      known one.
//!
//! The KNOWN scaffold is built from Rust types (so the shape is always a valid
//! `format_version` = [`CURRENT_FORMAT_VERSION`] document); the UNKNOWN parts
//! are injected as raw JSON, because this build has no Rust variant that could
//! construct them — exactly the situation a real forward-compat load faces.
//!
//! On object key ORDER: `serde_json::Map` re-emits keys sorted unless the
//! `preserve_order` feature is enabled, and `Document::to_json` normalizes number
//! formatting, so true whole-file byte identity is unattainable. "Re-saves
//! byte-identically for the unknown parts" (acceptance item 8) is therefore
//! evaluated as `serde_json::Value`-equality of each unknown subtree, which is
//! the semantically-lossless reading.

use photonic_core::document::CURRENT_FORMAT_VERSION;
use photonic_core::timeline::*;
use photonic_core::Document;
use serde_json::{json, Value};

// The six unknown tags injected below, one per open-ended enum.
const UNKNOWN_SOURCE: &str = "holo_gen";
const UNKNOWN_EFFECT: &str = "film_look";
const UNKNOWN_TRANSITION: &str = "iris_wipe";
const UNKNOWN_GRADE: &str = "bloom";
const UNKNOWN_AUDIOFX: &str = "exciter";
const UNKNOWN_GRAPHOP: &str = "caustics";

/// A valid current-`format_version` document with one video clip (source + effect +
/// transition + grade op), one audio track with a track-fx unit, and a project
/// graph with one node — every discriminant a KNOWN variant.
fn base_doc() -> Document {
    let mut project = TimelineProject::new();
    let mut sequence = Sequence::new("Seq 1", FrameRate::FPS_30, 1920, 1080);

    let mut vtrack = Track::new(TrackKind::Video, "V1");
    let mut c1 = Clip::new(
        ClipSource::SolidColor {
            color: photonic_core::Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        },
        Tick(0),
        Tick(1000),
    );
    c1.effects.push(ClipEffect::new(EffectKind::Invert));
    c1.transition_in = Some(Transition::new(TransitionKind::CrossDissolve, Tick(200)));
    c1.grade = Some(Grade {
        ops: vec![GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 0.5 },
        )],
        bypass: false,
    });
    vtrack.clips.push(c1);

    let mut atrack = Track::new(TrackKind::Audio, "A1");
    let mut ta = TrackAudio::new();
    ta.fx_chain.push(AudioFxUnit::new(AudioFxKind::Eq));
    atrack.audio = Some(ta);
    atrack
        .clips
        .push(Clip::new(ClipSource::Adjustment, Tick(0), Tick(2000)));

    sequence.video_tracks.push(vtrack);
    sequence.audio_tracks.push(atrack);
    project.insert_sequence(sequence);

    let mut graph = NodeGraph::new_project_graph("G1");
    let node = GraphNode::new(GraphOp::Invert);
    graph.nodes.insert(node.id, node);
    project.graphs.insert(graph.id, graph);

    let mut doc = Document::new("t", 100.0, 100.0);
    doc.timeline = Some(project);
    doc
}

/// The verbatim payloads we inject, keyed for later value-equality checks.
fn unknown_source() -> Value {
    json!({ "source": UNKNOWN_SOURCE, "seed": 7, "nested": { "a": [1, 2] } })
}
fn unknown_graph_op() -> Value {
    json!({ "op": UNKNOWN_GRAPHOP, "strength": 2.5 })
}
fn unknown_grade_base() -> Value {
    json!({ "kind": UNKNOWN_GRADE, "threshold": 0.8 })
}

/// The only sequence in the timeline (mutable view into a `Value` tree).
fn only_sequence(doc: &mut Value) -> &mut Value {
    doc["timeline"]["sequences"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap()
}

/// Serialize `base_doc`, then overwrite the six known discriminants with
/// unknown ones — simulating a file from a newer build.
fn newer_build_json() -> String {
    let mut v: Value = serde_json::from_str(&base_doc().to_json().unwrap()).unwrap();

    {
        let seq = only_sequence(&mut v);
        let clip = &mut seq["video_tracks"][0]["clips"][0];
        clip["source"] = unknown_source();
        clip["effects"][0]["kind"] = json!(UNKNOWN_EFFECT);
        clip["transition_in"]["kind"] = json!(UNKNOWN_TRANSITION);
        clip["grade"]["ops"][0]["kind"] = json!(UNKNOWN_GRADE);
        clip["grade"]["ops"][0]["params"]["base"] = unknown_grade_base();
        seq["audio_tracks"][0]["audio"]["fx_chain"][0]["kind"] = json!(UNKNOWN_AUDIOFX);
    }
    // The one non-Output graph node → an unknown op.
    {
        let graph = v["timeline"]["graphs"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        for node in graph["nodes"].as_object_mut().unwrap().values_mut() {
            if node["op"]["op"] == json!("invert") {
                node["op"] = unknown_graph_op();
            }
        }
    }
    serde_json::to_string_pretty(&v).unwrap()
}

/// Re-fetch the six unknown subtrees from a serialized document, by path.
fn extract_unknown_subtrees(json: &str) -> Vec<(&'static str, Value)> {
    let mut v: Value = serde_json::from_str(json).unwrap();
    let mut out = Vec::new();
    {
        let seq = only_sequence(&mut v);
        let clip = &seq["video_tracks"][0]["clips"][0];
        out.push(("source", clip["source"].clone()));
        out.push((
            "grade_base",
            clip["grade"]["ops"][0]["params"]["base"].clone(),
        ));
        out.push((
            "audiofx_kind",
            seq["audio_tracks"][0]["audio"]["fx_chain"][0]["kind"].clone(),
        ));
    }
    {
        let graph = v["timeline"]["graphs"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap();
        for node in graph["nodes"].as_object().unwrap().values() {
            if node["op"].get("op") == Some(&json!(UNKNOWN_GRAPHOP)) {
                out.push(("graph_op", node["op"].clone()));
            }
        }
    }
    out
}

#[test]
fn newer_build_document_loads_and_preserves_all_unknown_variants() {
    let json = newer_build_json();

    // (1) It LOADS. Before this section, the unknown grade-op `kind` alone made
    // the whole `from_json` fail with "unknown variant `bloom`". That is the
    // regression this asserts against.
    let doc = Document::from_json(&json).expect("newer-build document must load");
    assert_eq!(doc.format_version, CURRENT_FORMAT_VERSION);

    // (2) Exactly six unknowns, diagnosed once each, with verbatim tags.
    let (_doc, report) =
        Document::from_value_with_report(serde_json::from_str(&json).unwrap()).unwrap();
    let mut got: Vec<(&str, &str)> = report
        .unknown_variants
        .iter()
        .map(|u| (u.enum_name, u.tag.as_str()))
        .collect();
    got.sort();
    let mut want = vec![
        ("AudioFxKind", UNKNOWN_AUDIOFX),
        ("ClipSource", UNKNOWN_SOURCE),
        ("EffectKind", UNKNOWN_EFFECT),
        ("GradeOpKind", UNKNOWN_GRADE),
        ("GraphOp", UNKNOWN_GRAPHOP),
        ("TransitionKind", UNKNOWN_TRANSITION),
    ];
    want.sort();
    assert_eq!(
        got, want,
        "must report all six unknown variants exactly once"
    );
    for u in &report.unknown_variants {
        assert_eq!(u.count, 1, "each unknown appears once in this document");
    }

    // (3) Every unknown subtree re-emits value-equal to the injected payload.
    let saved = doc.to_json().unwrap();
    let before = extract_unknown_subtrees(&json);
    let after = extract_unknown_subtrees(&saved);
    assert_eq!(after, before, "unknown subtrees must round-trip verbatim");
    // Spot-check the deepest nested unknown payload survives intact.
    let src_after = &after.iter().find(|(k, _)| *k == "source").unwrap().1;
    assert_eq!(src_after["nested"]["a"], json!([1, 2]));
    assert_eq!(src_after["seed"], json!(7));

    // (4) Idempotence: load → save → load → save is a fixed point.
    let saved2 = Document::from_json(&saved).unwrap().to_json().unwrap();
    let v1: Value = serde_json::from_str(&saved).unwrap();
    let v2: Value = serde_json::from_str(&saved2).unwrap();
    assert_eq!(v1, v2, "second save must equal the first");
}

#[test]
fn known_variant_with_malformed_field_is_rejected_not_swallowed() {
    // (5) Negative control. serde's per-variant `#[serde(untagged)]` fallback is
    // greedy: a KNOWN `source` tag whose payload is malformed degrades to
    // `ClipSource::Unknown` at the serde layer rather than erroring (pinned in
    // `clip::tests`). The document-level integrity guard in `load::finalize_load`
    // (`KNOWN_CLIP_SOURCE_TAGS`) catches exactly this — a retained `Unknown`
    // whose tag is a KNOWN catalog tag is corrupt data, not a forward-compat
    // variant — so `Document::from_json` REJECTS it. 39 §2.2 rule 4: never guess,
    // and never silently accept corruption as an inert unknown.
    let mut v: Value = serde_json::from_str(&base_doc().to_json().unwrap()).unwrap();
    {
        let seq = only_sequence(&mut v);
        // `solid_color` is a KNOWN tag; `color` must be an RGBA object.
        seq["video_tracks"][0]["clips"][0]["source"] =
            json!({ "source": "solid_color", "color": "NOT-A-COLOR" });
    }
    let json = serde_json::to_string(&v).unwrap();
    let err = Document::from_json(&json)
        .expect_err("a malformed KNOWN source must be rejected, not loaded inert");
    assert!(
        err.to_string().contains("solid_color"),
        "the load error must name the corrupt tag: {err}"
    );

    // The serde layer alone still degrades it to Unknown-with-known-tag (this is
    // the exact input the load guard rejects) — never a *different* known
    // variant.
    let src: ClipSource =
        serde_json::from_value(json!({ "source": "solid_color", "color": "NOT-A-COLOR" })).unwrap();
    assert!(
        src.is_unknown(),
        "serde layer degrades a malformed known source to Unknown"
    );
    assert_eq!(src.unknown_tag(), Some("solid_color"));
    assert!(!matches!(src, ClipSource::SolidColor { .. }));
}

#[test]
fn unknown_source_clip_survives_a_move_edit() {
    // The unknown parts must survive a `finalize_load` validation pass and an
    // `ops.rs` edit (move the unknown-source clip, then save) unchanged — an
    // unknown-source clip is an ordinary opaque clip to the edit ops.
    let json = newer_build_json();
    let mut doc = Document::from_json(&json).unwrap();

    let (seq_id, track_id, clip_id, orig_start) = {
        let tl = doc.timeline.as_ref().unwrap();
        let (sid, seq) = tl.sequences.iter().next().unwrap();
        let track = &seq.video_tracks[0];
        let clip = &track.clips[0];
        (*sid, track.id, clip.id, clip.start)
    };

    // Move the unknown-source clip forward; the op treats it like any clip.
    let tl = doc.timeline.as_ref().unwrap();
    let cmd = ops::move_clip(tl, seq_id, track_id, clip_id, Tick(500))
        .expect("moving an unknown-source clip must be a normal edit");
    use photonic_core::history::Command;
    Command::Timeline(cmd).apply(&mut doc);

    let moved_start =
        doc.timeline.as_ref().unwrap().sequences[&seq_id].video_tracks[0].clips[0].start;
    assert_ne!(moved_start, orig_start, "the edit actually moved the clip");

    // Save; the unknown source subtree is byte-for-value identical to the input.
    let saved = doc.to_json().unwrap();
    let src_after = extract_unknown_subtrees(&saved)
        .into_iter()
        .find(|(k, _)| *k == "source")
        .unwrap()
        .1;
    assert_eq!(
        src_after,
        unknown_source(),
        "the edit must not touch the unknown source"
    );
}
