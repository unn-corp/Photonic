//! Caption text compositing (06 §5.3 — the `CaptionOverlay` GPU path).
//!
//! This is the public entry point the video-engine evaluator (`photonic-video`
//! `graph::eval`) uses to burn a compiled `CaptionBatch` of styled,
//! karaoke-resolved caption cues onto a working frame. It is the same glyphon
//! pipeline the interactive canvas uses, so burned captions match preview.
//!
//! (A `headless_text` sibling once rasterized document text for raster export
//! against an sRGB8 target. It was removed as dead code — headless export
//! outlines text to filled paths up front via `outline_document_text`, so no
//! `Text` node ever survived to reach it.) Captions instead composite
//! **directly onto the `Rgba16Float`, premultiplied, linear-light working
//! texture** (D-09) the evaluator carries, which glyphon does correctly for
//! free:
//!
//! - The atlas is created in glyphon's default [`ColorMode::Accurate`], whose
//!   shader converts each sRGB vertex colour to linear before output — so the
//!   caller passes plain **sRGB straight** colours and the glyphs land in the
//!   linear working space with the right gamma.
//! - glyphon's pipeline uses `BlendState::ALPHA_BLENDING` (`SrcAlpha`,
//!   `OneMinusSrcAlpha`) and emits *straight* colour with coverage-scaled alpha,
//!   which is exactly premultiplied `over` when the destination is premultiplied
//!   (text on top, straight-alpha over premultiplied, done correctly).
//!
//! Shaping and line-wrap happen here, at render time (06 §5.3): cues store words,
//! not pre-wrapped lines, so a later font/width change reflows automatically.
//!
//! Not yet rendered here (tracked follow-ups, noted so callers don't assume
//! otherwise): text **stroke/outline** and **background box** (glyphon has no
//! outline primitive), the karaoke **Underline** decoration, and the intra-glyph
//! **FillSweep** split — the compiler approximates FillSweep at word granularity
//! by colour-lerping the sweeping word (see `graph::compile`). Fill colour,
//! per-word karaoke recolour (WordPop / FillSweep-approx), FadeWords/SlideUp
//! opacity, and Typewriter reveal are all honoured.

use std::sync::Mutex;

use glyphon::{
    Attrs, Buffer, Cache, Color as GlyphonColor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};

/// One caption word to render: its text plus the fully cascade- and
/// karaoke-resolved style at the compiled tick (06 §5.1/§5.3). Colours are
/// **sRGB straight** RGBA `[r, g, b, a]` bytes; the compositor's atlas linearizes
/// them for the working texture. Determinism is the compiler's job — this type
/// carries no time, only resolved state.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionWordRun {
    pub text: String,
    pub font_family: String,
    pub font_weight: u16,
    /// sRGB straight RGBA, alpha already folded with animation opacity.
    pub color: [u8; 4],
}

/// One caption cue to render: its resolved words plus cue-level layout. Metrics
/// (`font_size`, `line_height_mul`) are cue-level because glyphon shapes one
/// buffer per cue with a single [`Metrics`]; per-word variation is limited to
/// family / weight / colour (06 §5.3).
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionCueRun {
    pub words: Vec<CaptionWordRun>,
    /// Font size in physical pixels.
    pub font_size: f32,
    /// Line-height multiple of `font_size`.
    pub line_height_mul: f32,
    /// Normalized anchor `[x, y]` in `0..1`: `x` is the horizontal centre, `y`
    /// the top of the text box (already adjusted for any SlideUp offset).
    pub anchor: [f32; 2],
    /// Normalized max text width for wrapping (`0..1` of frame width).
    pub max_width: f32,
}

/// Owns glyphon's `&mut`-only state (font system, atlas, renderer, …) behind a
/// `Mutex` so the evaluator can composite through `&self` (mirrors
/// `HeadlessRenderer`'s `HeadlessText`). Created once per evaluator against the
/// working-texture format.
pub struct CaptionCompositor {
    inner: Mutex<CaptionText>,
}

struct CaptionText {
    font_system: FontSystem,
    swash_cache: SwashCache,
    #[allow(dead_code)] // kept alive for the atlas/viewport it backs
    cache: Cache,
    viewport: Viewport,
    atlas: TextAtlas,
    renderer: TextRenderer,
}

