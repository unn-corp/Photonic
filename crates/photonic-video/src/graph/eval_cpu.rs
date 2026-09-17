//! CPU reference evaluator (02 §2 "Evaluation": `eval_cpu`).
//!
//! An f32 implementation of the P3 `IrOp` subset, used as the ground truth for
//! golden tests and GPU parity (03 §6, §4.4). Determinism is exact: the same
//! [`FrameGraph`] and the same [`FrameProvider`] outputs produce a byte-identical
//! f32 image (02 §2's normative property). The evaluator is time-ignorant — every
//! keyframe was already resolved by the compiler.
//!
//! Source ops (`DecodeVideo`/`DecodeStill`/`RasterVector`) are delegated to a
//! [`FrameProvider`] so tests can inject known pixels without a real decoder; the
//! GPU evaluator (`eval.rs`) resolves the same ops against decode rings and the
//! headless vector renderer.
//!
//! Real CPU kernels now cover `Grade` (`photonic_render::grade::apply_grade_cpu`,
//! the GPU-parity golden per 03 §4.4) and `Effect{Invert}` (`ops::invert`). The
//! remaining `Effect` kinds (Blur/Sharpen/Glow/ChromaKey/LumaKey/MaskShapeGen),
//! `MatteExtract`, and `ChannelSplit`/`Combine` stay input-passthrough with the
//! phase noted, exactly as 02 §2 permits — blocked on the still-opaque
//! `ResolvedParams` payload (`contract.rs`), which finalizes in P5/P7.
//!
//! `CaptionOverlay` (06 §5.3) and `TextGen` (G-12 title clips) both carry a fully
//! resolved glyph payload but are **GPU-only** composites here: glyph
//! rasterization is glyphon's (`eval.rs`), and `photonic_render`'s text path
//! exposes no CPU glyph raster to share, so the CPU reference passes the input
//! through (captions) / emits transparent (`TextGen`) and burned text comes only
//! from the GPU path. Caption and title tracks are therefore excluded from
//! GPU/CPU byte-parity in v1 (documented tolerance, per this story's brief); a
//! shared CPU glyph rasterizer for full export-determinism parity is a follow-up.

use photonic_core::layer::BlendMode;
use photonic_core::timeline::EffectKind;

use crate::contract::{AssetId, Tick, VectorRef, VectorStateKey};
use crate::graph::ir::{FitMode, FrameGraph, IrOp};
use crate::graph::ops::{self, Image};

/// Supplies source-op pixels to the CPU evaluator (02 §2's `eval_cpu` source
/// hook). All images are premultiplied linear Rec.709 (D-09). `w`/`h` are the
/// canvas hint (the output format); a provider may return that size or a native
/// size the evaluator resamples downstream.
pub trait FrameProvider {
    fn decode_video(
        &mut self,
        asset: AssetId,
        src_time: Tick,
        proxy: bool,
        w: u32,
        h: u32,
    ) -> Image;
    fn decode_still(&mut self, asset: AssetId, w: u32, h: u32) -> Image;
    fn raster_vector(&mut self, vref: VectorRef, key: VectorStateKey, w: u32, h: u32) -> Image;
}

/// A provider that yields transparent black for every source — enough to
/// exercise the SolidColor/Merge/Transform golden math with no real media.
pub struct EmptyProvider;

impl FrameProvider for EmptyProvider {
    fn decode_video(&mut self, _: AssetId, _: Tick, _: bool, w: u32, h: u32) -> Image {
        Image::new(w, h)
    }
    fn decode_still(&mut self, _: AssetId, w: u32, h: u32) -> Image {
        Image::new(w, h)
    }
    fn raster_vector(&mut self, _: VectorRef, _: VectorStateKey, w: u32, h: u32) -> Image {
        Image::new(w, h)
    }
}

/// Evaluate `graph` to its output [`Image`], sizing generators to `canvas`
/// (the active format's pixel dimensions). Nodes are already topologically
/// sorted (every input precedes its consumer), so one forward pass suffices.
pub fn evaluate(graph: &FrameGraph, canvas: (u32, u32), provider: &mut dyn FrameProvider) -> Image {
    evaluate_at(graph, canvas, provider, None)
}

