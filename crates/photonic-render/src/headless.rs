//! Headless (off-screen) renderer — no window surface required.
//!
//! Used by the Lua script runner to render a document to a PNG file
//! without opening a visible window.

use crate::{
    canvas::CanvasView,
    pipeline::{
        blend_mode_index, coalesce_segments, create_blur_bgl, create_blur_pipeline_with_blend,
        create_camera_bind_group_layout, create_composite_bgl, create_composite_pipeline,
        create_convert_bgl, create_convert_pipeline, create_fill_pipeline,
        create_fill_pipeline_with_blend, draw_segments, segments_need_isolation,
        separable_blend_state, BlurBlend, BlurParams, CameraUniform, CompositeParams, DrawSegment,
        Vertex, SEPARABLE_BLEND_MODES, WORKING_FORMAT,
    },
    tessellator::{adaptive_tolerance, tessellate_fill, tessellate_stroke},
};
use image::{ImageBuffer, Rgba};
use photonic_core::{
    layer::BlendMode, node::SceneNodeKind, raster::blend::blend_rgb, style::FillKind, Color,
    Document,
};
use wgpu::util::DeviceExt;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const BG: wgpu::Color = wgpu::Color {
    r: 0.15,
    g: 0.15,
    b: 0.15,
    a: 1.0,
};
const MSAA_SAMPLES: u32 = 4;

// ─── Export options ───────────────────────────────────────────────────────────

/// What to render behind the artwork.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExportBackground {
    /// White artboard rectangle (matches the in-app canvas appearance).
    Artboard,
    /// Fully transparent — shapes rendered over alpha=0 background.
    Transparent,
}

/// Settings that control how a document is rendered for export.
#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub background: ExportBackground,
    /// When true the output is cropped to the tight bounding box of all
    /// visible artwork rather than the full artboard dimensions.
    pub crop_to_content: bool,
    /// Which square sizes to include in an `.ico` file.
    pub ico_sizes: Vec<u32>,
    /// JPEG quality (1–100). Only used by `render_jpeg_*` methods.
    pub jpeg_quality: u8,
    /// Optional crop region in document coordinates `(x, y, w, h)`. When set, the
    /// render fits to this rectangle (and draws the artboard background over it)
    /// instead of the full document — used for per-artboard export. Takes
    /// precedence over `crop_to_content`.
    pub region: Option<(f64, f64, f64, f64)>,
    /// Overprint preview (#22): when true, any node whose solid fill hex-matches
    /// an `overprint`-flagged [`SpotColor`] in `document.spot_colors` composites
    /// with [`BlendMode::Multiply`] instead of knocking out. A non-ICC visual
    /// approximation of print overprint. Off for normal export.
    pub overprint_preview: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            background: ExportBackground::Artboard,
            crop_to_content: false,
            ico_sizes: vec![16, 32, 48, 256],
            jpeg_quality: 90,
            region: None,
            overprint_preview: false,
        }
    }
}

pub struct HeadlessRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Whether the adapter supports reinterpreting `Rgba8UnormSrgb` as
    /// `Rgba8Unorm` via `view_formats` (needed for Tier B GPU→working). Soft
    /// adapters (llvmpipe on GitHub Linux runners) often lack
    /// [`wgpu::DownlevelFlags::VIEW_FORMATS`]; we empty `view_formats` and
    /// fall back to the Tier A readback path when this is false.
    supports_view_formats: bool,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    fill_pipeline: wgpu::RenderPipeline,
    /// One fill-pipeline variant per separable blend mode (matches the windowed
    /// renderer so headless export agrees with the on-canvas result).
    blend_pipelines: Vec<(BlendMode, wgpu::RenderPipeline)>,
    // ── Live-effects blur layer ───────────────────────────────────────────────
    /// 1-sample fill pipeline for rendering effect silhouettes to an offscreen
    /// texture (the blur ping-pong textures are single-sample).
    fill_pipeline_1spp: wgpu::RenderPipeline,
    blur_bgl: wgpu::BindGroupLayout,
    /// Separable blur pass (alpha-composited). Also used with sigma≈0 as a
    /// texture-passthrough compositor.
    blur_pipeline: wgpu::RenderPipeline,
    blur_sampler: wgpu::Sampler,
    // ── Full-blend isolation compositing (03 §2.4) ─────────────────────────────
    /// `COMPOSITE_SHADER` pipeline + its bind-group layout + a clamp sampler,
    /// used to composite an isolated layer over a backdrop for the 22 blend
    /// modes that fixed-function blending can't express.
    composite_bgl: wgpu::BindGroupLayout,
    composite_pipeline: wgpu::RenderPipeline,
    composite_sampler: wgpu::Sampler,
    // ── Tier B asset→working conversion (03 §2.5) ──────────────────────────────
    /// `CONVERT_SHADER` pipeline + layout, converting a rendered `SCENE_FORMAT`
    /// vector layer into a linear premultiplied `Rgba16Float` working texture.
    convert_bgl: wgpu::BindGroupLayout,
    convert_pipeline: wgpu::RenderPipeline,
}

