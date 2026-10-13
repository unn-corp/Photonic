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
//! The Legacy entry point does not paint outlines or background boxes. Managed
//! evaluation explicitly composes these through caption_decoration.
//! Not rendered by this glyph-only entry point (so callers do not assume
//! otherwise): text **stroke/outline** and **background box** (glyphon has no
//! outline primitive), the karaoke **Underline** decoration, and the intra-glyph
//! **FillSweep** split. Managed evaluation adds spatial paint; the Legacy compiler
//! approximates FillSweep at word granularity
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
    /// Optional resolved `[font_size, line_height]` in physical pixels.
    /// None preserves the original cue-level shaping path exactly.
    pub metrics: Option<[f32; 2]>,
    pub text: String,
    pub font_family: String,
    pub font_weight: u16,
    /// sRGB straight RGBA, alpha already folded with animation opacity.
    pub color: [u8; 4],
}

/// One caption cue to render: its resolved words plus cue-level layout. Metrics
/// (`font_size`, `line_height_mul`) provide buffer defaults. Managed captions
/// may override individual word metrics through rich-text span attributes;
/// Legacy callers retain their original cue-level metrics (06 §5.3).
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionCueRun {
    pub direction: photonic_core::timeline::TextDirection,
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

impl CaptionCueRun {
    /// Scale both default and explicit word metrics once for a preview canvas.
    pub fn scale_metrics(&mut self, scale: f32) {
        self.font_size *= scale;
        for word in &mut self.words {
            if let Some(metrics) = &mut word.metrics {
                metrics[0] *= scale;
                metrics[1] *= scale;
            }
        }
    }
}

/// One shaped word segment on one wrapped line, in target pixel coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionWordBounds {
    pub word: usize,
    pub bounds: [f32; 4],
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
        if let Err(error) = self.composite_checked(device, queue, target, cues, w, h) {
            tracing::warn!("caption composite failed: {error}");
        }
    }

    /// The typographic line rectangle used by managed text backgrounds.
    /// Uses the same shaping, wrapping, metrics and anchor as composite_checked.
    pub fn layout_bounds_checked(
        &self,
        cue: &CaptionCueRun,
        w: u32,
        h: u32,
    ) -> Result<Option<[f32; 4]>, String> {
        validate_caption_layout(cue, w, h)?;
        if cue.words.iter().all(|word| word.text.is_empty()) {
            return Ok(None);
        }
        let mut state = self
            .inner
            .lock()
            .map_err(|_| "caption renderer lock poisoned".to_string())?;
        let font_size = cue.font_size.max(1.0);
        let mut buffer = Buffer::new(
            &mut state.font_system,
            Metrics::new(font_size, font_size * cue.line_height_mul.max(0.1)),
        );
        buffer.set_size(
            &mut state.font_system,
            Some((cue.max_width.clamp(0.0, 1.0) * w as f32).max(font_size)),
            None,
        );
        buffer.set_rich_text(
            &mut state.font_system,
            build_caption_spans_direction(&cue.words, false, cue.direction),
            Attrs::new(),
            Shaping::Advanced,
        );
        align_explicit_caption(&mut buffer, cue.direction);
        buffer.shape_until_scroll(&mut state.font_system, false);
        let runs: Vec<_> = buffer.layout_runs().collect();
        if runs.is_empty() {
            return Ok(None);
        }
        let width = runs.iter().map(|r| r.line_w).fold(0.0f32, f32::max);
        let top = runs
            .iter()
            .map(|r| r.line_top)
            .fold(f32::INFINITY, f32::min);
        let bottom = runs
            .iter()
            .map(|r| r.line_top + r.line_height)
            .fold(f32::NEG_INFINITY, f32::max);
        let left = cue.anchor[0] * w as f32 - width * 0.5;
        let y = cue.anchor[1] * h as f32;
        let bounds = [left, y + top, left + width, y + bottom];
        if bounds.iter().any(|v| !v.is_finite()) {
            return Err("non-finite shaped text bounds".into());
        }
        Ok(Some(bounds))
    }

    /// Exact typographic word spans from the same font, shaping, wrapping and
    /// anchor used for rendering. Separators are excluded from underline spans.
    pub fn word_bounds_checked(
        &self,
        cue: &CaptionCueRun,
        w: u32,
        h: u32,
    ) -> Result<Vec<CaptionWordBounds>, String> {
        validate_caption_layout(cue, w, h)?;
        if cue.words.iter().all(|word| word.text.is_empty()) {
            return Ok(Vec::new());
        }
        let mut state = self
            .inner
            .lock()
            .map_err(|_| "caption renderer lock poisoned".to_string())?;
        let font_size = cue.font_size.max(1.0);
        let mut buffer = Buffer::new(
            &mut state.font_system,
            Metrics::new(font_size, font_size * cue.line_height_mul.max(0.1)),
        );
        buffer.set_size(
            &mut state.font_system,
            Some((cue.max_width.clamp(0.0, 1.0) * w as f32).max(font_size)),
            None,
        );
        buffer.set_rich_text(
            &mut state.font_system,
            build_caption_spans_direction(&cue.words, true, cue.direction),
            Attrs::new(),
            Shaping::Advanced,
        );
        align_explicit_caption(&mut buffer, cue.direction);
        buffer.shape_until_scroll(&mut state.font_system, false);
        let width = buffer
            .layout_runs()
            .map(|r| r.line_w)
            .fold(0.0_f32, f32::max);
        let left = cue.anchor[0] * w as f32 - width * 0.5;
        let top = cue.anchor[1] * h as f32;
        let mut output = Vec::new();
        for run in buffer.layout_runs() {
            let mut spans = std::collections::BTreeMap::<usize, (f32, f32)>::new();
            for glyph in run.glyphs {
                if glyph.metadata >= cue.words.len() {
                    continue;
                }
                let a = glyph.x.min(glyph.x + glyph.w);
                let b = glyph.x.max(glyph.x + glyph.w);
                let entry = spans.entry(glyph.metadata).or_insert((a, b));
                entry.0 = entry.0.min(a);
                entry.1 = entry.1.max(b);
            }
            for (word, (a, b)) in spans {
                let bounds = [
                    left + a,
                    top + run.line_top,
                    left + b,
                    top + run.line_top + run.line_height,
                ];
                if bounds.iter().any(|v| !v.is_finite()) {
                    return Err("nonfinite shaped word bounds".into());
                }
                if b > a {
                    output.push(CaptionWordBounds { word, bounds });
                }
            }
        }
        Ok(output)
    }

    /// Checked rendering for managed titles: callers must not publish a blank
    /// success when shaping or atlas preparation fails.
    pub fn composite_checked(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::Texture,
        cues: &[CaptionCueRun],
        w: u32,
        h: u32,
    ) -> Result<(), String> {
        if w == 0
            || h == 0
            || w > target.width()
            || h > target.height()
            || target.format() != wgpu::TextureFormat::Rgba16Float
        {
            return Err("invalid caption target dimensions or format".into());
        }
        if cues.is_empty() {
            return Ok(());
        }
        for cue in cues {
            if cue.words.iter().any(|word| word.metrics.is_some()) {
                validate_caption_layout(cue, w, h)?;
            }
        }
        let mut st = self
            .inner
            .lock()
            .map_err(|_| "caption renderer lock poisoned".to_string())?;
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
            let spans = build_caption_spans_direction(&cue.words, false, cue.direction);
            buf.set_rich_text(&mut st.font_system, spans, Attrs::new(), Shaping::Advanced);
            align_explicit_caption(&mut buf, cue.direction);
            buf.shape_until_scroll(&mut st.font_system, false);
            buffers.push(buf);
            keep.push(ci);
        }
        if buffers.is_empty() {
            return Ok(());
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
            return Err(format!("caption glyphon prepare failed: {e:?}"));
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
                return Err(format!("caption glyphon render failed: {e:?}"));
            }
        }
        queue.submit([enc.finish()]);
        st.atlas.trim();
        Ok(())
    }
}