/// Evaluate `graph` and return the image at `node` — the CPU mirror of
/// [`crate::graph::eval::Evaluator::evaluate_with_tap`], so a K-E2 scope tap can
/// be asserted pixel-for-pixel without a GPU adapter. `None` means "the graph's
/// output", i.e. exactly [`evaluate`]. A node id outside the graph degrades to a
/// transparent canvas rather than panicking, matching the output path.
pub fn evaluate_at(
    graph: &FrameGraph,
    canvas: (u32, u32),
    provider: &mut dyn FrameProvider,
    node: Option<crate::graph::ir::IrNodeId>,
) -> Image {
    let (cw, ch) = (canvas.0.max(1), canvas.1.max(1));
    let canvas_scale = graph.canvas_scale((cw, ch));
    let native = graph.native_video_sources();
    let mut results: Vec<Option<Image>> = (0..graph.nodes.len()).map(|_| None).collect();

    for (i, node) in graph.nodes.iter().enumerate() {
        let img = {
            let inputs: Vec<&Image> = node
                .inputs
                .iter()
                .map(|(id, _)| {
                    results[id.0 as usize]
                        .as_ref()
                        .expect("input evaluated before consumer (topo order)")
                })
                .collect();
            eval_op(
                &node.op.at_canvas_scale(canvas_scale),
                &inputs,
                cw,
                ch,
                provider,
                native[i],
            )
        };
        results[i] = Some(img);
    }

    match node.or(graph.output) {
        Some(out) => results
            .get_mut(out.0 as usize)
            .and_then(|slot| slot.take())
            .map(|image| {
                if native[out.0 as usize] {
                    normalize_source(image, cw, ch)
                } else {
                    image
                }
            })
            .unwrap_or_else(|| Image::new(cw, ch)),
        None => Image::new(cw, ch),
    }
}