impl CaptionCompositor {
    /// Build a compositor whose atlas/pipeline target `format` — pass the
    /// evaluator's working-texture format (`Rgba16Float`). Default
    /// [`ColorMode::Accurate`](glyphon::ColorMode) so sRGB colours are linearized
    /// for the linear-light target.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(device);
        let viewport = Viewport::new(device, &cache);
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let renderer =
            TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);
        CaptionCompositor {
            inner: Mutex::new(CaptionText {
                font_system,
                swash_cache,
                cache,
                viewport,
                atlas,
                renderer,
            }),
        }
    }

    /// Composite `cues` onto `target` (size `w`×`h`), loading the existing
    /// contents so the glyphs land **over** the frame. Records and submits its
    /// own encoder. A shaping/atlas failure is logged and skipped (never panics).
    pub fn composite(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::Texture,
        cues: &[CaptionCueRun],
        w: u32,
        h: u32,
    ) {
        if cues.is_empty() || w == 0 || h == 0 {
            return;
        }
        let mut st = self.inner.lock().unwrap();
        let st = &mut *st;
        st.viewport.update(
            queue,
            Resolution {
                width: w,
                height: h,
            },
        );

        // One shaped buffer per cue. Empty cues (e.g. Typewriter before any
        // reveal) contribute nothing.
        let mut buffers: Vec<Buffer> = Vec::with_capacity(cues.len());
        let mut keep: Vec<usize> = Vec::with_capacity(cues.len());
        for (ci, cue) in cues.iter().enumerate() {
            if cue.words.iter().all(|word| word.text.is_empty()) {
                continue;
            }
            let font_size = cue.font_size.max(1.0);
            let line_height = font_size * cue.line_height_mul.max(0.1);
            let mut buf = Buffer::new(&mut st.font_system, Metrics::new(font_size, line_height));
            // Wrap at the cue's max_width (normalized → pixels); no vertical cap.
            let wrap_w = (cue.max_width.clamp(0.0, 1.0) * w as f32).max(font_size);
            buf.set_size(&mut st.font_system, Some(wrap_w), None);

            // Per-word coloured spans, joined only where a separator belongs
            // (42 §6.5): no space is emitted between scriptio-continua clusters.
            let spans = build_caption_spans(&cue.words);
            buf.set_rich_text(&mut st.font_system, spans, Attrs::new(), Shaping::Advanced);
            buf.shape_until_scroll(&mut st.font_system, false);
            buffers.push(buf);
            keep.push(ci);
        }
        if buffers.is_empty() {
            return;
        }

        let text_areas: Vec<TextArea> = keep
            .iter()
            .zip(buffers.iter())
            .map(|(&ci, buf)| {
                let cue = &cues[ci];
                // Centre horizontally on the anchor using the measured line width.
                let text_w = buf
                    .layout_runs()
                    .map(|run| run.line_w)
                    .fold(0.0_f32, f32::max);
                let left = cue.anchor[0] * w as f32 - text_w * 0.5;
                let top = cue.anchor[1] * h as f32;
                TextArea {
                    buffer: buf,
                    left,
                    top,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: i32::MIN,
                        top: i32::MIN,
                        right: i32::MAX,
                        bottom: i32::MAX,
                    },
                    default_color: GlyphonColor::rgba(255, 255, 255, 255),
                    custom_glyphs: &[],
                }
            })
            .collect();

        if let Err(e) = st.renderer.prepare(
            device,
            queue,
            &mut st.font_system,
            &mut st.atlas,
            &st.viewport,
            text_areas,
            &mut st.swash_cache,
        ) {
            tracing::warn!("caption glyphon prepare failed: {:?}", e);
            return;
        }

        let view = target.create_view(&Default::default());
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("caption_overlay_enc"),
        });
        {
            let mut pass = enc
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("caption_overlay_pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                })
                .forget_lifetime();
            // Video textures are bucket-padded; layout and rasterization must
            // both target the logical frame, leaving the padding untouched.
            pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
            if let Err(e) = st.renderer.render(&st.atlas, &st.viewport, &mut pass) {
                tracing::warn!("caption glyphon render failed: {:?}", e);
            }
        }
        queue.submit([enc.finish()]);
        st.atlas.trim();
    }
}

/// Build the glyphon rich-text spans for one cue's words. Each non-empty word
/// contributes its coloured span; a single `" "` separator span is inserted
/// between two words **only where one belongs** (42 §6.5) — never between
/// scriptio-continua clusters (CJK / Kana / Thai / …), so a per-cluster
/// tokenized caption renders `こんにちは`, not `こ ん に ち は`. The separator
/// inherits the following word's attrs (unchanged from the previous inline
/// form). Empty words are skipped, and the previous *non-empty* word's text
/// drives the separator decision.
fn build_caption_spans(words: &[CaptionWordRun]) -> Vec<(&str, Attrs<'_>)> {
    let mut spans: Vec<(&str, Attrs)> = Vec::with_capacity(words.len() * 2);
    let mut prev: Option<&str> = None;
    for word in words {
        if word.text.is_empty() {
            continue;
        }
        let attrs = Attrs::new()
            .family(Family::Name(&word.font_family))
            .weight(Weight(word.font_weight))
            .color(GlyphonColor::rgba(
                word.color[0],
                word.color[1],
                word.color[2],
                word.color[3],
            ));
        if let Some(p) = prev {
            if photonic_core::text_metrics::needs_separator(p, word.text.as_str()) {
                spans.push((" ", attrs));
            }
        }
        spans.push((word.text.as_str(), attrs));
        prev = Some(word.text.as_str());
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str) -> CaptionWordRun {
        CaptionWordRun {
            text: text.to_string(),
            font_family: "sans-serif".to_string(),
            font_weight: 400,
            color: [255, 255, 255, 255],
        }
    }

    #[test]
    fn ascii_words_get_space_separators() {
        let words = [word("Hello"), word("world")];
        let spans = build_caption_spans(&words);
        let texts: Vec<&str> = spans.iter().map(|s| s.0).collect();
        assert_eq!(texts, vec!["Hello", " ", "world"]);
    }

    #[test]
    fn scriptio_continua_cue_has_no_space_span() {
        // A Japanese cue tokenized per cluster must render with no interpolated
        // spaces (42 §6.5).
        let words = [
            word("\u{3053}"),
            word("\u{3093}"),
            word("\u{306B}"),
            word("\u{3061}"),
            word("\u{306F}"),
        ];
        let spans = build_caption_spans(&words);
        assert!(
            spans.iter().all(|s| s.0 != " "),
            "no separator span expected"
        );
        let joined: String = spans.iter().map(|s| s.0).collect();
        assert_eq!(joined, "\u{3053}\u{3093}\u{306B}\u{3061}\u{306F}");
    }

    #[test]
    fn empty_words_are_skipped() {
        let words = [word("hi"), word(""), word("there")];
        let spans = build_caption_spans(&words);
        let texts: Vec<&str> = spans.iter().map(|s| s.0).collect();
        assert_eq!(texts, vec!["hi", " ", "there"]);
    }
}