/// Caption anchoring centers the measured text block, not the wrap-width box.
/// Explicit RTL must not additionally right-align glyphs inside that larger box.
/// Keep Auto's historic rendering unchanged; only new explicit direction opts in.
fn align_explicit_caption(buffer: &mut Buffer, direction: photonic_core::timeline::TextDirection) {
    if direction != photonic_core::timeline::TextDirection::Auto {
        for line in &mut buffer.lines {
            line.set_align(Some(glyphon::cosmic_text::Align::Left));
        }
    }
}

fn validate_caption_layout(cue: &CaptionCueRun, w: u32, h: u32) -> Result<(), String> {
    if w == 0
        || h == 0
        || !cue.font_size.is_finite()
        || !(0.0..=2048.0).contains(&cue.font_size)
        || !cue.line_height_mul.is_finite()
        || cue.line_height_mul <= 0.0
        || cue.words.iter().any(|word| {
            word.metrics.is_some_and(|metrics| {
                metrics
                    .iter()
                    .any(|value| !value.is_finite() || *value <= 0.0 || *value > 4096.0)
            })
        })
        || !cue.max_width.is_finite()
        || cue.anchor.iter().any(|v| !v.is_finite())
    {
        return Err("invalid text layout canvas or metrics".into());
    }
    Ok(())
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
    build_caption_spans_indexed(words, false)
}