fn eval_op(
    op: &IrOp,
    inputs: &[&Image],
    cw: u32,
    ch: u32,
    provider: &mut dyn FrameProvider,
    native_video: bool,
) -> Image {
    // Missing-input safety: the compiler always wires unary/binary ops, but be
    // defensive so a malformed graph degrades to transparent, never panics.
    let in0 = || {
        inputs
            .first()
            .map(|i| (*i).clone())
            .unwrap_or_else(|| Image::new(cw, ch))
    };

    match op {
        IrOp::DecodeVideo {
            asset,
            src_time,
            proxy,
        } => {
            let image = provider.decode_video(*asset, *src_time, *proxy, cw, ch);
            if native_video {
                image
            } else {
                normalize_source(image, cw, ch)
            }
        }
        IrOp::DecodeStill { asset } => {
            normalize_source(provider.decode_still(*asset, cw, ch), cw, ch)
        }
        IrOp::RasterVector {
            vref,
            doc_state,
            w,
            h,
        } => normalize_source(provider.raster_vector(*vref, *doc_state, *w, *h), cw, ch),
        IrOp::SolidColor { color } => ops::solid(cw, ch, *color),
        IrOp::Transform2D { mat, sampling } => match inputs.first() {
            Some(input) => ops::transform2d_to_canvas(input, *mat, *sampling, cw, ch),
            None => Image::new(cw, ch),
        },
        IrOp::StabilizeWarp { warp, sampling } => match inputs.first() {
            Some(input) => ops::stabilize_warp(input, warp, *sampling),
            None => Image::new(cw, ch),
        },
        // Real kernels for all seven v1 effects (08 §3 / K-0.2). An
        // unknown/forward-compat kind passes through (39 §2.2).
        IrOp::Effect { kind, params } => match kind {
            EffectKind::Invert => ops::invert(&in0()),
            EffectKind::Blur => ops::blur(&in0(), params.f32_or("params.radius", 0.0)),
            EffectKind::Sharpen => ops::sharpen(
                &in0(),
                params.f32_or("params.amount", 0.0),
                params.f32_or("params.radius", 0.0),
            ),
            EffectKind::Glow => {
                let tint = params.color_or("params.tint", photonic_core::Color::WHITE);
                ops::glow(
                    &in0(),
                    params.f32_or("params.radius", 0.0),
                    params.f32_or("params.threshold", 0.0),
                    params.f32_or("params.intensity", 0.0),
                    [
                        ops::srgb_to_linear(tint.r),
                        ops::srgb_to_linear(tint.g),
                        ops::srgb_to_linear(tint.b),
                    ],
                )
            }
            EffectKind::LumaKey => {
                let img = in0();
                ops::luma_key(
                    &img,
                    params.f32_or("params.threshold", 0.0),
                    params.f32_or("params.softness", 0.0),
                    params.bool_or("params.invert", false),
                )
            }
            EffectKind::ChromaKey => {
                let img = in0();
                let key = params.color_or("params.key_color", photonic_core::Color::BLACK);
                ops::chroma_key(
                    &img,
                    [
                        ops::srgb_to_linear(key.r),
                        ops::srgb_to_linear(key.g),
                        ops::srgb_to_linear(key.b),
                    ],
                    params.f32_or("params.tolerance", 0.0),
                    params.f32_or("params.edge_softness", 0.0),
                    params.f32_or("params.spill_suppress", 0.0),
                )
            }
            EffectKind::MaskShapeGen => ops::mask_shape(
                cw,
                ch,
                [
                    params.f32_or("params.center_x", 0.5),
                    params.f32_or("params.center_y", 0.5),
                ],
                [
                    params.f32_or("params.size_x", 0.5),
                    params.f32_or("params.size_y", 0.5),
                ],
                params.f32_or("params.rotation", 0.0),
                params.f32_or("params.feather", 0.0),
            ),
            // Deflicker applies a gain that a *whole-range* measurement produced
            // (32 §2's two-pass rule — see `graph::deflicker`). The evaluator
            // stays pure: it never reaches for the analysis cache, it just
            // consumes the resolved per-channel gain the compile pass wrote into
            // the params. Absent that gain the effect is an exact pass-through,
            // which is the correct behaviour before the measurement job has run.
            EffectKind::Deflicker => {
                let gain = [
                    params.f32_or("params.gain_r", 1.0),
                    params.f32_or("params.gain_g", 1.0),
                    params.f32_or("params.gain_b", 1.0),
                ];
                // Bands first, in LINEAR light: they are a physical modulation
                // of the light, so dividing them out there inverts the actual
                // degradation. The exposure gain then runs in the perceptual
                // domain (see `graph::deflicker`). The two deliberately differ.
                let band_amp = params.f32_or("params.band_amp", 0.0);
                let img = if band_amp.abs() > 1e-6 {
                    let model = crate::graph::rolling_bands::BandModel {
                        cycles: params.f32_or("params.band_cycles", 0.0).max(0.0) as u32,
                        amplitude: band_amp,
                        phase: params.f32_or("params.band_phase", 0.0),
                        confidence: 1.0,
                    };
                    crate::graph::rolling_bands::apply_correction(&in0(), &model, 1.0)
                } else {
                    in0()
                };
                crate::graph::deflicker::apply_gain(&img, gain)
            }
            EffectKind::Unknown(tag) => {
                // K-B16: unknown tags that name a raster-bridge id evaluate on
                // the CPU oracle; other unknowns stay inert (39 §2.2).
                crate::graph::raster_bridge::apply(tag.as_str(), &in0(), params).unwrap_or_else(in0)
            }
            // `#[non_exhaustive]`: any future kind is inert until a kernel lands.
            _ => in0(),
        },
        // Real kernel: the resolved grade stack (07 §3), the GPU-parity golden.
        IrOp::Grade { ops: grade_ops } => apply_grade_cpu_image(in0(), grade_ops),
        IrOp::CaptionOverlay { .. } => in0(), // GPU-only glyph composite (see header); CPU passes through
        IrOp::MatteExtract { .. } => in0(),   // P8 U²-Net inference
        IrOp::ChannelSplit { .. } => in0(),
        IrOp::ChannelCombine => in0(),
        IrOp::Deinterlace {
            method,
            field_order,
        } => ops::deinterlace(&in0(), *method, *field_order),
        IrOp::TextGen { .. } => Image::new(cw, ch), // GPU-only glyph composite (see header); CPU emits transparent
        IrOp::Merge { mode, opacity } => match (inputs.first(), inputs.get(1)) {
            (Some(top), Some(bottom)) => ops::merge(top, bottom, *mode, *opacity),
            (Some(top), None) => (*top).clone(),
            (None, Some(bottom)) => (*bottom).clone(),
            (None, None) => Image::new(cw, ch),
        },
        // Directional transitions (08 §2.0b): inputs are [incoming, outgoing].
        IrOp::WipeMix {
            direction,
            softness,
            t,
        } => match (inputs.first(), inputs.get(1)) {
            (Some(incoming), Some(outgoing)) => {
                ops::wipe(incoming, outgoing, *direction, *softness, *t)
            }
            (Some(only), None) | (None, Some(only)) => (*only).clone(),
            (None, None) => Image::new(cw, ch),
        },
        IrOp::PushMix { direction, t } => match (inputs.first(), inputs.get(1)) {
            (Some(incoming), Some(outgoing)) => ops::push(incoming, outgoing, *direction, *t),
            (Some(only), None) | (None, Some(only)) => (*only).clone(),
            (None, None) => Image::new(cw, ch),
        },
        IrOp::LumaWipeMix {
            kind,
            softness,
            invert,
            t,
        } => match (inputs.first(), inputs.get(1)) {
            (Some(incoming), Some(outgoing)) => {
                ops::luma_wipe(incoming, outgoing, *kind, *softness, *invert, *t)
            }
            (Some(only), None) | (None, Some(only)) => (*only).clone(),
            (None, None) => Image::new(cw, ch),
        },
        IrOp::Crop {
            left,
            top,
            right,
            bottom,
        } => match inputs.first() {
            Some(input) => ops::crop(input, cw, ch, *left, *top, *right, *bottom),
            None => Image::new(cw, ch),
        },
        IrOp::Resize { w, h, fit } => match inputs.first() {
            Some(input) => ops::resize(input, *w, *h, *fit),
            None => Image::new(*w, *h),
        },
        IrOp::Output { w, h } => match inputs.first() {
            Some(input) if input.width == *w && input.height == *h => (*input).clone(),
            Some(input) => ops::resize(input, *w, *h, FitMode::Stretch),
            None => Image::new(*w, *h),
        },
    }
}