impl HeadlessRenderer {
    pub async fn new() -> Self {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None, // no window surface
                force_fallback_adapter: false,
            })
            .await
            .expect("No suitable GPU adapter for headless rendering");

        let supports_view_formats = adapter
            .get_downlevel_capabilities()
            .flags
            .contains(wgpu::DownlevelFlags::VIEW_FORMATS);

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("headless_device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: Default::default(),
                },
                None,
            )
            .await
            .expect("Failed to create headless wgpu device");

        let camera_bgl = create_camera_bind_group_layout(&device);
        let initial_cam = CameraUniform::from_viewport(0.0, 0.0, 1.0, 1, 1);
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("headless_camera_buf"),
            contents: bytemuck::bytes_of(&initial_cam),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("headless_camera_bg"),
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let fill_pipeline = create_fill_pipeline(&device, FORMAT, &camera_bgl, MSAA_SAMPLES);
        let blend_pipelines: Vec<(BlendMode, wgpu::RenderPipeline)> = SEPARABLE_BLEND_MODES
            .iter()
            .filter_map(|&mode| {
                separable_blend_state(mode).map(|blend| {
                    (
                        mode,
                        create_fill_pipeline_with_blend(
                            &device,
                            FORMAT,
                            &camera_bgl,
                            MSAA_SAMPLES,
                            blend,
                        ),
                    )
                })
            })
            .collect();
        let fill_pipeline_1spp = create_fill_pipeline(&device, FORMAT, &camera_bgl, 1);
        let blur_bgl = create_blur_bgl(&device);
        let blur_pipeline =
            create_blur_pipeline_with_blend(&device, FORMAT, &blur_bgl, BlurBlend::StraightAlpha);
        let blur_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("headless_blur_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let composite_bgl = create_composite_bgl(&device);
        let composite_pipeline = create_composite_pipeline(&device, FORMAT, &composite_bgl);
        // 1:1 fullscreen sampling — clamp, and linear is exact at texel centres.
        let composite_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("headless_composite_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let convert_bgl = create_convert_bgl(&device);
        let convert_pipeline = create_convert_pipeline(&device, &convert_bgl);

        Self {
            device,
            queue,
            supports_view_formats,
            camera_buffer,
            camera_bind_group,
            fill_pipeline,
            blend_pipelines,
            fill_pipeline_1spp,
            blur_bgl,
            blur_pipeline,
            blur_sampler,
            composite_bgl,
            composite_pipeline,
            composite_sampler,
            convert_bgl,
            convert_pipeline,
        }
    }

    /// Render `document` to a PNG and return the bytes.
    ///
    /// Output dimensions match the document artboard (clamped to 1 pixel minimum).
    pub fn render_png(&self, document: &Document) -> Vec<u8> {
        let w = (document.width as u32).max(1);
        let h = (document.height as u32).max(1);
        self.render_png_at_size(document, w, h)
    }

    /// Render `document` to a PNG at an explicit pixel size using default options.
    pub fn render_png_at_size(&self, document: &Document, w: u32, h: u32) -> Vec<u8> {
        self.render_png_with_opts(document, w, h, &ExportOptions::default())
    }

    /// Render `document` to a PNG at an explicit pixel size with full export control.
    /// Render to a raw RGBA8 pixel buffer (`(pixels, width, height)`), shared by
    /// PNG/JPEG/… export and the on-canvas Pixel/Overprint Preview overlay.
    /// Returns empty `pixels` on GPU readback failure.
    pub fn render_rgba_with_opts(
        &self,
        document: &Document,
        w: u32,
        h: u32,
        opts: &ExportOptions,
    ) -> (Vec<u8>, u32, u32) {
        let w = w.max(1);
        let h = h.max(1);

        // Text nodes: neither the GPU tessellation path (`build_geometry` only
        // emits Path geometry) nor the CPU compositor paint glyphs, so a live
        // `TextNode` would export as nothing — the PNG/raster export dropped text
        // entirely. Outline text to filled glyph paths up front (same font,
        // position and fill colour the live GUI and PDF export use) so both the
        // GPU and CPU paths below render text like every other filled shape. Only
        // pay the FontSystem cost when the document actually contains text.
        let outlined_doc;
        let document = if document
            .nodes
            .values()
            .any(|n| matches!(n.kind, SceneNodeKind::Text(_)))
        {
            let mut font_system = glyphon::FontSystem::new();
            outlined_doc = crate::outline_document_text(document, &mut font_system);
            &outlined_doc
        } else {
            document
        };

        // Camera: an explicit region (per-artboard export) wins; otherwise fit
        // the content bounding box or the full document to the output size.
        let include_artboard_bg = opts.background == ExportBackground::Artboard;
        let mut view = CanvasView::new(w, h);
        if let Some((rx, ry, rw, rh)) = opts.region {
            view.fit_to_rect(rx, ry, rw, rh);
        } else if opts.crop_to_content {
            // The crop bounds require geometry, but the final geometry must use
            // the resulting view scale. This preliminary pass is only for bounds.
            let (bounds_verts, _, _, _) =
                build_geometry(document, include_artboard_bg, opts.overprint_preview, 1.0);
            if let Some((cx, cy, cw, ch)) =
                content_bounds(&bounds_verts, include_artboard_bg, document)
            {
                view.fit_to_rect(cx, cy, cw, ch);
            } else {
                view.fit_to_rect(0.0, 0.0, document.width, document.height);
            }
        } else {
            view.fit_to_rect(0.0, 0.0, document.width, document.height);
        }

        let (verts, idxs, segments, blur_jobs) = build_geometry(
            document,
            include_artboard_bg,
            opts.overprint_preview,
            view.zoom,
        );

        let cam = CameraUniform::from_viewport(view.pan_x, view.pan_y, view.zoom, w, h);
        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&cam));

        let clear = match opts.background {
            ExportBackground::Artboard => BG,
            ExportBackground::Transparent => wgpu::Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
        };

        // ── Mixed-document path: CPU compositor ──────────────────────────────
        // When the document contains raster (pixel) layers, render the WHOLE
        // document on the CPU in true draw order so vector and raster nodes
        // z-interleave correctly (the GPU path renders all vectors as one plane
        // beneath the rasters). Pure-vector documents keep the GPU path below.
        //
        // Pattern fills are also routed here: the GPU path colours each fill at
        // its mesh *vertices* (great for gradients), but a tiled pattern must be
        // sampled per *pixel* — which is exactly what the CPU compositor does via
        // `FillKind::sample_at`. (Like the raster path, glyph text is not painted
        // by the CPU compositor; a document mixing pattern fills and text follows
        // the same long-standing limitation as raster+text documents.)
        if document_needs_cpu_compositor(document) {
            let mut pixels = vec![0u8; (w as usize) * (h as usize) * 4];
            let bg = match opts.background {
                ExportBackground::Artboard => [
                    (BG.r * 255.0) as u8,
                    (BG.g * 255.0) as u8,
                    (BG.b * 255.0) as u8,
                    255,
                ],
                ExportBackground::Transparent => [0, 0, 0, 0],
            };
            for px in pixels.chunks_exact_mut(4) {
                px.copy_from_slice(&bg);
            }
            // White artboard rectangle (matches the GPU path's artboard quad).
            if include_artboard_bg {
                let (rx, ry, rw, rh) =
                    opts.region
                        .unwrap_or((0.0, 0.0, document.width, document.height));
                let (ax0, ay0) = view.canvas_to_screen(rx, ry);
                let (ax1, ay1) = view.canvas_to_screen(rx + rw, ry + rh);
                let x0 = (ax0.min(ax1).floor() as i64).max(0);
                let y0 = (ay0.min(ay1).floor() as i64).max(0);
                let x1 = (ax0.max(ax1).ceil() as i64).min(w as i64);
                let y1 = (ay0.max(ay1).ceil() as i64).min(h as i64);
                for yy in y0..y1 {
                    for xx in x0..x1 {
                        let i = ((yy as usize) * (w as usize) + xx as usize) * 4;
                        pixels[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                    }
                }
            }
            crate::compositor::composite_document(&mut pixels, w, h, document, &view);
            return (pixels, w, h);
        }

        // Render the vector scene into an off-screen SCENE_FORMAT texture (shared
        // with the §2.5 Tier B GPU-to-GPU path so both produce identical pixels).
        let tex = self.render_scene_texture(
            &verts,
            &idxs,
            &segments,
            &blur_jobs,
            &view,
            clear,
            include_artboard_bg,
            w,
            h,
        );

        // Copy texture → staging buffer (row stride must be aligned to 256)
        let bpr = align256(w * 4);
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("headless_staging"),
            size: (bpr * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut enc2 = self.device.create_command_encoder(&Default::default());
        enc2.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::ImageCopyBuffer {
                buffer: &staging,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([enc2.finish()]);

        // Map and read back
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::Maintain::Wait);
        if rx.recv().ok().and_then(|r| r.ok()).is_none() {
            return (vec![], w, h);
        }

        let raw = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h {
            let start = (row * bpr) as usize;
            let end = start + (w * 4) as usize;
            pixels.extend_from_slice(&raw[start..end]);
        }
        drop(raw);
        staging.unmap();

        // Composite raster (pixel) layers over the GPU-rendered vector output,
        // aligned via the same camera so raster and vector content register.
        composite_raster_nodes(&mut pixels, w, h, document, &view);

        (pixels, w, h)
    }

    /// Render to an in-memory PNG with options. Thin wrapper over
    /// [`Self::render_rgba_with_opts`] (the shared raw-RGBA path).
    pub fn render_png_with_opts(
        &self,
        document: &Document,
        w: u32,
        h: u32,
        opts: &ExportOptions,
    ) -> Vec<u8> {
        let (pixels, w, h) = self.render_rgba_with_opts(document, w, h, opts);
        if pixels.is_empty() {
            return vec![];
        }
        let img: ImageBuffer<Rgba<u8>, _> =
            ImageBuffer::from_raw(w, h, pixels).unwrap_or_else(|| ImageBuffer::new(w, h));
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap_or_default();
        png
    }

    /// Render `document` to JPEG at an explicit pixel size with full export control.
    ///
    /// JPEG does not support transparency — alpha is composited onto a white
    /// background before encoding.  Quality is taken from `opts.jpeg_quality`
    /// (clamped 1–100).
    pub fn render_jpeg_with_opts(
        &self,
        document: &Document,
        w: u32,
        h: u32,
        opts: &ExportOptions,
    ) -> Vec<u8> {
        // Render to RGBA pixels using the existing PNG pipeline.
        let rgba_bytes = self.render_png_with_opts(document, w, h, opts);

        // Decode the PNG into an image buffer so we can re-encode as JPEG.
        let img = image::load_from_memory_with_format(&rgba_bytes, image::ImageFormat::Png)
            .unwrap_or_else(|_| image::DynamicImage::new_rgba8(w, h));

        // Composite alpha onto white (to_rgb8 composites onto black).
        let rgba = img.to_rgba8();
        let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
        for (src, dst) in rgba.pixels().zip(rgb.pixels_mut()) {
            let a = src[3] as f32 / 255.0;
            dst[0] = (src[0] as f32 * a + 255.0 * (1.0 - a)) as u8;
            dst[1] = (src[1] as f32 * a + 255.0 * (1.0 - a)) as u8;
            dst[2] = (src[2] as f32 * a + 255.0 * (1.0 - a)) as u8;
        }
        let rgb = image::DynamicImage::ImageRgb8(rgb);

        let quality = opts.jpeg_quality.clamp(1, 100);
        let mut buf = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
            std::io::Cursor::new(&mut buf),
            quality,
        );
        rgb.write_with_encoder(encoder).unwrap_or_default();
        buf
    }

    /// Render `document` to WebP at an explicit pixel size with full export control.
    ///
    /// WebP supports transparency (lossy or lossless). Quality from `opts.jpeg_quality`
    /// (reused field; 1–100, where 100 = lossless).
    pub fn render_webp_with_opts(
        &self,
        document: &Document,
        w: u32,
        h: u32,
        opts: &ExportOptions,
    ) -> Vec<u8> {
        let rgba_bytes = self.render_png_with_opts(document, w, h, opts);
        let img = image::load_from_memory_with_format(&rgba_bytes, image::ImageFormat::Png)
            .unwrap_or_else(|_| image::DynamicImage::new_rgba8(w, h));

        let mut buf = Vec::new();
        let encoder =
            image::codecs::webp::WebPEncoder::new_lossless(std::io::Cursor::new(&mut buf));
        img.write_with_encoder(encoder).unwrap_or_default();
        buf
    }

    /// Render `document` to GIF at an explicit pixel size.
    pub fn render_gif_with_opts(
        &self,
        document: &Document,
        w: u32,
        h: u32,
        opts: &ExportOptions,
    ) -> Vec<u8> {
        let rgba_bytes = self.render_png_with_opts(document, w, h, opts);
        let img = image::load_from_memory_with_format(&rgba_bytes, image::ImageFormat::Png)
            .unwrap_or_else(|_| image::DynamicImage::new_rgba8(w, h));
        let mut buf = Vec::new();
        let encoder = image::codecs::gif::GifEncoder::new(std::io::Cursor::new(&mut buf));
        img.write_with_encoder(encoder).unwrap_or_default();
        buf
    }

    /// Render `document` to TIFF at an explicit pixel size.
    pub fn render_tiff_with_opts(
        &self,
        document: &Document,
        w: u32,
        h: u32,
        opts: &ExportOptions,
    ) -> Vec<u8> {
        let rgba_bytes = self.render_png_with_opts(document, w, h, opts);
        let img = image::load_from_memory_with_format(&rgba_bytes, image::ImageFormat::Png)
            .unwrap_or_else(|_| image::DynamicImage::new_rgba8(w, h));
        let mut buf = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Tiff,
        )
        .unwrap_or_default();
        buf
    }

    /// Render `document` as a multi-resolution `.ico` file and return the bytes.
    pub fn render_ico(&self, document: &Document) -> anyhow::Result<Vec<u8>> {
        self.render_ico_with_opts(document, &ExportOptions::default())
    }

    /// Render `document` as a `.ico` file with full export control.
    pub fn render_ico_with_opts(
        &self,
        document: &Document,
        opts: &ExportOptions,
    ) -> anyhow::Result<Vec<u8>> {
        let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);

        for &size in &opts.ico_sizes {
            let png = self.render_png_with_opts(document, size, size, opts);
            if png.is_empty() {
                continue;
            }
            let icon_image = ico::IconImage::read_png(std::io::Cursor::new(&png))?;
            icon_dir.add_entry(ico::IconDirEntry::encode(&icon_image)?);
        }

        let mut buf = Vec::new();
        icon_dir.write(std::io::Cursor::new(&mut buf))?;
        Ok(buf)
    }

    /// Render the vector scene into a fresh single-sample `SCENE_FORMAT`
    /// (`Rgba8UnormSrgb`) texture and return it (submitted, ready to read or
    /// convert). Shared by the RGBA readback path (`render_rgba_with_opts`) and
    /// the §2.5 Tier B GPU-to-GPU working-texture path, so both render identical
    /// pixels. The caller must have written the camera uniform for `view`.
    ///
    /// The returned texture carries `COPY_SRC | TEXTURE_BINDING` so it can be
    /// either read back or sampled by the Tier B conversion pass.
    #[allow(clippy::too_many_arguments)]
    fn render_scene_texture(
        &self,
        verts: &[Vertex],
        idxs: &[u32],
        segments: &[DrawSegment],
        blur_jobs: &[BlurJob],
        view: &CanvasView,
        clear: wgpu::Color,
        include_artboard_bg: bool,
        w: u32,
        h: u32,
    ) -> wgpu::Texture {
        // `view_formats` includes the non-sRGB counterpart so the Tier B
        // conversion pass can sample the stored bytes raw (03 §2.5); it does not
        // affect readback (Tier A) or the sRGB render itself. Soft adapters
        // (llvmpipe) often lack VIEW_FORMATS — leave the list empty then.
        let view_formats: &[wgpu::TextureFormat] = if self.supports_view_formats {
            &[wgpu::TextureFormat::Rgba8Unorm]
        } else {
            &[]
        };
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("headless_scene_tex"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats,
        });
        let tex_view = tex.create_view(&Default::default());

        // MSAA render target for the sharp document geometry.
        let msaa_tex =
            self.make_color_tex(w, h, MSAA_SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let msaa_view = msaa_tex.create_view(&Default::default());

        let mut enc = self.device.create_command_encoder(&Default::default());

        if blur_jobs.is_empty() && segments_need_isolation(segments) {
            // Full-blend path (03 §2.4): the document uses a blend mode that
            // fixed-function GPU blending can't express, so composite each
            // segment as an isolated layer through COMPOSITE_SHADER instead of
            // the normal-alpha approximation. Entered only for such documents,
            // so all-Normal/separable exports keep the byte-identical fast path.
            self.record_pass_isolated(&mut enc, &tex_view, verts, idxs, segments, clear, w, h);
        } else if blur_jobs.is_empty() {
            // Fast path: render the document straight into the readback target.
            self.record_pass(
                &mut enc, &msaa_view, &tex_view, verts, idxs, segments, clear,
            );
        } else {
            // Layered path: the live-effects blur layer must sit *between* the
            // artboard background and the sharp shapes. So render the shapes
            // (minus the artboard rect) to a transparent offscreen texture, blur
            // the effect silhouettes into a separate layer, then composite
            //   background → effects → shapes
            // into the readback target.
            let doc_tex = self.make_color_tex(
                w,
                h,
                1,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            );
            let doc_view = doc_tex.create_view(&Default::default());

            // The artboard rect is the first 4 verts / 6 indices when present;
            // skip it here and reproduce it via the composite clear colour.
            let skip = if include_artboard_bg { 6 } else { 0 } as u32;
            // Re-base the blend segments onto the sliced index buffer: drop the
            // artboard segment and shift the rest down by `skip` so the
            // separable-blend draw still covers exactly the shape geometry.
            let fx_segments: Vec<DrawSegment> = segments
                .iter()
                .filter_map(|s| {
                    let end = s.start + s.count;
                    if end <= skip {
                        None
                    } else {
                        let new_start = s.start.saturating_sub(skip);
                        Some(DrawSegment {
                            mode: s.mode,
                            start: new_start,
                            count: end - skip - new_start,
                        })
                    }
                })
                .collect();
            let transparent = wgpu::Color::TRANSPARENT;
            self.record_pass(
                &mut enc,
                &msaa_view,
                &doc_view,
                verts,
                &idxs[skip as usize..],
                &fx_segments,
                transparent,
            );

            let (fx_tex, fx_view) = self.render_effects_layer(&mut enc, blur_jobs, view.zoom, w, h);

            // Composite: clear to the artboard/background, then effects, then shapes.
            let comp_clear = if include_artboard_bg {
                wgpu::Color::WHITE
            } else {
                clear
            };
            self.composite_layers(&mut enc, &tex_view, &[&fx_view, &doc_view], comp_clear);
            drop(fx_tex);
            drop(doc_tex);
        }
        drop(msaa_tex); // keep alive until submit
        self.queue.submit([enc.finish()]);
        tex
    }

    /// Convert a rendered `SCENE_FORMAT` layer (sampled through `src_view` as
    /// **raw** sRGB bytes) into a fresh linear, premultiplied [`WORKING_FORMAT`]
    /// (`Rgba16Float`) texture — the asset→working boundary (03 §2.5 / §4.2).
    /// Shared by Tier B (a raw view of the GPU scene texture) and Tier A (an
    /// `Rgba8Unorm` upload of the CPU-readback bytes), so both are numerically
    /// identical. The result carries `TEXTURE_BINDING | COPY_SRC`.
    fn convert_srgb_texture_to_working(
        &self,
        src_view: &wgpu::TextureView,
        w: u32,
        h: u32,
    ) -> wgpu::Texture {
        let out = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("working_texture"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORKING_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let out_view = out.create_view(&Default::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("convert_bg"),
            layout: &self.convert_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.composite_sampler),
                },
            ],
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("convert_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &out_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.convert_pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..6, 0..1);
        }
        self.queue.submit([enc.finish()]);
        out
    }

    /// Tier B (03 §2.5): render a **pure-vector** `document` straight to a GPU
    /// working texture — linear, premultiplied `Rgba16Float`, transparent
    /// outside the artboard so it composites over video — with no CPU
    /// round-trip. This is the fast path for the common vector-title case;
    /// `photonic-video` consumes it in P3.
    ///
    /// Gated on [`document_needs_cpu_compositor`] (reused, not re-derived): for a
    /// document that predicate reports `true`, use the universal Tier A path
    /// (`render_rgba_with_opts` → upload → [`convert`]) instead. This
    /// debug-asserts the precondition.
    pub fn render_vector_to_working_texture(
        &self,
        document: &Document,
        w: u32,
        h: u32,
    ) -> wgpu::Texture {
        debug_assert!(
            !document_needs_cpu_compositor(document),
            "render_vector_to_working_texture is the pure-vector Tier B fast path; \
             route CPU-composited documents through Tier A"
        );
        let w = w.max(1);
        let h = h.max(1);
        // Transparent background (no artboard fill) — a vector asset composites
        // over the video graph, so its alpha must be meaningful.
        let mut view = CanvasView::new(w, h);
        view.fit_to_rect(0.0, 0.0, document.width, document.height);
        let (verts, idxs, segments, blur_jobs) = build_geometry(document, false, false, view.zoom);
        let cam = CameraUniform::from_viewport(view.pan_x, view.pan_y, view.zoom, w, h);
        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&cam));
        let scene = self.render_scene_texture(
            &verts,
            &idxs,
            &segments,
            &blur_jobs,
            &view,
            wgpu::Color::TRANSPARENT,
            false,
            w,
            h,
        );
        if self.supports_view_formats {
            // Sample the sRGB scene texture through a non-sRGB view so the bytes
            // are read raw and the conversion applies the sRGB EOTF explicitly —
            // the exact same math Tier A runs on the CPU-readback bytes.
            let raw_view = scene.create_view(&wgpu::TextureViewDescriptor {
                format: Some(wgpu::TextureFormat::Rgba8Unorm),
                ..Default::default()
            });
            self.convert_srgb_texture_to_working(&raw_view, w, h)
        } else {
            // Soft adapters (no VIEW_FORMATS): fall back to the Tier A path —
            // readback sRGB bytes, re-upload as raw Unorm, then convert.
            let rgba = self
                .readback_rgba8(&scene, w, h)
                .expect("scene texture readback for Tier A working conversion");
            let src = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("tier_a_srgb_upload"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.queue.write_texture(
                src.as_image_copy(),
                &rgba,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 4),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            let src_view = src.create_view(&Default::default());
            self.convert_srgb_texture_to_working(&src_view, w, h)
        }
    }

    /// Read a `FORMAT` (`COPY_SRC`) texture back as tightly-packed RGBA8.
    fn readback_rgba8(&self, tex: &wgpu::Texture, w: u32, h: u32) -> Option<Vec<u8>> {
        let bpr = align256(w * 4);
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("text_readback"),
            size: (bpr * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::ImageCopyBuffer {
                buffer: &staging,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([enc.finish()]);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::Maintain::Wait);
        rx.recv().ok()?.ok()?;
        let raw = slice.get_mapped_range();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h {
            let start = (row * bpr) as usize;
            out.extend_from_slice(&raw[start..start + (w * 4) as usize]);
        }
        drop(raw);
        staging.unmap();
        Some(out)
    }

    fn record_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        msaa_view: &wgpu::TextureView,
        resolve_view: &wgpu::TextureView,
        vertices: &[Vertex],
        indices: &[u32],
        segments: &[DrawSegment],
        clear: wgpu::Color,
    ) {
        if !vertices.is_empty() {
            let vbuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("hl_vbuf"),
                    contents: bytemuck::cast_slice(vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            let ibuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("hl_ibuf"),
                    contents: bytemuck::cast_slice(indices),
                    usage: wgpu::BufferUsages::INDEX,
                });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hl_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: msaa_view,
                    resolve_target: Some(resolve_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_vertex_buffer(0, vbuf.slice(..));
            pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
            draw_segments(
                &mut pass,
                segments,
                &self.blend_pipelines,
                &self.fill_pipeline,
                indices.len() as u32,
            );
        } else {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hl_clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: msaa_view,
                    resolve_target: Some(resolve_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
    }

    /// Full-blend document pass (03 §2.4): composite each draw segment as an
    /// isolated layer through `COMPOSITE_SHADER`, giving correct output for the
    /// 22 blend modes fixed-function blending can't express (HSL + backdrop-read
    /// separable). Each segment is rendered alone (MSAA, resolved) to an isolated
    /// `FORMAT` texture, then composited over the running backdrop with the
    /// segment's `blend_mode_index`. Backdrops ping-pong between two textures;
    /// the final segment composites straight into `target_view`.
    #[allow(clippy::too_many_arguments)]
    fn record_pass_isolated(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
        vertices: &[Vertex],
        indices: &[u32],
        segments: &[DrawSegment],
        clear: wgpu::Color,
        w: u32,
        h: u32,
    ) {
        // Nothing to draw, or degenerate segment list — just clear the target.
        if vertices.is_empty() || segments.is_empty() {
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hl_iso_clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            return;
        }

        let vbuf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("hl_iso_vbuf"),
                contents: bytemuck::cast_slice(vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let ibuf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("hl_iso_ibuf"),
                contents: bytemuck::cast_slice(indices),
                usage: wgpu::BufferUsages::INDEX,
            });

        let sampleable =
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        // Ping-pong backdrops (single-sample, sampleable), the isolated layer's
        // MSAA target + its resolved single-sample copy.
        let back_a = self.make_color_tex(w, h, 1, sampleable);
        let back_b = self.make_color_tex(w, h, 1, sampleable);
        let iso_msaa =
            self.make_color_tex(w, h, MSAA_SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let iso_ss = self.make_color_tex(w, h, 1, sampleable);
        let view_a = back_a.create_view(&Default::default());
        let view_b = back_b.create_view(&Default::default());
        let iso_msaa_view = iso_msaa.create_view(&Default::default());
        let iso_ss_view = iso_ss.create_view(&Default::default());

        // Seed backdrop A with the clear colour (the artboard/BG the first
        // segment composites over).
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("hl_iso_seed"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view_a,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        let mut backdrop_is_a = true;
        for (i, seg) in segments.iter().enumerate() {
            // 1. Render this segment alone to the isolated MSAA target, resolved
            //    into `iso_ss`. Cleared transparent so only the segment's
            //    coverage is present.
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("hl_iso_layer"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &iso_msaa_view,
                        resolve_target: Some(&iso_ss_view),
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Discard,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_vertex_buffer(0, vbuf.slice(..));
                pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
                pass.set_pipeline(&self.fill_pipeline);
                pass.draw_indexed(seg.start..seg.start + seg.count, 0, 0..1);
            }

            // 2. Composite `iso_ss` over the current backdrop with this segment's
            //    blend mode. The final segment writes straight to `target_view`.
            let backdrop_view = if backdrop_is_a { &view_a } else { &view_b };
            let is_last = i == segments.len() - 1;
            let out_view = if is_last {
                target_view
            } else if backdrop_is_a {
                &view_b
            } else {
                &view_a
            };

            let params = CompositeParams {
                mode: blend_mode_index(seg.mode),
                opacity: 1.0,
                _pad: [0.0; 2],
            };
            let pbuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("hl_iso_params"),
                    contents: bytemuck::bytes_of(&params),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("hl_iso_composite_bg"),
                layout: &self.composite_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(backdrop_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&iso_ss_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.composite_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: pbuf.as_entire_binding(),
                    },
                ],
            });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("hl_iso_composite"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: out_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            // The shader writes every pixel of the full-screen
                            // quad, so the load value is irrelevant.
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&self.composite_pipeline);
                pass.set_bind_group(0, &bind, &[]);
                pass.draw(0..6, 0..1);
            }
            if !is_last {
                backdrop_is_a = !backdrop_is_a;
            }
        }
    }

    /// Create a colour texture of the given size and sample count.
    fn make_color_tex(
        &self,
        w: u32,
        h: u32,
        sample_count: u32,
        usage: wgpu::TextureUsages,
    ) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("headless_fx_tex"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage,
            view_formats: &[],
        })
    }

    /// Bind group for the blur shader: source texture + sampler + params.
    fn blur_bind_group(
        &self,
        src: &wgpu::TextureView,
        sigma: f32,
        horizontal: bool,
    ) -> wgpu::BindGroup {
        let params = BlurParams {
            sigma,
            horizontal: horizontal as u32,
            _pad: [0.0; 2],
        };
        let buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("headless_blur_params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("headless_blur_bg"),
            layout: &self.blur_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.blur_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: buf.as_entire_binding(),
                },
            ],
        })
    }

    /// Render each blur job (silhouette → H-blur → V-blur) and accumulate them
    /// into a single-sample effects texture (straight-alpha "over"). Returns the
    /// accumulation texture and its view.
    fn render_effects_layer(
        &self,
        enc: &mut wgpu::CommandEncoder,
        jobs: &[BlurJob],
        zoom: f64,
        w: u32,
        h: u32,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        let fx_a = self.make_color_tex(w, h, 1, usage);
        let fx_b = self.make_color_tex(w, h, 1, usage);
        let fx_accum = self.make_color_tex(w, h, 1, usage);
        let (a_view, b_view, accum_view) = (
            fx_a.create_view(&Default::default()),
            fx_b.create_view(&Default::default()),
            fx_accum.create_view(&Default::default()),
        );

        // Clear the accumulator once; jobs composite into it with Load below.
        let mut accum_cleared = false;
        for job in jobs {
            if job.idxs.is_empty() {
                continue;
            }
            let sigma = (job.radius_doc * zoom).max(0.0) as f32;

            // Pass A: silhouette → fx_a (cleared transparent).
            let vbuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("fx_vbuf"),
                    contents: bytemuck::cast_slice(&job.verts),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            let ibuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("fx_ibuf"),
                    contents: bytemuck::cast_slice(&job.idxs),
                    usage: wgpu::BufferUsages::INDEX,
                });
            {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("fx_silhouette"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &a_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&self.fill_pipeline_1spp);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_vertex_buffer(0, vbuf.slice(..));
                pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..job.idxs.len() as u32, 0, 0..1);
            }

            // Pass B: horizontal blur fx_a → fx_b.
            {
                let bg = self.blur_bind_group(&a_view, sigma, true);
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("fx_blur_h"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &b_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&self.blur_pipeline);
                pass.set_bind_group(0, &bg, &[]);
                pass.draw(0..6, 0..1);
            }

            // Pass C: vertical blur fx_b → fx_accum (accumulate).
            {
                let bg = self.blur_bind_group(&b_view, sigma, false);
                let load = if accum_cleared {
                    wgpu::LoadOp::Load
                } else {
                    accum_cleared = true;
                    wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                };
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("fx_blur_v"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &accum_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&self.blur_pipeline);
                pass.set_bind_group(0, &bg, &[]);
                pass.draw(0..6, 0..1);
            }
        }

        (fx_accum, accum_view)
    }

    /// Composite `layers` (bottom-first) onto `target` over a cleared background,
    /// using the blur shader at sigma≈0 as a straight-alpha texture passthrough.
    fn composite_layers(
        &self,
        enc: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        layers: &[&wgpu::TextureView],
        clear: wgpu::Color,
    ) {
        for (i, layer) in layers.iter().enumerate() {
            let bg = self.blur_bind_group(layer, 0.0, true);
            let load = if i == 0 {
                wgpu::LoadOp::Clear(clear)
            } else {
                wgpu::LoadOp::Load
            };
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fx_composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.blur_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..6, 0..1);
        }
    }
}