/// Zero-width strong direction marks set the paragraph base for cosmic-text's
/// Unicode bidi algorithm, including Latin brand names before RTL text. They
/// exist only in the renderer's shaping buffer, never in the stored cue text.
fn build_caption_spans_direction(
    words: &[CaptionWordRun],
    indexed: bool,
    direction: photonic_core::timeline::TextDirection,
) -> Vec<(&str, Attrs<'_>)> {
    use photonic_core::timeline::TextDirection;
    if direction == TextDirection::Auto {
        return if indexed {
            build_caption_spans_indexed(words, true)
        } else {
            build_caption_spans(words)
        };
    }
    let mark = if direction == TextDirection::Rtl {
        "\u{200f}"
    } else {
        "\u{200e}"
    };
    let mut output = Vec::new();
    let mut needs_mark = true;
    for (text, attrs) in build_caption_spans_indexed(words, indexed) {
        let marker = attrs
            .metadata(usize::MAX)
            .color(GlyphonColor::rgba(0, 0, 0, 0));
        let mut start = 0;
        let mut chars = text.char_indices().peekable();
        while let Some((index, ch)) = chars.next() {
            let paragraph_break = matches!(ch, '\n' | '\u{2029}')
                || (ch == '\r' && chars.peek().is_none_or(|(_, next)| *next != '\n'));
            if paragraph_break {
                let end = index + ch.len_utf8();
                let part = &text[start..end];
                if needs_mark && !part.trim_end_matches(['\r', '\n', '\u{2029}']).is_empty() {
                    output.push((mark, marker));
                }
                output.push((part, attrs));
                needs_mark = true;
                start = end;
            }
        }
        if start < text.len() {
            if needs_mark {
                output.push((mark, marker));
                needs_mark = false;
            }
            output.push((&text[start..], attrs));
        }
    }
    output
}
fn build_caption_spans_indexed(words: &[CaptionWordRun], indexed: bool) -> Vec<(&str, Attrs<'_>)> {
    let mut spans: Vec<(&str, Attrs)> = Vec::with_capacity(words.len() * 2);
    let mut prev: Option<&str> = None;
    for (index, word) in words.iter().enumerate() {
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
        let attrs = if let Some([font_size, line_height]) = word.metrics {
            attrs.metrics(Metrics::new(font_size, line_height))
        } else {
            attrs
        };
        let attrs = if indexed {
            attrs.metadata(index)
        } else {
            attrs
        };
        if let Some(p) = prev {
            if photonic_core::text_metrics::needs_separator(p, word.text.as_str()) {
                spans.push((
                    " ",
                    if indexed {
                        attrs.metadata(usize::MAX)
                    } else {
                        attrs
                    },
                ));
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

    #[test]
    fn word_metrics_are_explicit_and_shared_by_geometry_and_render_spans() {
        let mut words = vec![word("small"), word("large")];
        words[1].metrics = Some([40.0, 48.0]);
        let regular = build_caption_spans_direction(
            &words,
            false,
            photonic_core::timeline::TextDirection::Auto,
        );
        let indexed = build_caption_spans_direction(
            &words,
            true,
            photonic_core::timeline::TextDirection::Auto,
        );
        assert!(regular[0].1.metrics_opt.is_none());
        assert_eq!(
            regular.last().unwrap().1.metrics_opt,
            Some(Metrics::new(40.0, 48.0).into())
        );
        for (paint, bounds) in regular.iter().zip(&indexed) {
            assert_eq!(paint.0, bounds.0);
            assert_eq!(paint.1.metrics_opt, bounds.1.metrics_opt);
        }
    }

    fn word(text: &str) -> CaptionWordRun {
        CaptionWordRun {
            metrics: None,
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
    #[test]
    fn explicit_caption_direction_controls_each_paragraph_and_preserves_auto() {
        use photonic_core::timeline::TextDirection;
        let words = [word("OpenAI مرحبا\nBrand שלום"), word("OK")];
        let original = words.clone();
        let legacy = build_caption_spans(&words);
        assert_eq!(
            build_caption_spans_direction(&words, false, TextDirection::Auto),
            legacy
        );
        let mut fonts = FontSystem::new();
        for (direction, rtl) in [
            (TextDirection::Auto, false),
            (TextDirection::Ltr, false),
            (TextDirection::Rtl, true),
        ] {
            let spans = build_caption_spans_direction(&words, true, direction);
            let mut buffer = Buffer::new(&mut fonts, Metrics::new(30.0, 36.0));
            buffer.set_size(&mut fonts, Some(700.0), None);
            buffer.set_rich_text(&mut fonts, spans, Attrs::new(), Shaping::Advanced);
            buffer.shape_until_scroll(&mut fonts, false);
            let runs: Vec<_> = buffer.layout_runs().collect();
            assert_eq!(runs.len(), 2);
            assert!(runs.iter().all(|run| run.rtl == rtl), "{direction:?}");
            assert!(
                runs.iter()
                    .flat_map(|r| r.glyphs)
                    .filter(|g| g.metadata < words.len())
                    .all(|g| g.glyph_id != 0),
                "visible Arabic/Hebrew/Latin glyphs must not be tofu"
            );
        }
        assert_eq!(words, original);
    }
    #[test]
    fn explicit_direction_mark_spans_preserve_crlf_and_paragraph_text() {
        use photonic_core::timeline::TextDirection;
        let words = [word("A\r\nB\rC\u{2029}D")];
        let spans = build_caption_spans_direction(&words, true, TextDirection::Rtl);
        let text: String = spans.iter().map(|(s, _)| *s).collect();
        assert_eq!(text, "\u{200f}A\r\n\u{200f}B\r\u{200f}C\u{2029}\u{200f}D");
        assert_eq!(text.replace('\u{200f}', ""), words[0].text);
        let words = [word("\nA\r\n")];
        let text: String = build_caption_spans_direction(&words, true, TextDirection::Rtl)
            .iter()
            .map(|(s, _)| *s)
            .collect();
        assert_eq!(
            text, "\n\u{200f}A\r\n",
            "empty/trailing paragraphs must not acquire phantom text"
        );
    }
}