fn normalize_source(image: Image, width: u32, height: u32) -> Image {
    if image.width == width && image.height == height {
        image
    } else {
        ops::resize(&image, width, height, FitMode::Stretch)
    }
}

/// Run the resolved grade stack over an [`Image`] via
/// [`photonic_render::grade::apply_grade_cpu`] — the golden CPU reference the GPU
/// grade pass must match within 1e-3 (03 §4.4). The `Image` is premultiplied
/// linear Rec.709 (D-09); the grade ops operate on the stored RGB directly and
/// preserve alpha, exactly matching the GPU pass which samples the same
/// premultiplied working texture. Empty stacks are an exact passthrough.
fn apply_grade_cpu_image(mut img: Image, ops: &[photonic_render::grade::ResolvedGradeOp]) -> Image {
    if ops.is_empty() {
        return img;
    }
    // Flatten to the contiguous `[r,g,b,a, …]` f32 buffer `apply_grade_cpu` wants.
    let mut buf: Vec<f32> = Vec::with_capacity(img.pixels.len() * 4);
    for p in &img.pixels {
        buf.extend_from_slice(p);
    }
    photonic_render::grade::apply_grade_cpu(&mut buf, img.width, img.height, ops);
    for (px, chunk) in img.pixels.iter_mut().zip(buf.chunks_exact(4)) {
        px.copy_from_slice(chunk);
    }
    img
}