// ─── Shared geometry builder ──────────────────────────────────────────────────

/// Compute the axis-aligned bounding box of all shape vertices (content only,
/// excluding the artboard background rect).  Returns `(min_x, min_y, width,
/// height)` in canvas space, or `None` if there are no shape vertices.
fn content_bounds(
    verts: &[Vertex],
    include_artboard_bg: bool,
    doc: &Document,
) -> Option<(f64, f64, f64, f64)> {
    // When the artboard bg was included it occupies the first 4 vertices.
    let skip = if include_artboard_bg { 4 } else { 0 };
    let shape_verts = &verts[skip..];
    if shape_verts.is_empty() {
        return None;
    }
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for v in shape_verts {
        min_x = min_x.min(v.position[0]);
        min_y = min_y.min(v.position[1]);
        max_x = max_x.max(v.position[0]);
        max_y = max_y.max(v.position[1]);
    }
    let w = (max_x - min_x) as f64;
    let h = (max_y - min_y) as f64;
    if w < 1.0 || h < 1.0 {
        // Degenerate — fall back to artboard
        return Some((0.0, 0.0, doc.width, doc.height));
    }
    Some((min_x as f64, min_y as f64, w, h))
}

/// Map each node id to the product of its ancestor groups' opacities (and 0 if
/// any ancestor group is hidden). Photoshop propagates group opacity/visibility
/// down to children; `nodes_in_draw_order` flattens groups to leaves and drops
/// that context, so we recover it here and fold it into the rendered alpha.
fn group_opacity_map(
    doc: &Document,
) -> std::collections::HashMap<photonic_core::node::NodeId, f32> {
    use std::collections::HashMap;
    let mut parent: HashMap<photonic_core::node::NodeId, photonic_core::node::NodeId> =
        HashMap::new();
    for n in doc.nodes.values() {
        if let SceneNodeKind::Group(g) = &n.kind {
            for c in &g.children {
                parent.insert(*c, n.id);
            }
        }
    }
    let mut out = HashMap::new();
    for id in doc.nodes.keys() {
        let mut op = 1.0f32;
        let mut cur = *id;
        let mut guard = 0;
        while let Some(p) = parent.get(&cur) {
            if let Some(pn) = doc.nodes.get(p) {
                if !pn.visible {
                    op = 0.0;
                }
                op *= pn.opacity;
            }
            cur = *p;
            guard += 1;
            if guard > 64 {
                break;
            }
        }
        out.insert(*id, op);
    }
    out
}

/// One blurred effect to render into the offscreen effects layer (composited
/// beneath the sharp document): geometry already transformed into document
/// space, plus the blur radius in document units (scaled by zoom at render time).
pub(crate) struct BlurJob {
    verts: Vec<Vertex>,
    idxs: Vec<u32>,
    radius_doc: f64,
}

/// Tessellate `path`'s fill, transform it by `m` (+ `offset`), flat-color it,
/// and package it as a [`BlurJob`]. Returns `None` for empty geometry.
fn silhouette_job(
    path: &photonic_core::path::PathData,
    m: &[f64; 6],
    offset: (f64, f64),
    color: [f32; 4],
    radius_doc: f64,
    view_scale: f64,
) -> Option<BlurJob> {
    let mesh = tessellate_fill(path, false, adaptive_tolerance(view_scale, m));
    if mesh.is_empty() {
        return None;
    }
    let [a, b, c, d, e, f] = *m;
    let (ox, oy) = offset;
    let mut verts = Vec::with_capacity(mesh.vertices.len());
    for pos in &mesh.vertices {
        let x = a * pos[0] as f64 + c * pos[1] as f64 + e + ox;
        let y = b * pos[0] as f64 + d * pos[1] as f64 + f + oy;
        verts.push(Vertex {
            position: [x as f32, y as f32],
            color,
        });
    }
    Some(BlurJob {
        verts,
        idxs: mesh.indices,
        radius_doc,
    })
}

/// Whether `document` must be composited on the CPU rather than the pure-GPU
/// vector path. True for documents containing raster (pixel) layers, pattern
/// fills, a layer that isolates (opacity < 1 or non-Normal blend), a non-print
/// layer, or an enabled layer-style effect stack. Pure-vector documents
/// (predicate `false`) can take the GPU fast path — and the §2.5 Tier B
/// GPU-to-GPU working-texture path.
///
/// The single source of truth for this routing decision, reused by
/// [`HeadlessRenderer::render_rgba_with_opts`] (Tier A / CPU fallback) and
/// [`HeadlessRenderer::render_vector_to_working_texture`] (Tier B) and imported
/// by `photonic-video` rather than re-derived (03 §2.5).
pub fn document_needs_cpu_compositor(document: &Document) -> bool {
    let has_raster = document
        .nodes
        .values()
        .any(|n| matches!(&n.kind, SceneNodeKind::Raster(_)));
    let has_pattern = document.nodes.values().any(|n| {
        matches!(
            &n.kind,
            SceneNodeKind::Path(pn)
                if matches!(pn.fill.kind, photonic_core::style::FillKind::Pattern(_))
        )
    });
    // A layer with opacity < 1 or a non-Normal blend mode must composite as an
    // isolated unit (P0); the CPU compositor does that per layer.
    let has_isolated_layer = document.layer_order.iter().any(|lid| {
        document.layers.get(lid).is_some_and(|l| {
            l.visible
                && !l.node_ids.is_empty()
                && (l.opacity < 1.0 || l.blend_mode != BlendMode::Normal)
        })
    });
    // Non-print layers must be excluded from export.
    let has_non_print_layer = document.layer_order.iter().any(|lid| {
        document
            .layers
            .get(lid)
            .is_some_and(|l| !l.print && l.visible && !l.node_ids.is_empty())
    });
    // Layer Styles (the effect stack) render on the CPU compositor path.
    let has_stack_effects = document
        .nodes
        .values()
        .any(|n| n.effects.iter().any(|e| e.enabled()));
    has_raster || has_pattern || has_isolated_layer || has_non_print_layer || has_stack_effects
}