/// The blend mode a `Merge` node applies (exposed for tests / callers that want
/// to reason about the graph without re-matching the op).
pub fn merge_mode(op: &IrOp) -> Option<BlendMode> {
    match op {
        IrOp::Merge { mode, .. } => Some(*mode),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::compile::{compile, Quality};
    use crate::graph::ir::LinearColor;
    use photonic_core::timeline::{
        Clip, ClipSource, FrameRate, Sequence, SequenceId, TimelineProject, Track, TrackKind,
    };
    use photonic_core::Color;

    fn project_with_two_solids(
        top: Color,
        bottom: Color,
        top_opacity: f64,
    ) -> (TimelineProject, SequenceId) {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("seq", FrameRate::FPS_30, 4, 4);
        let seq_id = seq.id;
        // bottom track (index 0), top track (index 1)
        let mut t0 = Track::new(TrackKind::Video, "V1");
        t0.clips.push(Clip::new(
            ClipSource::SolidColor { color: bottom },
            crate::contract::Tick(0),
            crate::contract::Tick::from_seconds(2),
        ));
        let mut t1 = Track::new(TrackKind::Video, "V2");
        let mut c = Clip::new(
            ClipSource::SolidColor { color: top },
            crate::contract::Tick(0),
            crate::contract::Tick::from_seconds(2),
        );
        c.transform.base.opacity = top_opacity;
        t1.clips.push(c);
        seq.video_tracks.push(t0);
        seq.video_tracks.push(t1);
        project.insert_sequence(seq);
        (project, seq_id)
    }

    /// SolidColor + Merge golden math: opaque top fully covers the backdrop.
    #[test]
    fn opaque_top_covers_backdrop() {
        // Colors are given in sRGB straight; the compiler converts to linear.
        let red = Color {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };
        let blue = Color {
            r: 0.0,
            g: 0.0,
            b: 1.0,
            a: 1.0,
        };
        let (project, seq_id) = project_with_two_solids(red, blue, 1.0);
        let out = compile(
            &project,
            seq_id,
            0,
            crate::contract::Tick(0),
            Quality::FULL,
            None,
        );
        let img = evaluate(&out.graph, (4, 4), &mut EmptyProvider);
        // Linear red premultiplied (alpha 1): srgb→linear(1.0) == 1.0, srgb→linear(0.0) == 0.
        for p in &img.pixels {
            assert!((p[0] - 1.0).abs() < 1e-4, "r={}", p[0]);
            assert!(p[1].abs() < 1e-4);
            assert!(p[2].abs() < 1e-4, "b={}", p[2]);
            assert!((p[3] - 1.0).abs() < 1e-4);
        }
    }

    /// Half-opacity top over an opaque backdrop is an exact 50/50 linear blend.
    #[test]
    fn half_opacity_blends_linearly() {
        // White over black at opacity 0.5. srgb→linear(1.0)=1, (0.0)=0.
        let white = Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        };
        let black = Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };
        let (project, seq_id) = project_with_two_solids(white, black, 0.5);
        let out = compile(
            &project,
            seq_id,
            0,
            crate::contract::Tick(0),
            Quality::FULL,
            None,
        );
        let img = evaluate(&out.graph, (4, 4), &mut EmptyProvider);
        for p in &img.pixels {
            #[allow(clippy::needless_range_loop)]
            for c in 0..3 {
                assert!((p[c] - 0.5).abs() < 1e-4, "channel {c} = {}", p[c]);
            }
            assert!((p[3] - 1.0).abs() < 1e-4);
        }
    }

    /// Determinism (02 §2): the same graph evaluates byte-identically twice.
    #[test]
    fn evaluation_is_byte_identical() {
        let c1 = Color {
            r: 0.2,
            g: 0.5,
            b: 0.9,
            a: 1.0,
        };
        let c2 = Color {
            r: 0.7,
            g: 0.1,
            b: 0.3,
            a: 1.0,
        };
        let (project, seq_id) = project_with_two_solids(c1, c2, 0.4);
        let out = compile(
            &project,
            seq_id,
            0,
            crate::contract::Tick(0),
            Quality::FULL,
            None,
        );
        let a = evaluate(&out.graph, (8, 8), &mut EmptyProvider);
        let b = evaluate(&out.graph, (8, 8), &mut EmptyProvider);
        assert_eq!(a.pixels, b.pixels);
    }

    // ── real Grade / Effect / transition kernels (this story) ─────────────────

    use photonic_core::timeline::{
        grade::{Grade, GradeOp, GradeOpKind, GradeOpParams},
        ClipEffect, EffectKind, GraphEdge, GraphId, GraphNode, GraphOp, InPort,
        OutPort as GOutPort, Transition, TransitionKind,
    };

    fn single_solid_project(color: Color) -> (TimelineProject, SequenceId) {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("seq", FrameRate::FPS_30, 4, 4);
        let seq_id = seq.id;
        let mut t = Track::new(TrackKind::Video, "V1");
        t.clips.push(Clip::new(
            ClipSource::SolidColor { color },
            crate::contract::Tick(0),
            crate::contract::Tick::from_seconds(2),
        ));
        seq.video_tracks.push(t);
        project.insert_sequence(seq);
        (project, seq_id)
    }

    /// The deflicker dispatch reaches the real kernel and consumes the gain the
    /// compile pass resolves, rather than silently passing through.
    #[test]
    fn deflicker_effect_applies_the_resolved_gain() {
        use crate::contract::ResolvedParams;
        use photonic_core::timeline::{PropPath, PropValue};

        let lin = crate::graph::ops::srgb_to_linear(0.4);
        let img = Image::filled(
            2,
            2,
            crate::graph::ir::LinearColor {
                r: lin,
                g: lin,
                b: lin,
                a: 1.0,
            },
        );

        let params = ResolvedParams {
            entries: vec![
                (PropPath::new("params.gain_r"), PropValue::Float(1.25)),
                (PropPath::new("params.gain_g"), PropValue::Float(1.25)),
                (PropPath::new("params.gain_b"), PropValue::Float(1.25)),
            ],
        };
        let op = IrOp::Effect {
            kind: EffectKind::Deflicker,
            params,
        };
        let out = eval_op(&op, &[&img], 2, 2, &mut EmptyProvider);
        let got = crate::graph::ops::linear_to_srgb(out.pixels[0][0]);
        assert!(
            (got - 0.5).abs() < 1e-3,
            "0.4 encoded x1.25 should be 0.5, got {got}"
        );

        // Before the measurement job has run there is no gain, and the effect
        // must then be an exact pass-through — not an approximate one.
        let inert = IrOp::Effect {
            kind: EffectKind::Deflicker,
            params: ResolvedParams::default(),
        };
        let passthrough = eval_op(&inert, &[&img], 2, 2, &mut EmptyProvider);
        assert_eq!(passthrough.pixels, img.pixels);
    }

    /// A clip grade of Exposure +1 stop doubles the linear working value — proving
    /// `IrOp::Grade` runs the real resolved stack, not a passthrough.
    #[test]
    fn grade_exposure_applies_non_passthrough() {
        // sRGB 0.5 → ~0.2140 linear; +1 stop (×2) → ~0.4280.
        let (mut project, seq_id) = single_solid_project(Color {
            r: 0.5,
            g: 0.5,
            b: 0.5,
            a: 1.0,
        });
        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        ));
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[0].clips[0].grade = Some(grade);

        let out = compile(
            &project,
            seq_id,
            0,
            crate::contract::Tick(0),
            Quality::FULL,
            None,
        );
        // The graph must contain a real Grade node with one resolved op.
        assert!(
            out.graph
                .nodes
                .iter()
                .any(|n| matches!(&n.op, IrOp::Grade { ops } if ops.len() == 1)),
            "a Grade IR op with one resolved op is present"
        );
        let img = evaluate(&out.graph, (4, 4), &mut EmptyProvider);
        let expected = {
            let lin = ((0.5f32 + 0.055) / 1.055).powf(2.4);
            lin * 2.0
        };
        for p in &img.pixels {
            assert!(
                (p[0] - expected).abs() < 1e-3,
                "graded r={} want {}",
                p[0],
                expected
            );
        }
    }

    /// A clip `Invert` effect maps opaque white → black — proving `Effect{Invert}`
    /// is a real kernel, not a passthrough.
    #[test]
    fn invert_effect_applies_non_passthrough() {
        let (mut project, seq_id) = single_solid_project(Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        });
        project.sequences.get_mut(&seq_id).unwrap().video_tracks[0].clips[0]
            .effects
            .push(ClipEffect::new(EffectKind::Invert));
        let out = compile(
            &project,
            seq_id,
            0,
            crate::contract::Tick(0),
            Quality::FULL,
            None,
        );
        let img = evaluate(&out.graph, (4, 4), &mut EmptyProvider);
        for p in &img.pixels {
            for (c, &v) in p[..3].iter().enumerate() {
                assert!(v.abs() < 1e-4, "inverted white → black, channel {c} = {v}");
            }
            assert!((p[3] - 1.0).abs() < 1e-4);
        }
    }

    /// A CrossDissolve at its exact midpoint blends the outgoing + incoming clips
    /// 50/50 (08 §2.0b). Two adjacent opaque solids on one track; the second
    /// transitions in from the first.
    #[test]
    fn cross_dissolve_midpoint_is_5050() {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("seq", FrameRate::FPS_30, 4, 4);
        let seq_id = seq.id;
        let mut t = Track::new(TrackKind::Video, "V1");
        // A: red [0,100); B: blue [100,200) transitioning in over 40 ticks.
        t.clips.push(Clip::new(
            ClipSource::SolidColor {
                color: Color {
                    r: 1.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            },
            crate::contract::Tick(0),
            crate::contract::Tick(100),
        ));
        let mut b = Clip::new(
            ClipSource::SolidColor {
                color: Color {
                    r: 0.0,
                    g: 0.0,
                    b: 1.0,
                    a: 1.0,
                },
            },
            crate::contract::Tick(100),
            crate::contract::Tick(100),
        );
        b.transition_in = Some(Transition::new(
            TransitionKind::CrossDissolve,
            crate::contract::Tick(40),
        ));
        t.clips.push(b);
        seq.video_tracks.push(t);
        project.insert_sequence(seq);

        // Midpoint of the [100,140) window → raw 0.5, EaseInOut(0.5)=0.5.
        let out = compile(
            &project,
            seq_id,
            0,
            crate::contract::Tick(120),
            Quality::FULL,
            None,
        );
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        let img = evaluate(&out.graph, (4, 4), &mut EmptyProvider);
        for p in &img.pixels {
            assert!((p[0] - 0.5).abs() < 1e-4, "red half r={}", p[0]);
            assert!(p[1].abs() < 1e-4, "g={}", p[1]);
            assert!((p[2] - 0.5).abs() < 1e-4, "blue half b={}", p[2]);
            assert!((p[3] - 1.0).abs() < 1e-4);
        }
    }

    /// A two-node composition (`SolidColor(white) → Invert → Output`) lowers to IR
    /// and evaluates: the invert produces black, exercising node-catalog lowering
    /// + the real effect kernel end-to-end.
    #[test]
    fn two_node_composition_lowers_and_evals() {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("seq", FrameRate::FPS_30, 4, 4);
        let seq_id = seq.id;
        let tk = {
            seq.video_tracks.push(Track::new(TrackKind::Video, "V1"));
            0usize
        };

        let mut solid = GraphNode::new(GraphOp::SolidColor);
        solid.params.base.0.set(
            "params.color",
            photonic_core::timeline::PropValue::Color(Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            }),
        );
        let invert = GraphNode::new(GraphOp::Invert);
        let output = GraphNode::new(GraphOp::Output);
        let (so, iv, ou) = (solid.id, invert.id, output.id);
        let mut nodes = std::collections::HashMap::new();
        for n in [solid, invert, output] {
            nodes.insert(n.id, n);
        }
        let graph = photonic_core::timeline::NodeGraph {
            id: GraphId::new(),
            name: "comp".into(),
            nodes,
            edges: vec![
                GraphEdge {
                    from: (so, GOutPort::PRIMARY),
                    to: (iv, InPort::PRIMARY),
                },
                GraphEdge {
                    from: (iv, GOutPort::PRIMARY),
                    to: (ou, InPort::PRIMARY),
                },
            ],
            output: ou,
            ui: std::collections::HashMap::new(),
        };
        let gid = graph.id;
        project.graphs.insert(gid, graph);
        let mut clip = Clip::new(
            ClipSource::SolidColor {
                color: Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            },
            crate::contract::Tick(0),
            crate::contract::Tick::from_seconds(2),
        );
        clip.composition = Some(gid);
        seq.video_tracks[tk].clips.push(clip);
        project.insert_sequence(seq);

        let out = compile(
            &project,
            seq_id,
            0,
            crate::contract::Tick(0),
            Quality::FULL,
            None,
        );
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.graph
                .nodes
                .iter()
                .any(|n| matches!(&n.op, IrOp::Effect { kind, .. } if *kind == EffectKind::Invert)),
            "the composition's Invert lowered to an Effect{{Invert}} node"
        );
        let img = evaluate(&out.graph, (4, 4), &mut EmptyProvider);
        for p in &img.pixels {
            for (c, &v) in p[..3].iter().enumerate() {
                assert!(v.abs() < 1e-4, "white inverted to black, channel {c} = {v}");
            }
        }
    }

    /// A bare SolidColor graph evaluates to the premultiplied linear color.
    #[test]
    fn solid_graph_matches_linear_conversion() {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("seq", FrameRate::FPS_30, 2, 2);
        let seq_id = seq.id;
        let mut t = Track::new(TrackKind::Video, "V1");
        // 50% grey in sRGB → ~0.2140 linear.
        t.clips.push(Clip::new(
            ClipSource::SolidColor {
                color: Color {
                    r: 0.5,
                    g: 0.5,
                    b: 0.5,
                    a: 1.0,
                },
            },
            crate::contract::Tick(0),
            crate::contract::Tick::from_seconds(2),
        ));
        seq.video_tracks.push(t);
        project.insert_sequence(seq);
        let out = compile(
            &project,
            seq_id,
            0,
            crate::contract::Tick(0),
            Quality::FULL,
            None,
        );
        let img = evaluate(&out.graph, (2, 2), &mut EmptyProvider);
        let expected = {
            let c = 0.5f32;
            ((c + 0.055) / 1.055).powf(2.4)
        };
        for p in &img.pixels {
            assert!((p[0] - expected).abs() < 1e-4, "grey linear {}", p[0]);
        }
        // Sanity on the constant used above.
        assert!((expected - 0.2140).abs() < 1e-3);
        let _ = LinearColor {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };
    }

    /// G7 adjustment layer (07 §2 / source-substitution): an Adjustment clip
    /// carries no source of its own — its grade re-roots the *composite of every
    /// track beneath it* across the Adjustment's own span. Bottom track V1 holds
    /// three sequential solids: A `[0,100)` and B `[100,200)` fall under a single
    /// Adjustment `[0,200)` on V2 carrying Exposure +1 (×2 in linear); the control
    /// C `[200,300)` sits past the Adjustment's end. Evaluated per frame, A and B
    /// are BOTH shifted while C is untouched — proving the re-root is applied to
    /// whichever lower clip composites at each tick and only within the
    /// Adjustment's extent, not per-source and not to unrelated regions.
    #[test]
    fn adjustment_exposure_shifts_every_lower_clip_but_not_the_control() {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("seq", FrameRate::FPS_30, 4, 4);
        let seq_id = seq.id;

        let grey_half = Color {
            r: 0.5,
            g: 0.5,
            b: 0.5,
            a: 1.0,
        };
        let grey_quarter = Color {
            r: 0.25,
            g: 0.25,
            b: 0.25,
            a: 1.0,
        };
        let mut v1 = Track::new(TrackKind::Video, "V1");
        v1.clips.push(Clip::new(
            ClipSource::SolidColor { color: grey_half },
            crate::contract::Tick(0),
            crate::contract::Tick(100),
        )); // A [0,100)
        v1.clips.push(Clip::new(
            ClipSource::SolidColor {
                color: grey_quarter,
            },
            crate::contract::Tick(100),
            crate::contract::Tick(100),
        )); // B [100,200)
        v1.clips.push(Clip::new(
            ClipSource::SolidColor { color: grey_half },
            crate::contract::Tick(200),
            crate::contract::Tick(100),
        )); // C [200,300) control
        seq.video_tracks.push(v1);

        // Top track: one Adjustment [0,200) carrying Exposure +1 stop.
        let mut adj = Clip::new(
            ClipSource::Adjustment,
            crate::contract::Tick(0),
            crate::contract::Tick(200),
        );
        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        ));
        adj.grade = Some(grade);
        let mut v2 = Track::new(TrackKind::Video, "V2");
        v2.clips.push(adj);
        seq.video_tracks.push(v2);

        project.insert_sequence(seq);

        // sRGB→linear; the Exposure +1 stop doubles the linear value.
        let lin = |c: f32| ((c + 0.055) / 1.055).powf(2.4);
        let eval_at = |t: i64| {
            let out = compile(
                &project,
                seq_id,
                0,
                crate::contract::Tick(t),
                Quality::FULL,
                None,
            );
            evaluate(&out.graph, (4, 4), &mut EmptyProvider)
        };

        // t=50: clip A beneath the Adjustment → linear(0.5) × 2.
        let want_a = lin(0.5) * 2.0;
        for p in &eval_at(50).pixels {
            assert!(
                (p[0] - want_a).abs() < 1e-3,
                "A shifted r={} want {}",
                p[0],
                want_a
            );
        }
        // t=150: clip B beneath the SAME Adjustment → linear(0.25) × 2. Proves
        // BOTH lower clips are re-rooted, not just the first-composited one.
        let want_b = lin(0.25) * 2.0;
        for p in &eval_at(150).pixels {
            assert!(
                (p[0] - want_b).abs() < 1e-3,
                "B shifted r={} want {}",
                p[0],
                want_b
            );
        }
        // t=250: control clip C is past the Adjustment's [0,200) span → untouched
        // (plain linear(0.5), NOT doubled), alpha preserved.
        let want_c = lin(0.5);
        for p in &eval_at(250).pixels {
            assert!(
                (p[0] - want_c).abs() < 1e-3,
                "C untouched r={} want {}",
                p[0],
                want_c
            );
            assert!((p[3] - 1.0).abs() < 1e-4, "control alpha preserved");
        }

        // Structural guard: the Adjustment introduces a Grade node within its span
        // and none past it, and never a track-fold Merge (it replaces the
        // accumulator rather than compositing over it).
        let g50 = compile(
            &project,
            seq_id,
            0,
            crate::contract::Tick(50),
            Quality::FULL,
            None,
        );
        assert!(
            g50.graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::Grade { .. })),
            "Grade present under the Adjustment"
        );
        assert!(
            !g50.graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::Merge { .. })),
            "Adjustment re-roots without introducing a Merge"
        );
        let g250 = compile(
            &project,
            seq_id,
            0,
            crate::contract::Tick(250),
            Quality::FULL,
            None,
        );
        assert!(
            !g250
                .graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::Grade { .. })),
            "no Grade past the Adjustment's span"
        );
    }
}