pub(crate) fn build_geometry(
    doc: &Document,
    include_artboard_bg: bool,
    overprint_preview: bool,
    view_scale: f64,
) -> (Vec<Vertex>, Vec<u32>, Vec<DrawSegment>, Vec<BlurJob>) {
    let mut verts: Vec<Vertex> = Vec::new();
    let mut idxs: Vec<u32> = Vec::new();
    let eff = group_opacity_map(doc);
    // Per-node index ranges tagged with their blend mode, coalesced at the end.
    let mut raw_segments: Vec<(BlendMode, u32, u32)> = Vec::new();
    let mut blur_jobs: Vec<BlurJob> = Vec::new();

    // Overprint preview (#22): canonicalised hexes of the overprint-flagged spot
    // colours. A node's solid fill matching one composites with Multiply (below).
    let overprint_hexes: std::collections::HashSet<String> = if overprint_preview {
        doc.spot_colors
            .iter()
            .filter(|s| s.overprint)
            .filter_map(|s| Color::from_hex(&s.hex).map(|c| c.to_hex()))
            .collect()
    } else {
        std::collections::HashSet::new()
    };

    // Optional white artboard rectangle (always first 4 vertices when present).
    if include_artboard_bg {
        let (w, h) = (doc.width as f32, doc.height as f32);
        let white = [1.0f32, 1.0, 1.0, 1.0];
        let base = verts.len() as u32;
        verts.extend_from_slice(&[
            Vertex {
                position: [0.0, 0.0],
                color: white,
            },
            Vertex {
                position: [w, 0.0],
                color: white,
            },
            Vertex {
                position: [w, h],
                color: white,
            },
            Vertex {
                position: [0.0, h],
                color: white,
            },
        ]);
        idxs.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        raw_segments.push((BlendMode::Normal, 0, idxs.len() as u32));
    }

    for node in doc.nodes_in_draw_order() {
        let nid = node.id;
        // Resolve symbol instances to the current master (+ overrides) so
        // headless export matches the live renderer.
        let resolved = doc.resolve_render_node(node);
        let node = resolved.as_ref();
        let SceneNodeKind::Path(path_node) = &node.kind else {
            continue;
        };
        let gop = eff.get(&nid).copied().unwrap_or(1.0);
        if gop <= 0.0 {
            continue;
        }
        let seg_start = idxs.len() as u32;
        let [a, b, c, d, e, f] = node.transform.matrix;

        // ── Drop shadow → blurred offset silhouette in the effects layer ───────
        if node.drop_shadow.enabled {
            let s = &node.drop_shadow;
            let alpha = (s.color.a * s.opacity * node.opacity * gop).min(1.0);
            if let Some(job) = silhouette_job(
                &path_node.path_data,
                &node.transform.matrix,
                (s.dx as f64, s.dy as f64),
                [s.color.r, s.color.g, s.color.b, alpha],
                s.blur as f64,
                view_scale,
            ) {
                blur_jobs.push(job);
            }
        }

        // ── Object blur / feather → blurred fill in the effects layer ──────────
        // For solid fills the sharp fill is suppressed and replaced by a true
        // Gaussian-blurred copy. Gradient/image interior blur is a follow-up.
        let blur_radius = if node.object_blur.enabled {
            node.object_blur.radius
        } else if node.feather.enabled {
            node.feather.radius
        } else {
            0.0
        };
        let mut fill_blurred = false;
        if blur_radius > 0.0 {
            if let FillKind::Solid(col) = &path_node.fill.kind {
                let alpha = col.a * path_node.fill.opacity * node.opacity * gop;
                if let Some(job) = silhouette_job(
                    &path_node.path_data,
                    &node.transform.matrix,
                    (0.0, 0.0),
                    [col.r, col.g, col.b, alpha],
                    blur_radius as f64,
                    view_scale,
                ) {
                    blur_jobs.push(job);
                    fill_blurred = true;
                }
            }
        }

        // ── Fill (skipped when replaced by a blurred copy) ─────────────────────
        if !fill_blurred
            && path_node.fill.enabled
            && !matches!(&path_node.fill.kind, FillKind::None)
        {
            let opacity = path_node.fill.opacity * node.opacity * gop;
            let mesh = tessellate_fill(
                &path_node.path_data,
                path_node.is_compound,
                adaptive_tolerance(view_scale, &node.transform.matrix),
            );
            if !mesh.is_empty() {
                // Non-linear fills sample per vertex → refine the triangulation.
                let mesh = if path_node.fill.kind.is_nonlinear() {
                    let (mut lx, mut ly, mut lxx, mut lyy) =
                        (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
                    for p in &mesh.vertices {
                        lx = lx.min(p[0] as f64);
                        ly = ly.min(p[1] as f64);
                        lxx = lxx.max(p[0] as f64);
                        lyy = lyy.max(p[1] as f64);
                    }
                    let maxdim = ((lxx - lx).max(lyy - ly)) as f32;
                    crate::tessellator::refine_mesh(&mesh, (maxdim / 48.0).max(1.0))
                } else {
                    mesh
                };
                // Object-space gradients resolve against the fill's bbox; the
                // rotation-following variant resolves in local space.
                let rotated = path_node.fill.kind.gradient_follows_rotation();
                let (mut minx, mut miny, mut maxx, mut maxy) =
                    (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
                for p in &mesh.vertices {
                    let (x, y) = if rotated {
                        (p[0] as f64, p[1] as f64)
                    } else {
                        (
                            a * p[0] as f64 + c * p[1] as f64 + e,
                            b * p[0] as f64 + d * p[1] as f64 + f,
                        )
                    };
                    minx = minx.min(x);
                    miny = miny.min(y);
                    maxx = maxx.max(x);
                    maxy = maxy.max(y);
                }
                let fill_kind = path_node
                    .fill
                    .kind
                    .for_bbox(minx, miny, maxx - minx, maxy - miny);

                // Mesh fills: cut along grid lines for clean cell boundaries.
                if let FillKind::MeshGradient(mg) = &*fill_kind {
                    let interior = |lines: &[f64]| -> Vec<f64> {
                        if lines.len() > 2 {
                            lines[1..lines.len() - 1].to_vec()
                        } else {
                            Vec::new()
                        }
                    };
                    let xs = interior(&mg.x_lines);
                    let ys = interior(&mg.y_lines);
                    let sc = |p: &[f32; 2]| -> [f64; 2] {
                        if rotated {
                            [p[0] as f64, p[1] as f64]
                        } else {
                            [
                                a * p[0] as f64 + c * p[1] as f64 + e,
                                b * p[0] as f64 + d * p[1] as f64 + f,
                            ]
                        }
                    };
                    let mut tris: Vec<[[f64; 2]; 3]> = mesh
                        .indices
                        .chunks_exact(3)
                        .map(|t| {
                            [
                                sc(&mesh.vertices[t[0] as usize]),
                                sc(&mesh.vertices[t[1] as usize]),
                                sc(&mesh.vertices[t[2] as usize]),
                            ]
                        })
                        .collect();
                    crate::tessellator::cut_triangles(&mut tris, &xs, &ys);
                    for tri in &tris {
                        let cx = (tri[0][0] + tri[1][0] + tri[2][0]) / 3.0;
                        let cy = (tri[0][1] + tri[1][1] + tri[2][1]) / 3.0;
                        let base = verts.len() as u32;
                        for p in tri {
                            let color = fill_kind.sample_at(
                                p[0] + (cx - p[0]) * 0.02,
                                p[1] + (cy - p[1]) * 0.02,
                                opacity,
                            );
                            let (wx, wy) = if rotated {
                                (a * p[0] + c * p[1] + e, b * p[0] + d * p[1] + f)
                            } else {
                                (p[0], p[1])
                            };
                            verts.push(Vertex {
                                position: [wx as f32, wy as f32],
                                color,
                            });
                        }
                        idxs.extend_from_slice(&[base, base + 1, base + 2]);
                    }
                } else {
                    let base = verts.len() as u32;
                    for pos in &mesh.vertices {
                        let x = a * pos[0] as f64 + c * pos[1] as f64 + e;
                        let y = b * pos[0] as f64 + d * pos[1] as f64 + f;
                        let (sx, sy) = if rotated {
                            (pos[0] as f64, pos[1] as f64)
                        } else {
                            (x, y)
                        };
                        let color = fill_kind.sample_at(sx, sy, opacity);
                        verts.push(Vertex {
                            position: [x as f32, y as f32],
                            color,
                        });
                    }
                    for &i in &mesh.indices {
                        idxs.push(base + i);
                    }
                }
            }
        }

        // ── Stroke ───────────────────────────────────────────────────────────
        if path_node.stroke.enabled && path_node.stroke.width > 0.0 {
            let sc = &path_node.stroke;
            let alpha = sc.color.a * sc.opacity * node.opacity * gop;
            let stroke_color = [sc.color.r, sc.color.g, sc.color.b, alpha];
            // Gradient/pattern stroke paint (#201): sample the paint per stroke
            // vertex, exactly as the fill path does. `None` = flat stroke color.
            let stroke_paint_opacity = sc.opacity * node.opacity * gop;

            // Non-scaling stroke: cancel the object transform's uniform scale so
            // the stroke width stays constant regardless of object size, exactly
            // as the live renderer does (see `renderer.rs`). No-op when det == 1.
            let obj_scale = (a * d - b * c).abs().sqrt().max(1e-6);
            let mesh = tessellate_stroke(
                &path_node.path_data,
                (sc.width / obj_scale) as f32,
                sc.line_cap,
                sc.line_join,
                sc.miter_limit as f32,
                adaptive_tolerance(view_scale, &node.transform.matrix),
            );
            if !mesh.is_empty() {
                let base = verts.len() as u32;
                for pos in &mesh.vertices {
                    let x = a * pos[0] as f64 + c * pos[1] as f64 + e;
                    let y = b * pos[0] as f64 + d * pos[1] as f64 + f;
                    let color = match &sc.paint {
                        Some(kind) => kind.sample_at(x, y, stroke_paint_opacity),
                        None => stroke_color,
                    };
                    verts.push(Vertex {
                        position: [x as f32, y as f32],
                        color,
                    });
                }
                for &i in &mesh.indices {
                    idxs.push(base + i);
                }
            }
        }

        // Overprint preview: a (crisp) solid fill matching an overprint-flagged
        // spot ink composites with Multiply instead of its own knockout blend.
        // Skip when the fill is blurred (object-blur/feather) — that fill is
        // suppressed to the effects layer, so this segment is only the stroke and
        // must not be forced to Multiply.
        let mut blend = node.blend_mode;
        if !overprint_hexes.is_empty() && !fill_blurred && path_node.fill.enabled {
            if let FillKind::Solid(col) = &path_node.fill.kind {
                if overprint_hexes.contains(&col.to_hex()) {
                    blend = BlendMode::Multiply;
                }
            }
        }
        raw_segments.push((blend, seg_start, idxs.len() as u32));
    }

    let segments = coalesce_segments(raw_segments);
    (verts, idxs, segments, blur_jobs)
}

fn align256(n: u32) -> u32 {
    (n + 255) & !255
}

// ─── Raster layer compositing ───────────────────────────────────────────────────

/// Composite every visible `Raster` node over the rendered `pixels` buffer
/// (RGBA8, `w`×`h`), aligned through the same `view` the GPU pass used.
///
/// Each output pixel is inverse-mapped through the camera and the node's affine
/// transform into the image's local pixel space, bilinearly sampled, then
/// source-over composited with the node's opacity, blend mode, and layer mask.
pub(crate) fn composite_raster_nodes(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    doc: &Document,
    view: &CanvasView,
) {
    let eff = group_opacity_map(doc);
    for node in doc.nodes_in_draw_order() {
        let nid = node.id;
        let resolved = doc.resolve_render_node(node);
        let node = resolved.as_ref();
        let SceneNodeKind::Raster(rn) = &node.kind else {
            continue;
        };
        let gop = eff.get(&nid).copied().unwrap_or(1.0);
        let node_opacity = (node.opacity * gop).clamp(0.0, 1.0);
        if node_opacity <= 0.0 {
            continue;
        }

        // ── Non-destructive adjustment layer ─────────────────────────────────
        // Re-applies its adjustment to the composite of everything beneath it,
        // blended back by the layer's opacity (the adjustment "strength") and,
        // when present, gated by the layer's (document-space) mask.
        if let Some(spec) = &rn.adjustment {
            let Ok(mut buf) =
                photonic_core::raster::image::RasterImage::from_rgba(w, h, pixels.to_vec())
            else {
                continue;
            };
            spec.apply(&mut buf, None);
            let mask = rn.mask.as_ref();
            for py in 0..h {
                for px in 0..w {
                    let mut amt = node_opacity;
                    if let Some(m) = mask {
                        // Output pixel → canvas (document) coords → mask sample.
                        let (cx, cy) = view.screen_to_canvas(px as f64 + 0.5, py as f64 + 0.5);
                        if doc.width > 0.0 && doc.height > 0.0 {
                            let mx = cx / doc.width * m.width as f64;
                            let my = cy / doc.height * m.height as f64;
                            if mx < 0.0 || my < 0.0 || mx >= m.width as f64 || my >= m.height as f64
                            {
                                amt = 0.0;
                            } else {
                                amt *= m.coverage(mx as u32, my as u32);
                            }
                        }
                    }
                    if amt <= 0.0 {
                        continue;
                    }
                    let i = ((py * w + px) * 4) as usize;
                    for c in 0..4 {
                        let orig = pixels[i + c] as f32;
                        let adj = buf.pixels[i + c] as f32;
                        pixels[i + c] = (orig + (adj - orig) * amt).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
            continue;
        }

        let img = &rn.image;
        if img.width == 0 || img.height == 0 {
            continue;
        }
        let affine = node.transform.to_kurbo();
        let inv = affine.inverse();

        // Screen-space AABB of the transformed image rect, to bound iteration.
        let corners = [
            (0.0, 0.0),
            (img.width as f64, 0.0),
            (img.width as f64, img.height as f64),
            (0.0, img.height as f64),
        ];
        let (mut min_x, mut min_y) = (f64::MAX, f64::MAX);
        let (mut max_x, mut max_y) = (f64::MIN, f64::MIN);
        for (lx, ly) in corners {
            let (dx, dy) = node.transform.apply(lx, ly);
            let (sx, sy) = view.canvas_to_screen(dx, dy);
            min_x = min_x.min(sx);
            min_y = min_y.min(sy);
            max_x = max_x.max(sx);
            max_y = max_y.max(sy);
        }
        let x0 = (min_x.floor() as i64).max(0);
        let y0 = (min_y.floor() as i64).max(0);
        let x1 = (max_x.ceil() as i64).min(w as i64);
        let y1 = (max_y.ceil() as i64).min(h as i64);

        for py in y0..y1 {
            for px in x0..x1 {
                let (dx, dy) = view.screen_to_canvas(px as f64 + 0.5, py as f64 + 0.5);
                let lp = inv * kurbo::Point::new(dx, dy);
                if lp.x < 0.0 || lp.y < 0.0 || lp.x >= img.width as f64 || lp.y >= img.height as f64
                {
                    continue;
                }
                let s = img.sample_bilinear(lp.x as f32 - 0.5, lp.y as f32 - 0.5);
                let mut sa = (s[3] as f32 / 255.0) * node_opacity;
                if let Some(mask) = &rn.mask {
                    sa *= mask.coverage(lp.x as u32, lp.y as u32);
                }
                if sa <= 0.0 {
                    continue;
                }

                let idx = ((py as u32 * w + px as u32) * 4) as usize;
                let b = [
                    pixels[idx] as f32 / 255.0,
                    pixels[idx + 1] as f32 / 255.0,
                    pixels[idx + 2] as f32 / 255.0,
                ];
                let ba = pixels[idx + 3] as f32 / 255.0;
                let cs = [
                    s[0] as f32 / 255.0,
                    s[1] as f32 / 255.0,
                    s[2] as f32 / 255.0,
                ];

                let blended = blend_rgb(node.blend_mode, b, cs);
                let mixed = [
                    (1.0 - ba) * cs[0] + ba * blended[0],
                    (1.0 - ba) * cs[1] + ba * blended[1],
                    (1.0 - ba) * cs[2] + ba * blended[2],
                ];
                let oa = sa + ba * (1.0 - sa);
                if oa > 0.0 {
                    for c in 0..3 {
                        let co = (mixed[c] * sa + b[c] * ba * (1.0 - sa)) / oa;
                        pixels[idx + c] = (co * 255.0).round().clamp(0.0, 255.0) as u8;
                    }
                }
                pixels[idx + 3] = (oa * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

#[cfg(test)]
mod blend_tests {
    use super::*;
    use photonic_core::{
        color::Color,
        node::{GroupNode, PathNode, SceneNode, SceneNodeKind},
        path::PathData,
        style::Fill,
        Document,
    };

    /// sRGB (0–1) → linear, matching the hardware decode for an `Rgba8UnormSrgb`
    /// render target so we can compare read-back bytes against linear blend math.
    fn srgb_to_linear(c: f32) -> f32 {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    /// Returns Some(renderer) if a GPU adapter is available, else None so the
    /// test can skip on headless CI without a GPU.
    fn try_renderer() -> Option<HeadlessRenderer> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))?;
        Some(pollster::block_on(HeadlessRenderer::new()))
    }

    /// Decode an IEEE-754 half (`Rgba16Float` storage) to `f32`. Inline to avoid
    /// pulling `half` in as a direct dependency for one test.
    fn f16_to_f32(bits: u16) -> f32 {
        let sign = ((bits >> 15) & 1) as u32;
        let exp = ((bits >> 10) & 0x1f) as i32;
        let frac = (bits & 0x3ff) as u32;
        let v = if exp == 0 {
            (frac as f32) * 2f32.powi(-24)
        } else if exp == 0x1f {
            if frac == 0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        } else {
            (1.0 + frac as f32 / 1024.0) * 2f32.powi(exp - 15)
        };
        if sign == 1 {
            -v
        } else {
            v
        }
    }

    /// Read back an `Rgba16Float` texture as linear `f32` RGBA.
    fn readback_working(r: &HeadlessRenderer, tex: &wgpu::Texture, w: u32, h: u32) -> Vec<f32> {
        let bpr = align256(w * 8); // 4 channels × 2 bytes
        let staging = r.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("working_staging"),
            size: (bpr * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = r.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::ImageCopyBuffer {
                buffer: &staging,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        r.queue.submit([enc.finish()]);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |x| {
            let _ = tx.send(x);
        });
        r.device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();
        let raw = slice.get_mapped_range();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h {
            let start = (row * bpr) as usize;
            for px in raw[start..start + (w * 8) as usize].chunks_exact(2) {
                out.push(f16_to_f32(u16::from_le_bytes([px[0], px[1]])));
            }
        }
        drop(raw);
        staging.unmap();
        out
    }

    /// 03 §2.5 / §4.4 rule 4: the Tier B GPU-to-GPU vector→working path must
    /// produce the same premultiplied-linear pixels as Tier A (render to RGBA8,
    /// upload, convert) for a pure-vector document. Uses overlapping partial-alpha
    /// fills on a transparent background so both premultiplication and edge alpha
    /// are exercised.
    #[test]
    fn tier_a_and_tier_b_working_textures_match() {
        let Some(r) = try_renderer() else {
            eprintln!("no GPU adapter — skipping Tier A/B equivalence test");
            return;
        };
        let (w, h) = (48u32, 48u32);
        let mut doc = Document::new("tierab", w as f64, h as f64);
        doc.add_node(
            SceneNode::new(
                "bg-rect",
                doc.active_layer_id.unwrap(),
                SceneNodeKind::Path(
                    PathNode::new(PathData::rect(4.0, 4.0, 40.0, 40.0))
                        .with_fill(Fill::solid(Color::new(0.9, 0.3, 0.2, 0.6))),
                ),
            ),
            None,
        );
        doc.add_node(
            SceneNode::new(
                "fg-rect",
                doc.active_layer_id.unwrap(),
                SceneNodeKind::Path(
                    PathNode::new(PathData::rect(16.0, 16.0, 24.0, 24.0))
                        .with_fill(Fill::solid(Color::new(0.2, 0.5, 0.95, 0.8))),
                ),
            ),
            None,
        );
        assert!(
            !crate::document_needs_cpu_compositor(&doc),
            "fixture must be a pure-vector document (Tier B eligible)"
        );

        // Tier B: GPU-to-GPU.
        let tier_b = r.render_vector_to_working_texture(&doc, w, h);
        let b = readback_working(&r, &tier_b, w, h);

        // Tier A: render to RGBA8 (transparent bg), upload as Rgba8Unorm, convert.
        let opts = ExportOptions {
            background: ExportBackground::Transparent,
            ..ExportOptions::default()
        };
        let (rgba, _, _) = r.render_rgba_with_opts(&doc, w, h, &opts);
        let src = r.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tierA_src"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm, // raw bytes, no hardware decode
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        r.queue.write_texture(
            src.as_image_copy(),
            &rgba,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        let src_view = src.create_view(&Default::default());
        let tier_a = r.convert_srgb_texture_to_working(&src_view, w, h);
        let a = readback_working(&r, &tier_a, w, h);

        assert_eq!(a.len(), b.len());
        let mut max_diff = 0.0f32;
        for (x, y) in a.iter().zip(&b) {
            max_diff = max_diff.max((x - y).abs());
        }
        assert!(
            max_diff < 1e-3,
            "Tier A vs Tier B max linear diff {max_diff:.5} exceeds 1e-3"
        );
    }

    #[test]
    fn raster_export_preserves_compound_path_cutouts() {
        let Some(renderer) = try_renderer() else {
            eprintln!("no GPU adapter — skipping compound raster export test");
            return;
        };
        let mut document = Document::new("compound", 64.0, 64.0);
        let layer_id = document.active_layer_id.unwrap();
        let data = PathData::from_svg(
            "M8 8 L56 8 L56 56 L8 56 Z \
             M18 18 L28 18 L28 28 L18 28 Z \
             M36 36 L46 36 L46 46 L36 46 Z",
        )
        .unwrap();
        let mut path = PathNode::new(data).with_fill(Fill::solid(Color::BLACK));
        path.is_compound = true;
        document.add_node(
            SceneNode::new("compound", layer_id, SceneNodeKind::Path(path)),
            None,
        );

        let png = renderer.render_png_with_opts(
            &document,
            64,
            64,
            &ExportOptions {
                background: ExportBackground::Transparent,
                ..ExportOptions::default()
            },
        );
        let image = image::load_from_memory(&png).unwrap().to_rgba8();
        let outer = image.get_pixel(10, 10).0;
        let hole_1 = image.get_pixel(23, 23).0;
        let hole_2 = image.get_pixel(41, 41).0;
        assert!(outer[3] > 0, "outer contour is filled: {outer:?}");
        assert_eq!(hole_1[3], 0, "first cutout is transparent: {hole_1:?}");
        assert_eq!(hole_2[3], 0, "second cutout is transparent: {hole_2:?}");
    }

    // Backdrop and source fills chosen so every separable mode yields a distinct
    // colour (avoids primaries where Multiply==Darken etc.). Values are linear.
    const BACKDROP: [f32; 3] = [0.8, 0.4, 0.2];
    const SOURCE: [f32; 3] = [0.3, 0.6, 0.9];

    /// Build a 100×100 doc: full-artboard backdrop rect (Normal) + a centred
    /// 50×50 source rect with `mode`, and read back the centre overlap pixel as
    /// linear RGB.
    fn render_center_pixel(r: &HeadlessRenderer, mode: BlendMode) -> [f32; 3] {
        let mut doc = Document::new("blend-test", 100.0, 100.0);

        let backdrop = SceneNode::new(
            "backdrop",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(0.0, 0.0, 100.0, 100.0)).with_fill(Fill::solid(
                    Color::new(BACKDROP[0], BACKDROP[1], BACKDROP[2], 1.0),
                )),
            ),
        );
        doc.add_node(backdrop, None);

        let mut source = SceneNode::new(
            "source",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(25.0, 25.0, 50.0, 50.0)).with_fill(Fill::solid(
                    Color::new(SOURCE[0], SOURCE[1], SOURCE[2], 1.0),
                )),
            ),
        );
        source.blend_mode = mode;
        doc.add_node(source, None);

        let png = r.render_png_at_size(&doc, 100, 100);
        let img = image::load_from_memory(&png)
            .expect("decode png")
            .to_rgba8();
        let px = img.get_pixel(50, 50).0;
        [
            srgb_to_linear(px[0] as f32 / 255.0),
            srgb_to_linear(px[1] as f32 / 255.0),
            srgb_to_linear(px[2] as f32 / 255.0),
        ]
    }

    #[test]
    fn export_geometry_uses_fitted_view_scale_for_curve_tessellation() {
        let mut doc = Document::new("zoomed-export", 200.0, 200.0);
        let node = SceneNode::new(
            "circle",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(
                PathNode::new(PathData::ellipse(100.0, 100.0, 80.0, 80.0))
                    .with_fill(Fill::solid(Color::new(0.0, 0.0, 0.0, 1.0))),
            ),
        );
        doc.add_node(node, None);

        let (_, one_x_indices, _, _) = build_geometry(&doc, false, false, 1.0);
        let (_, high_zoom_indices, _, _) = build_geometry(&doc, false, false, 32.0);

        assert!(
            high_zoom_indices.len() > one_x_indices.len(),
            "high-resolution export should tessellate curves more densely: {} <= {}",
            high_zoom_indices.len(),
            one_x_indices.len()
        );
    }

    fn expected(mode: BlendMode) -> [f32; 3] {
        let mut out = [0.0; 3];
        for i in 0..3 {
            let (b, s) = (BACKDROP[i], SOURCE[i]);
            out[i] = match mode {
                BlendMode::Multiply => s * b,
                BlendMode::Screen => s + b - s * b,
                BlendMode::Darken => s.min(b),
                BlendMode::Lighten => s.max(b),
                _ => unreachable!("only separable modes tested"),
            };
        }
        out
    }

    #[test]
    fn separable_blend_modes_match_reference() {
        let Some(r) = try_renderer() else {
            eprintln!("no GPU adapter — skipping blend-mode golden test");
            return;
        };
        // Generous tolerance absorbs 8-bit quantisation and the sRGB round-trip.
        const TOL: f32 = 0.03;
        for mode in SEPARABLE_BLEND_MODES {
            let got = render_center_pixel(&r, mode);
            let want = expected(mode);
            for i in 0..3 {
                assert!(
                    (got[i] - want[i]).abs() < TOL,
                    "{mode:?} channel {i}: got {:.3}, want {:.3}",
                    got[i],
                    want[i],
                );
            }
        }
    }

    /// The isolation-compositing path (03 §2.4) produces the correct blend for
    /// modes fixed-function blending can't express. Difference `|Cb-Cs|` and
    /// Exclusion `Cb+Cs-2·Cb·Cs` have unambiguous per-channel formulas and force
    /// `segments_need_isolation` true, so this exercises `record_pass_isolated` +
    /// `COMPOSITE_SHADER` end to end. Both are evaluated in linear light (the
    /// sRGB SCENE_FORMAT round-trip), matching the fixed-function separable path.
    #[test]
    fn nonseparable_blend_modes_match_reference() {
        let Some(r) = try_renderer() else {
            eprintln!("no GPU adapter — skipping non-separable blend test");
            return;
        };
        const TOL: f32 = 0.03;
        for mode in [BlendMode::Difference, BlendMode::Exclusion] {
            let got = render_center_pixel(&r, mode);
            for i in 0..3 {
                let (b, s) = (BACKDROP[i], SOURCE[i]);
                let want = match mode {
                    BlendMode::Difference => (b - s).abs(),
                    BlendMode::Exclusion => b + s - 2.0 * b * s,
                    // Unreachable: the enclosing loop only iterates over
                    // `[Difference, Exclusion]`.
                    _ => unreachable!(),
                };
                assert!(
                    (got[i] - want).abs() < TOL,
                    "{mode:?} channel {i}: got {:.3}, want {:.3}",
                    got[i],
                    want,
                );
            }
        }
    }

    #[test]
    fn normal_mode_shows_source_unblended() {
        let Some(r) = try_renderer() else {
            eprintln!("no GPU adapter — skipping normal-mode test");
            return;
        };
        // Normal mode: opaque source fully replaces the backdrop at the overlap.
        let got = render_center_pixel(&r, BlendMode::Normal);
        for i in 0..3 {
            assert!(
                (got[i] - SOURCE[i]).abs() < 0.03,
                "Normal channel {i}: got {:.3}, want {:.3}",
                got[i],
                SOURCE[i],
            );
        }
    }

    /// Two-layer doc: full-canvas RED on the base layer, full-canvas BLUE on a
    /// second layer whose opacity/blend the caller sets. Returns the centre pixel.
    fn layer_blend_center(r: &HeadlessRenderer, opacity: f32, mode: BlendMode) -> [u8; 4] {
        let mut doc = Document::new("lb", 20.0, 20.0);
        doc.add_node(
            SceneNode::new(
                "red",
                doc.active_layer_id.unwrap(),
                SceneNodeKind::Path(
                    PathNode::new(PathData::rect(0.0, 0.0, 20.0, 20.0))
                        .with_fill(Fill::solid(Color::new(1.0, 0.0, 0.0, 1.0))),
                ),
            ),
            None,
        );
        let top = doc.add_layer(photonic_core::layer::Layer::new("top"));
        {
            let l = doc.layers.get_mut(&top).unwrap();
            l.opacity = opacity;
            l.blend_mode = mode;
        }
        doc.add_node(
            SceneNode::new(
                "blue",
                Default::default(),
                SceneNodeKind::Path(
                    PathNode::new(PathData::rect(0.0, 0.0, 20.0, 20.0))
                        .with_fill(Fill::solid(Color::new(0.0, 0.0, 1.0, 1.0))),
                ),
            ),
            Some(top),
        );
        let png = r.render_png_at_size(&doc, 20, 20);
        let img = image::load_from_memory(&png).expect("png").to_rgba8();
        img.get_pixel(10, 10).0
    }

    /// P0 (headless): a Multiply blend set on a whole LAYER composites the layer
    /// as a unit against the layer beneath. Pure primaries → space-independent
    /// (sRGB == linear), so RED × BLUE = black regardless of blend space.
    #[test]
    fn layer_multiply_composites_as_a_unit() {
        let Some(r) = try_renderer() else {
            eprintln!("no GPU adapter — skipping layer-multiply test");
            return;
        };
        let p = layer_blend_center(&r, 1.0, BlendMode::Multiply);
        assert!(
            p[0] < 24 && p[1] < 24 && p[2] < 24,
            "RED × BLUE via a Multiply layer should be ~black, got {p:?}"
        );
    }

    /// P0 (headless): a half-opacity Normal layer blends 50/50 over the layer
    /// beneath → ~purple (the previously-dead Layer.opacity now takes effect).
    #[test]
    fn layer_half_opacity_via_headless() {
        let Some(r) = try_renderer() else {
            eprintln!("no GPU adapter — skipping layer-opacity test");
            return;
        };
        let p = layer_blend_center(&r, 0.5, BlendMode::Normal);
        assert!(
            (p[0] as i32 - 127).abs() < 32 && p[1] < 32 && (p[2] as i32 - 127).abs() < 32,
            "half-opacity blue over red should be ~purple, got {p:?}"
        );
    }

    #[test]
    fn group_opacity_is_not_applied_twice_to_css_style_background_path() {
        let Some(r) = try_renderer() else {
            eprintln!("no GPU adapter — skipping group-opacity test");
            return;
        };
        let mut doc = Document::new("css-opacity", 20.0, 20.0);
        let layer = doc.active_layer_id.unwrap();
        doc.add_node(
            SceneNode::new(
                "backdrop",
                layer,
                SceneNodeKind::Path(
                    PathNode::new(PathData::rect(0.0, 0.0, 20.0, 20.0))
                        .with_fill(Fill::solid(Color::new(1.0, 0.0, 0.0, 1.0))),
                ),
            ),
            None,
        );
        let mut background = SceneNode::new(
            "element/background",
            layer,
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(0.0, 0.0, 20.0, 20.0))
                    .with_fill(Fill::solid(Color::new(0.0, 0.0, 1.0, 1.0))),
            ),
        );
        background.opacity = 1.0;
        let background_id = background.id;
        doc.add_node(background, None);
        let mut group = GroupNode::new();
        group.children.push(background_id);
        let mut element = SceneNode::new("element", layer, SceneNodeKind::Group(group));
        element.opacity = 0.5;
        doc.add_node(element, None);

        let png = r.render_png_at_size(&doc, 20, 20);
        let pixel = image::load_from_memory(&png)
            .unwrap()
            .to_rgba8()
            .get_pixel(10, 10)
            .0;
        // The render target is sRGB, so a 50% source-over blend is not 127 in
        // byte space. Double application would instead leave the pixel mostly
        // red (roughly 224/137 for red/blue).
        assert!(pixel[0] < 180 && pixel[2] > 180, "pixel: {pixel:?}");
    }

    /// P4 (headless): a Color Overlay layer style recolours the shape. A RED
    /// rect with an opaque BLUE Color Overlay exports BLUE.
    #[test]
    fn color_overlay_recolours_shape() {
        use photonic_core::effects::{ColorOverlay, LayerEffect};
        let Some(r) = try_renderer() else {
            eprintln!("no GPU adapter — skipping color-overlay test");
            return;
        };
        let mut doc = Document::new("co", 20.0, 20.0);
        let mut node = SceneNode::new(
            "rect",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(0.0, 0.0, 20.0, 20.0))
                    .with_fill(Fill::solid(Color::new(1.0, 0.0, 0.0, 1.0))),
            ),
        );
        node.effects.push(LayerEffect::ColorOverlay(ColorOverlay {
            enabled: true,
            color: Color::new(0.0, 0.0, 1.0, 1.0),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
        }));
        doc.add_node(node, None);

        let png = r.render_png_at_size(&doc, 20, 20);
        let img = image::load_from_memory(&png).expect("png").to_rgba8();
        let p = img.get_pixel(10, 10).0;
        assert!(
            p[2] > 200 && p[0] < 40,
            "opaque blue Color Overlay should recolour the red rect blue, got {p:?}"
        );
    }

    /// P4 (headless): a Stroke layer style paints an outline. A small RED rect
    /// centred in the canvas with a thick GREEN stroke → the rect's edge is green.
    #[test]
    fn stroke_effect_paints_outline() {
        use photonic_core::effects::{LayerEffect, StrokeEffect};
        use photonic_core::style::{Fill, StrokeAlign};
        let Some(r) = try_renderer() else {
            eprintln!("no GPU adapter — skipping stroke-effect test");
            return;
        };
        let mut doc = Document::new("st", 40.0, 40.0);
        let mut node = SceneNode::new(
            "rect",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(10.0, 10.0, 20.0, 20.0))
                    .with_fill(Fill::solid(Color::new(1.0, 0.0, 0.0, 1.0))),
            ),
        );
        node.effects.push(LayerEffect::Stroke(StrokeEffect {
            enabled: true,
            width: 6.0,
            position: StrokeAlign::Center,
            fill: Fill::solid(Color::new(0.0, 1.0, 0.0, 1.0)),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
        }));
        doc.add_node(node, None);

        let png = r.render_png_at_size(&doc, 40, 40);
        let img = image::load_from_memory(&png).expect("png").to_rgba8();
        // On the rect's edge (x=10, y=20): green stroke (centred → straddles edge).
        let edge = img.get_pixel(10, 20).0;
        // Interior (x=20, y=20): still red fill.
        let interior = img.get_pixel(20, 20).0;
        assert!(
            edge[1] > 180 && edge[0] < 80,
            "stroke edge should be green, got {edge:?}"
        );
        assert!(
            interior[0] > 180 && interior[1] < 80,
            "interior should stay red, got {interior:?}"
        );
    }

    /// P4 (headless): a Gradient Overlay recolours the shape with a gradient.
    /// A RED 20×20 rect with the default black→white overlay at angle 90°
    /// (top→bottom) → the top is dark and the bottom is light, and neither is red.
    #[test]
    fn gradient_overlay_recolours_with_gradient() {
        use photonic_core::effects::{GradientOverlay, LayerEffect};
        let Some(r) = try_renderer() else {
            eprintln!("no GPU adapter — skipping gradient-overlay test");
            return;
        };
        let mut doc = Document::new("go", 20.0, 20.0);
        let mut node = SceneNode::new(
            "rect",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(0.0, 0.0, 20.0, 20.0))
                    .with_fill(Fill::solid(Color::new(1.0, 0.0, 0.0, 1.0))),
            ),
        );
        node.effects
            .push(LayerEffect::GradientOverlay(GradientOverlay::default()));
        doc.add_node(node, None);

        let png = r.render_png_at_size(&doc, 20, 20);
        let img = image::load_from_memory(&png).expect("png").to_rgba8();
        let top = img.get_pixel(10, 3).0;
        let bottom = img.get_pixel(10, 17).0;
        assert!(
            top[0] < 90 && top[1] < 90 && top[2] < 90,
            "top of a top→bottom black→white overlay should be dark, got {top:?}"
        );
        assert!(
            bottom[0] > 170 && bottom[1] > 170 && bottom[2] > 170,
            "bottom should be light, got {bottom:?}"
        );
    }

    /// P7 (headless): a non-print layer stays off exports. A full-canvas BLUE
    /// non-print layer over a RED print layer → export shows RED.
    #[test]
    fn non_print_layer_excluded_from_export() {
        let Some(r) = try_renderer() else {
            eprintln!("no GPU adapter — skipping non-print test");
            return;
        };
        let mut doc = Document::new("np", 20.0, 20.0);
        doc.add_node(
            SceneNode::new(
                "red",
                doc.active_layer_id.unwrap(),
                SceneNodeKind::Path(
                    PathNode::new(PathData::rect(0.0, 0.0, 20.0, 20.0))
                        .with_fill(Fill::solid(Color::new(1.0, 0.0, 0.0, 1.0))),
                ),
            ),
            None,
        );
        let top = doc.add_layer(photonic_core::layer::Layer::new("top"));
        doc.layers.get_mut(&top).unwrap().print = false;
        doc.add_node(
            SceneNode::new(
                "blue",
                Default::default(),
                SceneNodeKind::Path(
                    PathNode::new(PathData::rect(0.0, 0.0, 20.0, 20.0))
                        .with_fill(Fill::solid(Color::new(0.0, 0.0, 1.0, 1.0))),
                ),
            ),
            Some(top),
        );
        let png = r.render_png_at_size(&doc, 20, 20);
        let img = image::load_from_memory(&png).expect("png").to_rgba8();
        let p = img.get_pixel(10, 10).0;
        assert!(
            p[0] > 200 && p[2] < 40,
            "non-print blue layer must be excluded from export (centre should be red), got {p:?}"
        );
    }

    /// P2 (headless): a Photoshop-extra mode (Linear Dodge / Add) exports
    /// correctly — RED ⊕ BLUE = magenta (per channel min(cb+cs, 1)).
    #[test]
    fn layer_linear_dodge_via_headless() {
        let Some(r) = try_renderer() else {
            eprintln!("no GPU adapter — skipping linear-dodge test");
            return;
        };
        let p = layer_blend_center(&r, 1.0, BlendMode::LinearDodge);
        assert!(
            p[0] > 200 && p[1] < 40 && p[2] > 200,
            "RED + BLUE via Linear Dodge should be ~magenta, got {p:?}"
        );
    }

    fn luma(px: [u8; 4]) -> f32 {
        (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32) / 255.0
    }

    #[test]
    fn hard_drop_shadow_appears_offset_and_darkens_backdrop() {
        let Some(r) = try_renderer() else {
            eprintln!("no GPU adapter — skipping drop-shadow test");
            return;
        };
        let mut doc = Document::new("ds", 100.0, 100.0);
        // White square at (30,30)-(70,70).
        let mut node = SceneNode::new(
            "sq",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(30.0, 30.0, 40.0, 40.0))
                    .with_fill(Fill::solid(Color::WHITE)),
            ),
        );
        // Hard black shadow offset down-right by (20,20).
        node.drop_shadow.enabled = true;
        node.drop_shadow.color = Color::new(0.0, 0.0, 0.0, 1.0);
        node.drop_shadow.opacity = 0.5;
        node.drop_shadow.dx = 20.0;
        node.drop_shadow.dy = 20.0;
        node.drop_shadow.blur = 0.0;
        doc.add_node(node, None);

        let png = r.render_png_at_size(&doc, 100, 100);
        let img = image::load_from_memory(&png).expect("png").to_rgba8();
        let at = |x, y| luma(img.get_pixel(x, y).0);

        // (80,80): inside shadow square (50-90) but outside fill (30-70) → darkened.
        let shadow = at(80, 80);
        // (50,50): inside the white fill → stays bright (fill drawn over shadow).
        let fill = at(50, 50);
        // (10,10): untouched white artboard.
        let bg = at(10, 10);

        assert!(bg > 0.9, "artboard should be white, got {bg}");
        assert!(fill > 0.9, "fill should be white, got {fill}");
        assert!(
            shadow < 0.8 && shadow > 0.2,
            "shadow region should be a mid-gray (got {shadow})",
        );
    }

    #[test]
    fn object_blur_softens_the_edge() {
        let Some(r) = try_renderer() else {
            eprintln!("no GPU adapter — skipping object-blur test");
            return;
        };
        let mut doc = Document::new("blur", 100.0, 100.0);
        let mut node = SceneNode::new(
            "sq",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(30.0, 30.0, 40.0, 40.0))
                    .with_fill(Fill::solid(Color::WHITE)),
            ),
        );
        node.object_blur.enabled = true;
        node.object_blur.radius = 8.0;
        doc.add_node(node, None);

        // Transparent background so the soft halo shows as partial coverage.
        let opts = ExportOptions {
            background: ExportBackground::Transparent,
            ..Default::default()
        };
        let png = r.render_png_with_opts(&doc, 100, 100, &opts);
        let img = image::load_from_memory(&png).expect("png").to_rgba8();
        // Square spans 30–70; just outside the right edge a hard fill would be
        // fully transparent — a soft edge gives partial coverage there.
        let halo = img.get_pixel(72, 50).0[3] as f32 / 255.0; // alpha, ~2px out
        let far = img.get_pixel(95, 50).0[3] as f32 / 255.0;
        let inside = img.get_pixel(50, 50).0[3] as f32 / 255.0;

        assert!(inside > 0.9, "fill interior should be opaque, got {inside}");
        assert!(
            halo > 0.03 && halo < 0.95,
            "edge should be partially covered (soft), got {halo}",
        );
        assert!(far < 0.05, "far outside should stay transparent, got {far}");
    }

    #[test]
    fn soft_drop_shadow_falls_off_gradually() {
        let Some(r) = try_renderer() else {
            eprintln!("no GPU adapter — skipping soft-shadow falloff test");
            return;
        };
        let mut doc = Document::new("soft", 100.0, 100.0);
        let mut node = SceneNode::new(
            "sq",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(20.0, 20.0, 30.0, 30.0))
                    .with_fill(Fill::solid(Color::WHITE)),
            ),
        );
        node.drop_shadow.enabled = true;
        node.drop_shadow.color = Color::new(0.0, 0.0, 0.0, 1.0);
        node.drop_shadow.opacity = 1.0;
        node.drop_shadow.dx = 0.0;
        node.drop_shadow.dy = 0.0;
        node.drop_shadow.blur = 10.0; // true gaussian
        doc.add_node(node, None);

        let opts = ExportOptions {
            background: ExportBackground::Transparent,
            ..Default::default()
        };
        let png = r.render_png_with_opts(&doc, 100, 100, &opts);
        let img = image::load_from_memory(&png).expect("png").to_rgba8();
        // Shadow alpha just outside the right edge (x=50), increasing distance.
        let a = |x: u32| img.get_pixel(x, 35).0[3] as f32 / 255.0;
        let near = a(53); // 3px out
        let mid = a(60); // 10px out
        let outer = a(66); // 16px out

        // A true Gaussian blur decays monotonically with distance; a hard edge
        // would jump to ~0 immediately.
        assert!(near > mid, "near ({near}) should exceed mid ({mid})");
        assert!(mid > outer, "mid ({mid}) should exceed outer ({outer})");
        assert!(
            near > 0.1,
            "shadow should be visible near the edge, got {near}"
        );
    }

    #[test]
    fn overprint_preview_multiplies_matching_spot_ink() {
        use photonic_core::SpotColor;
        let Some(r) = try_renderer() else {
            eprintln!("no GPU adapter — skipping overprint-preview test");
            return;
        };

        let mut doc = Document::new("overprint-test", 100.0, 100.0);
        let backdrop = SceneNode::new(
            "backdrop",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(0.0, 0.0, 100.0, 100.0)).with_fill(Fill::solid(
                    Color::new(BACKDROP[0], BACKDROP[1], BACKDROP[2], 1.0),
                )),
            ),
        );
        doc.add_node(backdrop, None);
        // Top shape keeps Normal blend; overprint must come from the spot match.
        let top_col = Color::new(SOURCE[0], SOURCE[1], SOURCE[2], 1.0);
        let top = SceneNode::new(
            "top",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(25.0, 25.0, 50.0, 50.0))
                    .with_fill(Fill::solid(top_col)),
            ),
        );
        doc.add_node(top, None);
        doc.spot_colors
            .push(SpotColor::new("Overprint Ink", top_col.to_hex(), true));

        let opts = ExportOptions {
            overprint_preview: true,
            ..ExportOptions::default()
        };
        let (pixels, w, _h) = r.render_rgba_with_opts(&doc, 100, 100, &opts);
        let i = ((50 * w + 50) * 4) as usize;
        let got = [
            srgb_to_linear(pixels[i] as f32 / 255.0),
            srgb_to_linear(pixels[i + 1] as f32 / 255.0),
            srgb_to_linear(pixels[i + 2] as f32 / 255.0),
        ];
        let want = expected(BlendMode::Multiply);
        for c in 0..3 {
            assert!(
                (got[c] - want[c]).abs() < 0.03,
                "overprint channel {c}: got {:.3}, want {:.3}",
                got[c],
                want[c],
            );
        }
    }

    /// Render a rectangle filled with a 20×20 two-colour checker pattern and
    /// confirm (a) the pattern actually tiles on-canvas (the checker alternates
    /// across the rectangle), and (b) the pattern is pinned to document space —
    /// translating the rectangle by a whole tile period leaves each absolute
    /// document pixel showing the same pattern colour (transform independence).
    #[test]
    fn pattern_fill_tiles_and_is_transform_independent() {
        use photonic_core::style::{Fill, PatternFill};
        use photonic_core::transform::Transform;
        use photonic_core::RasterImage;

        let Some(r) = try_renderer() else {
            eprintln!("no GPU adapter — skipping pattern tiling test");
            return;
        };

        // 20×20 checker: 10px cells, red / blue.
        let cell = 10u32;
        let n = 20u32;
        let mut tile = RasterImage::new(n, n);
        for y in 0..n {
            for x in 0..n {
                let on = ((x / cell) + (y / cell)).is_multiple_of(2);
                let rgba = if on {
                    [230, 30, 30, 255]
                } else {
                    [30, 30, 230, 255]
                };
                tile.set_pixel(x, y, rgba);
            }
        }
        let pattern = PatternFill::new(tile);

        let build = |dx: f64| -> image::RgbaImage {
            let mut doc = Document::new("pattern-test", 100.0, 100.0);
            let mut node = SceneNode::new(
                "rect",
                doc.active_layer_id.unwrap(),
                SceneNodeKind::Path(
                    PathNode::new(PathData::rect(0.0, 0.0, 100.0, 100.0))
                        .with_fill(Fill::pattern(pattern.clone())),
                ),
            );
            // Move the shape; the pattern must NOT move with it.
            node.transform = Transform::translate(dx, 0.0);
            doc.add_node(node, None);
            let png = r.render_png_at_size(&doc, 100, 100);
            image::load_from_memory(&png)
                .expect("decode png")
                .to_rgba8()
        };

        let img = build(0.0);
        // (a) Tiling: a red cell texel and the horizontally-adjacent blue cell.
        let red = img.get_pixel(5, 5).0; // cell (0,0) → red
        let blue = img.get_pixel(15, 5).0; // cell (1,0) → blue
        assert!(red[0] > 150 && red[2] < 100, "expected red, got {:?}", red);
        assert!(
            blue[2] > 150 && blue[0] < 100,
            "expected blue, got {:?}",
            blue
        );

        // (b) Transform independence: shifting the rect by a whole 20px tile
        // period leaves the same document pixel showing the same pattern colour.
        let shifted = build(20.0);
        let a = img.get_pixel(50, 50).0;
        let b = shifted.get_pixel(50, 50).0;
        let close = (0..3).all(|i| (a[i] as i32 - b[i] as i32).abs() <= 12);
        assert!(
            close,
            "pattern should be pinned to doc space: {:?} vs {:?}",
            a, b
        );
    }

    /// Regression: the headless PNG/raster export path used to drop text nodes
    /// entirely (`build_geometry` only emits Path geometry and the CPU compositor
    /// skips glyphs), so exported artboards had no text. A doc with a solid black
    /// background rect and a large white text node must export a PNG that
    /// actually contains white text pixels.
    #[test]
    fn raster_export_includes_text() {
        use photonic_core::node::TextNode;
        use photonic_core::transform::Transform;

        let Some(r) = try_renderer() else {
            eprintln!("no GPU adapter — skipping raster text export test");
            return;
        };

        let w = 300u32;
        let h = 120u32;
        let mut doc = Document::new("text-export-test", w as f64, h as f64);
        let layer = doc.active_layer_id.unwrap();

        // Full-artboard black backdrop: the only source of white in the output
        // is therefore the text itself.
        let backdrop = SceneNode::new(
            "backdrop",
            layer,
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(0.0, 0.0, w as f64, h as f64))
                    .with_fill(Fill::solid(Color::new(0.0, 0.0, 0.0, 1.0))),
            ),
        );
        doc.add_node(backdrop, None);

        // Capture the backdrop-only result before adding text. Comparing against
        // this baseline is more portable than assuming that every font backend
        // produces fully covered (near-255) pixels for a particular glyph.
        let backdrop_png = r.render_png_at_size(&doc, w, h);
        let backdrop_img = image::load_from_memory(&backdrop_png)
            .expect("decode backdrop png")
            .to_rgba8();

        // Large white text near the top-left. The glyph outline sits between the
        // transform origin and ~font_size below it (baseline-anchored), so this
        // lands well inside the artboard. Use an ascender and descender so the
        // assertion exercises real glyph outlines.
        let mut t = TextNode::new("Ag");
        t.font_size = 48.0;
        t.fill = Fill::solid(Color::new(1.0, 1.0, 1.0, 1.0));

        // Choose a concrete regular face whose glyph outline and lyon mesh are
        // both usable on this runner. Some headless macOS images expose font
        // metadata but no outline-capable system face; a raster export cannot
        // exercise text in that environment, so skip it explicitly rather than
        // treating missing host assets as a renderer regression.
        let mut font_system = glyphon::FontSystem::new();
        let mut candidates: Vec<(String, u16)> = font_system
            .db()
            .faces()
            .filter(|face| matches!(face.style, glyphon::cosmic_text::fontdb::Style::Normal))
            .filter_map(|face| {
                face.families
                    .first()
                    .map(|(family, _)| (family.clone(), face.weight.0))
            })
            .collect();
        candidates.sort_by_key(|(_, weight)| (*weight != 400, *weight));
        candidates.dedup();

        let selected = candidates.into_iter().find(|(family, weight)| {
            t.font_family = family.clone();
            t.font_weight = *weight;
            let path = crate::layout_text_flat(&mut font_system, &t);
            !path.is_empty()
                && !crate::tessellator::tessellate_fill(&path, true, 0.1)
                    .indices
                    .is_empty()
        });
        let Some((family, weight)) = selected else {
            eprintln!("no outline-capable system font — skipping raster text export test");
            return;
        };
        t.font_family = family;
        t.font_weight = weight;

        let mut text_node = SceneNode::new("label", layer, SceneNodeKind::Text(t));
        text_node.transform = Transform::new(1.0, 0.0, 0.0, 1.0, 20.0, 15.0);
        doc.add_node(text_node, None);

        let png = r.render_png_at_size(&doc, w, h);
        let img = image::load_from_memory(&png)
            .expect("decode png")
            .to_rgba8();

        // Count pixels that became brighter than the backdrop. Font backends can
        // legitimately produce different coverage values and glyph positions, so
        // compare the rendered result to its own black-backdrop baseline instead
        // of imposing a fixed glyph bbox or near-white threshold. With the bug
        // present both images are identical.
        let mut changed = 0u32;
        for y in 0..h {
            for x in 0..w {
                let current = img.get_pixel(x, y).0;
                let backdrop = backdrop_img.get_pixel(x, y).0;
                let current_luma =
                    u16::from(current[0]) + u16::from(current[1]) + u16::from(current[2]);
                let backdrop_luma =
                    u16::from(backdrop[0]) + u16::from(backdrop[1]) + u16::from(backdrop[2]);
                if current_luma > backdrop_luma + 6 {
                    changed += 1;
                }
            }
        }
        assert!(
            changed > 0,
            "raster export must contain text pixels — no pixels changed from the backdrop"
        );
    }
}
