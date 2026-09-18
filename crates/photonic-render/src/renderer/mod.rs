use crate::{
    canvas::CanvasView,
    headless::ExportBackground,
    pipeline::{
        blend_mode_index, coalesce_segments, create_blit_bgl, create_blit_pipeline,
        create_blur_bgl, create_blur_pipeline, create_blur_pipeline_with_blend,
        create_camera_bind_group_layout, create_composite_bgl, create_composite_pipeline,
        create_fill_pipeline, create_fill_pipeline_with_blend, draw_segments,
        separable_blend_state, BlurBlend, BlurParams, CameraUniform, CompositeParams, DrawSegment,
        Vertex, SEPARABLE_BLEND_MODES,
    },
    tessellator::{adaptive_tolerance, tessellate_fill},
};
use anyhow::{anyhow, Result};
use glyphon::{
    Attrs, Buffer, Cache, Color as GlyphonColor, FontSystem, Metrics, Resolution, Shaping,
    Style as GlyphonStyle, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
    Weight,
};
use image::{ImageBuffer, Rgba};
use photonic_core::{
    document::Document,
    history::CommandHistory,
    layer::BlendMode,
    node::{NodeId, SceneNodeKind},
    path::PathData,
    style::{FillKind, StrokeAlign},
};
use std::cell::RefCell;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::sync::Mutex;
use wgpu::util::DeviceExt;
use winit::window::Window;

mod camera;
mod capture;
mod effects_renderer;
mod frame_manager;
mod glow_renderer;
mod scene_renderer;
mod tess_cache;
mod text_renderer;

use tess_cache::{hash_svg, TessCache};

// ─── Background colour (deep violet-dark canvas surround) ─────────────────────
// Linear values for sRGB target #0D0D14 (r:13 g:13 b:20).
pub(crate) const BG: wgpu::Color = wgpu::Color {
    r: 0.002,
    g: 0.002,
    b: 0.005,
    a: 1.0,
};
/// [`BG`] as the **swapchain** expects it.
///
/// `BG` is a linear-light value, correct for the sRGB scene target where the
/// hardware re-encodes on write. The surface is a plain `*Unorm` view (see
/// `choose_surface_config`), so a clear written there lands raw — feeding it
/// `BG` would paint near-black instead of the window fill. These are the same
/// colour, sRGB-encoded: `#07070B`, matching the dark theme's `window_fill`.
pub(crate) const SURFACE_BG: wgpu::Color = wgpu::Color {
    r: 7.0 / 255.0,
    g: 7.0 / 255.0,
    b: 11.0 / 255.0,
    a: 1.0,
};
const MSAA_SAMPLES: u32 = 4;

fn adapter_rank(device_type: wgpu::DeviceType) -> u8 {
    match device_type {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Cpu => 3,
        wgpu::DeviceType::Other => 4,
    }
}

fn choose_surface_config(
    caps: &wgpu::SurfaceCapabilities,
    width: u32,
    height: u32,
) -> Result<wgpu::SurfaceConfiguration> {
    let Some(surface_format) = caps
        .formats
        .iter()
        .find(|format| {
            matches!(
                format,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
            )
        })
        .copied()
        .or_else(|| caps.formats.first().copied())
    else {
        return Err(anyhow!("surface has no compatible texture formats"));
    };
    let Some(present_mode) = caps
        .present_modes
        .iter()
        .copied()
        .find(|mode| *mode == wgpu::PresentMode::Fifo)
        .or_else(|| caps.present_modes.first().copied())
    else {
        return Err(anyhow!("surface has no compatible presentation modes"));
    };
    let Some(alpha_mode) = caps.alpha_modes.first().copied() else {
        return Err(anyhow!("surface has no compatible alpha modes"));
    };

    Ok(wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: surface_format,
        width,
        height,
        present_mode,
        alpha_mode,
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    })
}

// ─── Frame handle ─────────────────────────────────────────────────────────────

/// Owns the surface frame for one in-flight render.
///
/// After `begin_frame` records the document pass, callers may record
/// additional passes (e.g. egui) into `encoder` before handing the handle
/// back to `finish_frame`.
pub struct FrameHandle {
    pub surface_texture: wgpu::SurfaceTexture,
    pub view: wgpu::TextureView,
    pub encoder: wgpu::CommandEncoder,
}

// ─── Main renderer ───────────────────────────────────────────────────────────

pub struct PhotonicRenderer {
    /// `None` for a windowless (offscreen-only) renderer built via
    /// [`PhotonicRenderer::new_offscreen`] — it renders only through
    /// `capture_png` and never presents a swapchain frame.
    pub(crate) surface: Option<wgpu::Surface<'static>>,
    /// `Arc`-wrapped so the device/queue can be shared with a second, isolated
    /// offscreen export renderer ([`new_offscreen_shared`](Self::new_offscreen_shared))
    /// without creating a conflicting second `wgpu` device. `wgpu::Device`/`Queue`
    /// are not `Clone` in wgpu 22, so `Arc` is how we hand out a shared handle.
    pub(crate) device: Arc<wgpu::Device>,
    pub(crate) queue: Arc<wgpu::Queue>,
    pub(crate) surface_config: wgpu::SurfaceConfiguration,
    /// The swapchain *presentation* format only. It is a non-sRGB `*Unorm`
    /// surface (see `choose_surface_config`), so writing document pixels raw
    /// into it would blend in the wrong space (audit A-1). The document pass
    /// therefore targets [`scene_format`](Self::scene_format), not this.
    pub(crate) surface_format: wgpu::TextureFormat,
    /// The colour encoding the document fill/blend pass renders in — always
    /// [`pipeline::SCENE_FORMAT`](crate::pipeline::SCENE_FORMAT)
    /// (`Rgba8UnormSrgb`), independent of the presentation `surface_format`.
    /// An sRGB target makes fixed-function blending decode→blend→re-encode in
    /// linear light (03 §4.5.1), so on-canvas rendering matches the headless
    /// export path (`headless::FORMAT`, the same sRGB format). Read back via
    /// [`scene_format`](Self::scene_format); guarded sRGB by the invariant test.
    pub(crate) scene_format: wgpu::TextureFormat,
    /// Physical-pixel `(x, y, w, h)` the document is allowed to present into,
    /// or `None` for the whole surface (the default, and what headless/offscreen
    /// renderers use).
    ///
    /// The document pass covers the entire window, but the GUI only *shows* a
    /// slice of it: egui's panels paint over the rest. That was fine while the
    /// panels tiled the window edge-to-edge, and wrong once the rails and
    /// drawers became floating cards with deliberate gaps between them — the
    /// document (a white artboard, most of the time) showed through those gaps
    /// as a bright bar beside the canvas. Clipping the present to the canvas
    /// viewport leaves the gaps on the clear colour, which is the window fill
    /// they are meant to show.
    pub(crate) canvas_scissor: Option<(u32, u32, u32, u32)>,

    pub(crate) fill_pipeline: wgpu::RenderPipeline,
    /// One fill-pipeline variant per separable blend mode (Multiply/Screen/
    /// Darken/Lighten), built at `MSAA_SAMPLES`. Modes without an entry fall back
    /// to `fill_pipeline` (normal alpha blending).
    pub(crate) blend_pipelines: Vec<(BlendMode, wgpu::RenderPipeline)>,
    pub(crate) camera_buffer: wgpu::Buffer,
    pub(crate) camera_bind_group: wgpu::BindGroup,

    pub(crate) msaa_texture: wgpu::Texture,
    pub(crate) msaa_view: wgpu::TextureView,

    /// The offscreen document target (03 §4.5.4, audit A-1). The document
    /// fill/blend/composite pass resolves here at [`scene_format`](Self::scene_format)
    /// (`Rgba8UnormSrgb`), so on-canvas blending runs in linear light exactly as
    /// the headless export path does — never in the swapchain's non-sRGB
    /// presentation format. `begin_frame` then blits this into the surface view
    /// (see [`blit_scene_to_surface`](Self::blit_scene_to_surface)). Sized to the
    /// surface, recreated in `resize` alongside the MSAA/glow targets.
    pub(crate) scene_tex: wgpu::Texture,
    pub(crate) scene_view: wgpu::TextureView,
    /// Full-screen present pipeline that samples [`scene_view`](Self::scene_tex)
    /// (hardware sRGB-decode on sample) and writes the sRGB-encoded pixel into the
    /// non-sRGB `surface_format` view — the A-1 blit (03 §4.5.4). Built against
    /// `surface_format` (the presentation format), not `scene_format`.
    pub(crate) blit_pipeline: wgpu::RenderPipeline,
    pub(crate) blit_bgl: wgpu::BindGroupLayout,
    pub(crate) blit_sampler: wgpu::Sampler,

    pub view: CanvasView,
    document: Arc<Mutex<Document>>,
    /// Read-only handle to the shared undo history, used purely as a change
    /// signal: `revision()` drives the per-frame skip and `changes_since` the
    /// cache-invalidation policy (03 §2.2). Polled with `try_lock` and never
    /// blocked on — a contended lock falls back to a full rebuild. `None` for
    /// the offscreen/export renderers, which have no document history to poll
    /// and therefore never take the frame-skip (the content-addressed
    /// tessellation memo still applies, so this costs nothing but correctness).
    history: Option<Arc<Mutex<CommandHistory>>>,
    pub(crate) capture_rx: std::sync::mpsc::Receiver<oneshot::Sender<Vec<u8>>>,

    pub(crate) width: u32,
    pub(crate) height: u32,

    /// Last successfully built geometry — returned as-is when the doc lock is contended.
    cached_vertices: Vec<Vertex>,
    cached_indices: Vec<u32>,
    /// Per-blend-mode index ranges for the current frame, in draw order. Read by
    /// `record_document_pass` to issue one draw call per contiguous run.
    pub(crate) draw_segments: Vec<DrawSegment>,
    cached_segments: Vec<DrawSegment>,

    // ── Dirty tracking + persistent buffers (03 §2.2 / §2.3) ───────────────────
    /// Content-addressed memo of the three `tessellate_*` functions, so an
    /// unchanged path is triangulated at most once and reused across frames.
    tess_cache: TessCache,
    /// `CommandHistory::revision()` observed at the last geometry build. When it
    /// and the view are unchanged, the whole build is skipped (§2.2 step 1).
    last_revision: Option<u64>,
    /// Bit pattern of `(view.zoom, view.pan_x, view.pan_y)` at the last build.
    /// Vertices are document-space (pan/zoom live in the camera uniform), but
    /// glyph screen positions and Gaussian-glow sigma are view-derived, so the
    /// frame-skip is gated on the view being unchanged too.
    last_view: Option<(u64, u64, u64)>,
    /// Persistent, growable document vertex/index buffers (03 §2.3). Replace the
    /// per-frame `create_buffer_init` in `record_document_pass`; grown by
    /// doubling and re-uploaded in place with `queue.write_buffer`.
    doc_vbuf: wgpu::Buffer,
    doc_vbuf_cap: u64,
    doc_ibuf: wgpu::Buffer,
    doc_ibuf_cap: u64,
    /// Instrumentation for the last `build_geometry`: (distinct nodes
    /// re-tessellated, total `tessellate_*` calls). Zero on a frame-skip.
    last_tess_nodes: u32,
    last_tess_calls: u32,

    // ── Text rendering (glyphon) ───────────────────────────────────────────────
    pub(crate) font_system: FontSystem,
    pub(crate) swash_cache: SwashCache,
    /// Kept alive so `text_atlas` and `text_viewport` remain valid.
    #[allow(dead_code)]
    text_glyph_cache: Cache,
    pub(crate) text_atlas: TextAtlas,
    pub(crate) text_renderer: TextRenderer,
    pub(crate) text_viewport: Viewport,
    /// Text nodes collected during `build_geometry` for the current frame.
    pub(crate) pending_texts: Vec<TextSnapshot>,
    /// Text-on-path glyph outlines (document space) + RGBA fill colour, built
    /// during `build_geometry` and tessellated into the fill geometry. Rendered
    /// as vector fills because glyphon cannot rotate glyphs along a curve.
    pending_path_text: Vec<(Vec<PathData>, [f32; 4])>,

    // ── Gaussian glow blur ────────────────────────────────────────────────────
    pub(crate) fill_pipeline_1spp: wgpu::RenderPipeline, // sample_count=1 for offscreen silhouette
    pub(crate) blur_pipeline_h: wgpu::RenderPipeline,    // H blur (alpha-blend output)
    blur_pipeline_v: wgpu::RenderPipeline,               // V blur (additive composite to surface)
    pub(crate) blur_bgl: wgpu::BindGroupLayout,
    pub(crate) blur_sampler: wgpu::Sampler,
    pub(crate) glow_tex_a: wgpu::Texture, // silhouette & V-blur source
    pub(crate) glow_tex_a_view: wgpu::TextureView,
    pub(crate) glow_tex_b: wgpu::Texture, // H-blur output
    pub(crate) glow_tex_b_view: wgpu::TextureView,
    /// Gaussian glow jobs built each frame by build_geometry, consumed by render_gaussian_glow_pass.
    pub(crate) pending_gaussian_glows: Vec<GaussianGlowJob>,

    // ── Live-effects (drop shadow / object blur / feather) blur layer ──────────
    /// Straight-alpha blur/composite pipeline (the effect textures hold
    /// non-premultiplied colour, unlike the premultiplied glow path). The blur
    /// ping-pong / layer textures are allocated per frame at the target size so
    /// the same path serves both the window and offscreen capture.
    pub(crate) blur_pipeline_alpha: wgpu::RenderPipeline,
    /// Effect blur jobs built each frame by build_geometry.
    pub(crate) pending_blur_jobs: Vec<BlurJob>,

    // ── Per-layer compositing (#226) ──────────────────────────────────────────
    /// Full-screen composite of an isolated layer texture over a backdrop,
    /// honouring every blend mode with a backdrop read (the GPU port of the CPU
    /// compositor's per-layer isolation).
    pub(crate) composite_bgl: wgpu::BindGroupLayout,
    pub(crate) composite_pipeline: wgpu::RenderPipeline,
    pub(crate) composite_sampler: wgpu::Sampler,
    /// Index ranges of each layer's geometry in the frame's shared index buffer,
    /// in draw order, tagged with the layer's opacity + blend mode. Built by
    /// build_geometry; consumed by the isolated render path when any layer is
    /// non-trivial (opacity < 1 or a non-Normal blend).
    pub(crate) layer_runs: Vec<LayerRun>,
    /// Index count of the artboard-background quads (the preamble drawn before
    /// any layer). The first `artboard_idx_end` indices are the white boards.
    pub(crate) artboard_idx_end: u32,

    /// Export mode — set only while rendering for PNG/artboard export via
    /// [`Self::render_export_rgba`]. `None` is the normal editor frame. When set
    /// it steers the shared draw path so export is pixel-identical to the editor:
    /// `Some(Artboard)` keeps the white board + violet surround (identical to the
    /// on-canvas frame); `Some(Transparent)` suppresses the white board quads and
    /// clears the scene to alpha 0 instead of the surround colour.
    pub(crate) export_bg: Option<ExportBackground>,

    /// Shared GPU-health handle (37 §1.3). The device-loss callback installed in
    /// [`assemble`](Self::assemble) flips this to `Lost`, and `begin_frame` marks
    /// it lost on a terminal surface error. Cloned out via [`gpu_health`](Self::gpu_health)
    /// so photonic-video and the app shell observe the same machine.
    pub(crate) gpu_health: crate::gpu_state::GpuHealth,
    /// Capability floor result for the adapter that backs this renderer (37 §1.2).
    /// Empty (floor met) for offscreen/export renderers, which never present video.
    /// The windowed [`new`](Self::new) constructor fills it from the selected adapter.
    capability_report: crate::capability::CapabilityReport,
}

/// One layer's contiguous geometry range in the frame's shared index buffer,
/// plus the layer opacity + blend mode used to composite it as an isolated unit.
#[derive(Clone)]
pub(crate) struct LayerRun {
    pub(crate) opacity: f32,
    pub(crate) blend: BlendMode,
    pub(crate) idx_start: u32,
    pub(crate) idx_end: u32,
    /// Dense ordinal of this layer — used to gather its blur-effect jobs.
    pub(crate) ordinal: u32,
}

impl LayerRun {
    /// A layer that changes the pixels when isolated — needs its own offscreen
    /// pass + composite. Opaque Normal layers can draw straight into the scene.
    fn is_nontrivial(&self) -> bool {
        (self.opacity - 1.0).abs() > f32::EPSILON || self.blend != BlendMode::Normal
    }
}

/// One blurred live effect for the windowed renderer's effects layer:
/// pre-transformed document-space geometry + blur radius in document units.
pub(crate) struct BlurJob {
    pub(crate) verts: Vec<Vertex>,
    pub(crate) idxs: Vec<u32>,
    pub(crate) radius_doc: f64,
    /// Owning layer's dense ordinal (matches `NodeSnapshot::layer_ordinal`), so
    /// effects render inside their layer's isolated unit (#226 Stage 2).
    pub(crate) layer_ordinal: u32,
}

/// Screen-space snapshot of one text node, ready for glyphon.
pub(crate) struct TextSnapshot {
    pub(crate) content: String,
    pub(crate) font_family: String,
    /// Font size already scaled by canvas zoom (physical pixels).
    pub(crate) font_size: f32,
    /// Line height multiplier (default: 1.2).
    pub(crate) line_height_mul: f32,
    /// Font weight (100–900).
    pub(crate) font_weight: u16,
    /// Font style: 0=Normal, 1=Italic, 2=Oblique.
    pub(crate) font_style: u8,
    /// RGBA 0-255 fill colour.
    pub(crate) color: [u8; 4],
    pub(crate) screen_x: f32,
    pub(crate) screen_y: f32,
    /// Vertical offset in physical pixels added to the text's top, encoding the
    /// node's baseline shift and super/subscript position. Positive moves the
    /// glyphs down; negative (raised, e.g. superscript) moves them up.
    pub(crate) top_offset: f32,
}

pub(crate) struct GaussianGlowJob {
    /// Fill geometry coloured with the glow tint (rendered to offscreen silhouette texture).
    pub(crate) verts: Vec<Vertex>,
    pub(crate) idxs: Vec<u32>,
    /// Blur sigma in screen pixels (radius_doc_units × zoom).
    pub(crate) sigma_px: f32,
}

impl PhotonicRenderer {
    pub async fn new(
        window: Arc<Window>,
        document: Arc<Mutex<Document>>,
        history: Arc<Mutex<CommandHistory>>,
        capture_rx: std::sync::mpsc::Receiver<oneshot::Sender<Vec<u8>>>,
    ) -> Result<Self> {
        let size = window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);

        let backends = wgpu::util::backend_bits_from_env().unwrap_or(wgpu::Backends::all());
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });

        let surface = instance
            .create_surface(window)
            .map_err(|error| anyhow!("failed to create wgpu surface: {error}"))?;

        let mut adapters = instance.enumerate_adapters(backends);
        adapters.sort_by_key(|adapter| adapter_rank(adapter.get_info().device_type));
        let mut selected = None;
        for adapter in adapters {
            let info = adapter.get_info();
            let caps = surface.get_capabilities(&adapter);
            if caps.formats.is_empty()
                || caps.present_modes.is_empty()
                || caps.alpha_modes.is_empty()
            {
                tracing::warn!(adapter = %info.name, ?info.backend, "Skipping adapter with incompatible surface capabilities");
                continue;
            }
            match adapter
                .request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("photonic_device"),
                        required_features: wgpu::Features::empty(),
                        required_limits: wgpu::Limits::default(),
                        memory_hints: Default::default(),
                    },
                    None,
                )
                .await
            {
                Ok((device, queue)) => {
                    // Capability floor (37 §1.2): evaluated on the *selected*
                    // adapter while it is still in scope. We do NOT reject the
                    // adapter here — vector mode must run below the floor; only
                    // video mode is gated (in photonic-gui), off this report.
                    let capability = crate::capability::check_capability_floor(&adapter);
                    selected = Some((info, caps, device, queue, capability));
                    break;
                }
                Err(error) => {
                    tracing::warn!(adapter = %info.name, ?info.backend, %error, "Unable to create device; trying next adapter")
                }
            }
        }
        let Some((adapter_info, caps, device, queue, capability)) = selected else {
            return Err(anyhow!("no GPU adapter could create a device compatible with this window (backends: {backends:?})"));
        };
        let (device, queue) = (Arc::new(device), Arc::new(queue));
        let surface_config = choose_surface_config(&caps, width, height)?;
        let surface_format = surface_config.format;
        tracing::info!(adapter = %adapter_info.name, ?adapter_info.backend, ?adapter_info.device_type, ?surface_format, ?surface_config.present_mode, "Selected GPU adapter and surface configuration");
        if !capability.meets_floor() {
            tracing::warn!(reason = %capability.reason(), "GPU below the video-mode capability floor; vector mode only");
        }
        surface.configure(&device, &surface_config);

        let mut renderer = Self::assemble(
            device,
            queue,
            Some(surface),
            surface_config,
            surface_format,
            width,
            height,
            document,
            Some(history),
            capture_rx,
        );
        renderer.capability_report = capability;
        Ok(renderer)
    }

    /// Construct a **windowless** renderer that draws only to offscreen textures
    /// (via `capture_png`). No surface is created, so `begin_frame`/`render`
    /// present nothing — the real GPU pipeline (`render_scene`) still runs, which
    /// is what lets tests and headless captures exercise it. Returns `None` when
    /// no GPU adapter is available (e.g. CI without a GPU) so callers skip cleanly.
    pub async fn new_offscreen(
        width: u32,
        height: u32,
        document: Arc<Mutex<Document>>,
        capture_rx: std::sync::mpsc::Receiver<oneshot::Sender<Vec<u8>>>,
    ) -> Option<Self> {
        let width = width.max(1);
        let height = height.max(1);

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await?;
        let (device, queue) = Self::request_device(&adapter).await;
        let (device, queue) = (Arc::new(device), Arc::new(queue));

        // Linear (non-sRGB) RGBA: readback needs no channel swap and blending
        // matches the windowed path's preferred format.
        let surface_format = wgpu::TextureFormat::Rgba8Unorm;
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width,
            height,
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        Some(Self::assemble(
            device,
            queue,
            None,
            surface_config,
            surface_format,
            width,
            height,
            document,
            None,
            capture_rx,
        ))
    }

    /// Construct a **windowless** renderer that reuses an **already-created**
    /// `device`/`queue` instead of requesting its own adapter + device. This is the
    /// export path inside the live GUI process: the app hands us a clone of the
    /// windowed renderer's device/queue so a full-resolution artboard/PNG export
    /// renders on the SAME GPU context — no second `wgpu::Instance`/adapter/device
    /// is created. Spinning up a second device alongside the presenting surface
    /// device is what crashed the running app; sharing one device is safe because
    /// `wgpu::Device`/`Queue` are `Send + Sync` and internally synchronised, and
    /// this renderer keeps its own isolated `document`, size and camera state, so it
    /// never disturbs the live view.
    ///
    /// Uses `Rgba8Unorm` (like [`new_offscreen`](Self::new_offscreen)) regardless of
    /// the windowed surface format, so readback needs no channel swap.
    pub fn new_offscreen_shared(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        width: u32,
        height: u32,
        document: Arc<Mutex<Document>>,
        capture_rx: std::sync::mpsc::Receiver<oneshot::Sender<Vec<u8>>>,
    ) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let surface_format = wgpu::TextureFormat::Rgba8Unorm;
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width,
            height,
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        Self::assemble(
            device,
            queue,
            None,
            surface_config,
            surface_format,
            width,
            height,
            document,
            None,
            capture_rx,
        )
    }

    async fn request_device(adapter: &wgpu::Adapter) -> (wgpu::Device, wgpu::Queue) {
        adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("photonic_device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: Default::default(),
                },
                None,
            )
            .await
            .expect("Failed to create wgpu device")
    }

    /// Build all pipelines, textures and per-frame state from an already-acquired
    /// device/queue. Shared by the windowed ([`new`](Self::new)) and offscreen
    /// ([`new_offscreen`](Self::new_offscreen)) constructors.
    #[allow(clippy::too_many_arguments)]
    fn assemble(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        surface: Option<wgpu::Surface<'static>>,
        surface_config: wgpu::SurfaceConfiguration,
        surface_format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        document: Arc<Mutex<Document>>,
        history: Option<Arc<Mutex<CommandHistory>>>,
        capture_rx: std::sync::mpsc::Receiver<oneshot::Sender<Vec<u8>>>,
    ) -> Self {
        // Camera bind group
        let camera_bgl = create_camera_bind_group_layout(&device);
        let initial_cam = CameraUniform::from_viewport(0.0, 0.0, 1.0, width, height);
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera_buf"),
            contents: bytemuck::bytes_of(&initial_cam),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera_bg"),
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        // Log any uncaptured GPU errors (device lost, OOM, etc.) so they appear
        // in the log file even if they don't trigger a Rust panic.
        device.on_uncaptured_error(Box::new(|e| {
            tracing::error!("wgpu uncaptured error: {:?}", e);
        }));

        // Device-loss detection (37 §1.3). A driver-triggered loss (`Unknown` —
        // a TDR, a driver reset, an eGPU unplug) flips the shared health machine
        // to `Lost` so the engine/app can pause, drop GPU caches and rebuild. Our
        // own teardown reasons (`Destroyed`/`Dropped` on device drop, plus the
        // `ReplacedCallback`/`DeviceInvalid` bookkeeping reasons) MUST NOT trip
        // recovery — they are not real losses.
        let gpu_health = crate::gpu_state::GpuHealth::new();
        {
            let health = gpu_health.clone();
            device.set_device_lost_callback(move |reason, msg| {
                if matches!(reason, wgpu::DeviceLostReason::Unknown) {
                    health.mark_lost();
                    tracing::error!(?reason, %msg, "wgpu device lost");
                } else {
                    tracing::debug!(?reason, %msg, "wgpu device lost callback (teardown)");
                }
            });
        }

        // The document fill/blend/composite pass renders in the sRGB scene format
        // (03 §4.5.4, audit A-1), independent of the non-sRGB presentation
        // `surface_format`, so fixed-function + shader blending run in linear light
        // and the canvas matches the headless export path by construction.
        let scene_format = crate::pipeline::SCENE_FORMAT;

        let fill_pipeline = create_fill_pipeline(&device, scene_format, &camera_bgl, MSAA_SAMPLES);
        // One pipeline variant per separable blend mode, sharing the fill shader.
        let blend_pipelines: Vec<(BlendMode, wgpu::RenderPipeline)> = SEPARABLE_BLEND_MODES
            .iter()
            .filter_map(|&mode| {
                separable_blend_state(mode).map(|blend| {
                    (
                        mode,
                        create_fill_pipeline_with_blend(
                            &device,
                            scene_format,
                            &camera_bgl,
                            MSAA_SAMPLES,
                            blend,
                        ),
                    )
                })
            })
            .collect();
        let (msaa_texture, msaa_view) = create_msaa_texture(&device, scene_format, width, height);

        let blur_bgl = create_blur_bgl(&device);
        let fill_pipeline_1spp = create_fill_pipeline(&device, scene_format, &camera_bgl, 1);
        let blur_pipeline_h = create_blur_pipeline(&device, scene_format, &blur_bgl, false);
        // The gaussian-glow present pass (glow_renderer.rs Pass C) blends its blur
        // directly into the swapchain frame, so this ONE pipeline must target the
        // non-sRGB `surface_format`; every other document-pass resource is
        // `scene_format`. (Its input glow texture is `scene_format`; sampling
        // hardware-decodes it, so the additive glow blend still reads linear.)
        let blur_pipeline_v = create_blur_pipeline(&device, surface_format, &blur_bgl, true);
        let blur_pipeline_alpha = create_blur_pipeline_with_blend(
            &device,
            scene_format,
            &blur_bgl,
            BlurBlend::StraightAlpha,
        );
        let blur_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blur_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let (glow_tex_a, glow_tex_a_view, glow_tex_b, glow_tex_b_view) =
            create_glow_textures(&device, scene_format, width, height);

        // Offscreen document target + present (blit) pipeline (03 §4.5.4).
        let (scene_tex, scene_view) = create_scene_texture(&device, scene_format, width, height);
        let blit_bgl = create_blit_bgl(&device);
        let blit_pipeline = create_blit_pipeline(&device, surface_format, &blit_bgl);
        let blit_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blit_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        // Per-layer compositing (#226): shader + a filtering sampler.
        let composite_bgl = create_composite_bgl(&device);
        let composite_pipeline = create_composite_pipeline(&device, scene_format, &composite_bgl);
        let composite_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("composite_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        // Fit the view to the document artboard
        let mut view = CanvasView::new(width, height);
        {
            let doc = document.blocking_lock();
            view.fit_to_rect(0.0, 0.0, doc.width, doc.height);
        }

        // ── Glyphon text rendering ─────────────────────────────────────────────
        let font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let text_glyph_cache = Cache::new(&device);
        let text_viewport = Viewport::new(&device, &text_glyph_cache);
        // Construct the atlas in `ColorMode::Web` so glyphon writes raw sRGB colours
        // to the sRGB render target instead of linearizing them (its default
        // `Accurate` mode). This makes interactive text colour match the vector fill
        // pipeline, which passes sRGB through unmodified (see pipeline.rs `fs_main`).
        // The capture-path text pass paints into the sRGB scene target (capture.rs),
        // so the atlas must match `scene_format`, not the presentation surface.
        let mut text_atlas = TextAtlas::with_color_mode(
            &device,
            &queue,
            &text_glyph_cache,
            scene_format,
            glyphon::ColorMode::Web,
        );
        let text_renderer = TextRenderer::new(
            &mut text_atlas,
            &device,
            wgpu::MultisampleState::default(),
            None,
        );

        // Persistent document geometry buffers (03 §2.3). Start with a small
        // non-zero capacity; the first `build_geometry` grows them to fit.
        const INITIAL_BUF_CAP: u64 = 64 * 1024;
        let doc_vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("doc_vbuf"),
            size: INITIAL_BUF_CAP,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let doc_ibuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("doc_ibuf"),
            size: INITIAL_BUF_CAP,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            surface,
            device,
            queue,
            surface_config,
            surface_format,
            scene_format,
            canvas_scissor: None,
            fill_pipeline,
            blend_pipelines,
            camera_buffer,
            camera_bind_group,
            msaa_texture,
            msaa_view,
            scene_tex,
            scene_view,
            blit_pipeline,
            blit_bgl,
            blit_sampler,
            view,
            document,
            history,
            capture_rx,
            width,
            height,
            cached_vertices: Vec::new(),
            cached_indices: Vec::new(),
            draw_segments: Vec::new(),
            cached_segments: Vec::new(),
            tess_cache: TessCache::default(),
            last_revision: None,
            last_view: None,
            doc_vbuf,
            doc_vbuf_cap: INITIAL_BUF_CAP,
            doc_ibuf,
            doc_ibuf_cap: INITIAL_BUF_CAP,
            last_tess_nodes: 0,
            last_tess_calls: 0,
            font_system,
            swash_cache,
            text_glyph_cache,
            text_atlas,
            text_renderer,
            text_viewport,
            pending_texts: Vec::new(),
            pending_path_text: Vec::new(),
            fill_pipeline_1spp,
            blur_pipeline_h,
            blur_pipeline_v,
            blur_bgl,
            blur_sampler,
            glow_tex_a,
            glow_tex_a_view,
            glow_tex_b,
            glow_tex_b_view,
            pending_gaussian_glows: Vec::new(),
            blur_pipeline_alpha,
            pending_blur_jobs: Vec::new(),
            composite_bgl,
            composite_pipeline,
            composite_sampler,
            layer_runs: Vec::new(),
            artboard_idx_end: 0,
            export_bg: None,
            gpu_health,
            capability_report: crate::capability::CapabilityReport::default(),
        }
    }

    /// A cheaply-cloned handle to this renderer's GPU-health machine (37 §1.3),
    /// so photonic-video and the app shell observe the same device-loss state.
    pub fn gpu_health(&self) -> crate::gpu_state::GpuHealth {
        self.gpu_health.clone()
    }

    /// The capability-floor report for the adapter backing this renderer (37 §1.2).
    /// Consulted by the app shell to refuse video mode below the floor while
    /// keeping vector mode alive.
    pub fn capability_report(&self) -> &crate::capability::CapabilityReport {
        &self.capability_report
    }

    /// Clear colour for the scene background pass. The normal editor frame clears
    /// to the violet surround [`BG`]; a transparent export clears to alpha 0 so
    /// pixels outside the artwork stay see-through. Read by every scene-background
    /// clear site (`scene_renderer`, `effects_renderer`) so the choice is uniform.
    pub(crate) fn scene_clear(&self) -> wgpu::Color {
        match self.export_bg {
            Some(ExportBackground::Transparent) => wgpu::Color::TRANSPARENT,
            _ => BG,
        }
    }

    /// Restrict presentation of the document to `rect` in physical pixels
    /// (`(x, y, w, h)`), or `None` for the whole surface.
    ///
    /// See [`canvas_scissor`](Self::canvas_scissor). The host sets this each
    /// frame from the GUI's canvas viewport; anything outside stays on the clear
    /// colour so the floating panel cards read against the window fill instead
    /// of against the artboard.
    pub fn set_canvas_scissor(&mut self, rect: Option<(u32, u32, u32, u32)>) {
        // A zero-area scissor is invalid in wgpu; treat it as "nothing to show".
        self.canvas_scissor = rect.filter(|&(_, _, w, h)| w > 0 && h > 0);
    }

    // ── Public accessors ──────────────────────────────────────────────────────

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Shared handle to the GPU device — cloned into the MCP export renderer so a
    /// full-resolution export renders on this SAME device instead of spinning up a
    /// conflicting second one (see [`new_offscreen_shared`](Self::new_offscreen_shared)).
    pub fn device_arc(&self) -> Arc<wgpu::Device> {
        Arc::clone(&self.device)
    }

    /// Shared handle to the GPU queue — companion to [`device_arc`](Self::device_arc).
    pub fn queue_arc(&self) -> Arc<wgpu::Queue> {
        Arc::clone(&self.queue)
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.surface_format
    }

    /// The colour encoding the document fill/blend pass renders in — always the
    /// sRGB [`pipeline::SCENE_FORMAT`](crate::pipeline::SCENE_FORMAT), never the
    /// non-sRGB presentation [`surface_format`](Self::surface_format). Exposed so
    /// tests can pin the invariant that the document pass stays sRGB (linear-light
    /// blending, 03 §4.5.1) rather than silently regressing to the swapchain's
    /// non-sRGB format.
    pub fn scene_format(&self) -> wgpu::TextureFormat {
        self.scene_format
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Measure the rendered dimensions of a text string using glyphon layout.
    /// Returns `(width, height)` in document units (same coordinate space as `font_size`).
    pub fn measure_text(&mut self, content: &str, font_family: &str, font_size: f64) -> (f64, f64) {
        let fs = font_size as f32;
        let line_height = fs * 1.2;
        let mut buf = Buffer::new(&mut self.font_system, Metrics::new(fs, line_height));
        buf.set_size(&mut self.font_system, None, None);
        let attrs = Attrs::new().family(crate::text_outline::cosmic_family(font_family));
        buf.set_text(&mut self.font_system, content, attrs, Shaping::Advanced);
        buf.shape_until_scroll(&mut self.font_system, false);
        let width = buf.layout_runs().map(|r| r.line_w).fold(0.0_f32, f32::max);
        let height = buf
            .layout_runs()
            .map(|r| r.line_height)
            .fold(0.0_f32, |a, h| a + h);
        let height = if height == 0.0 { line_height } else { height };
        (width as f64, height as f64)
    }

    /// Convenience: full render loop without an egui overlay.
    pub fn render(&mut self) {
        let (verts, idxs) = self.update(); // already &mut self
        if let Ok(Some(frame)) = self.begin_frame(&verts, &idxs) {
            self.finish_frame(frame);
        }
        self.service_captures(&verts, &idxs);
    }

    /// Walk the document and build one combined vertex + index buffer.
    ///
    /// The doc lock is held only long enough to clone the node data needed for
    /// tessellation. All actual tessellation happens after the lock is released,
    /// so MCP handlers and the egui closure are never blocked by tessellation time.
    ///
    /// If the doc lock is currently held by another thread (e.g. an MCP handler
    /// that outlived its session), we return the cached geometry from the previous
    /// frame instead of blocking indefinitely.
    fn build_geometry(&mut self) -> (Vec<Vertex>, Vec<u32>) {
        // ── Dirty-tracking gate (03 §2.2) ─────────────────────────────────────
        // Vertices are document-space (pan/zoom live in the camera uniform), but
        // glyph screen positions and Gaussian-glow sigma are view-derived, so the
        // whole-frame skip is gated on both the revision and the view.
        let view_key = (
            self.view.zoom.to_bits(),
            self.view.pan_x.to_bits(),
            self.view.pan_y.to_bits(),
        );
        // Poll the history revision without ever blocking the render loop. A
        // contended lock leaves `revision = None`, forcing a full rebuild — the
        // content-addressed tessellation memo keeps that correct and cheap
        // (unchanged geometry still hits), only the frame-skip is unavailable.
        let (revision, overflowed) = match self.history.as_ref().and_then(|h| h.try_lock().ok()) {
            Some(h) => {
                let rev = h.revision();
                let overflowed = match self.last_revision {
                    // A command ran: if the change ring can't attribute it to a
                    // node set, invalidate everything (§2.2 step 2).
                    Some(last) if rev != last => h.changes_since(last).overflowed,
                    // No baseline (first build / post-contention): rebuild all.
                    None => true,
                    _ => false,
                };
                (Some(rev), overflowed)
            }
            // No history handle (offscreen/export) or a contended lock: skip the
            // frame-skip entirely and rebuild, which is always correct.
            None => (None, false),
        };
        // One integer + one tuple compare for the common idle case (§2.2 step 1):
        // nothing changed and the view is unchanged, so reuse every cached range.
        if let (Some(rev), Some(last)) = (revision, self.last_revision) {
            if rev == last && self.last_view == Some(view_key) {
                self.draw_segments = self.cached_segments.clone();
                self.last_tess_nodes = 0;
                self.last_tess_calls = 0;
                return (self.cached_vertices.clone(), self.cached_indices.clone());
            }
        }
        let clear_cache = overflowed;

        // ── Read phase: clone what we need, release doc lock immediately ──────
        struct NodeSnapshot {
            /// Document node id (pre-symbol-resolution), used only to attribute
            /// tessellation work to a node for the perf instrumentation.
            node_id: NodeId,
            matrix: [f64; 6],
            fill_enabled: bool,
            fill_kind: FillKind,
            fill_opacity: f32,
            node_opacity: f32,
            fill_is_none: bool,
            stroke_enabled: bool,
            stroke_color: [f32; 4],
            /// Gradient/pattern stroke paint (#201). `None` = flat `stroke_color`.
            stroke_paint: Option<FillKind>,
            /// Combined opacity (stroke.opacity × node.opacity) for paint sampling.
            stroke_paint_opacity: f32,
            stroke_width: f32,
            stroke_cap: photonic_core::style::LineCap,
            stroke_join: photonic_core::style::LineJoin,
            stroke_miter: f32,
            stroke_align: StrokeAlign,
            /// Resolved variable-width profile samples (uniform-t along the path).
            /// `None` → render with the uniform `stroke_width`.
            stroke_widths: Option<Vec<f64>>,
            path_data: photonic_core::path::PathData,
            /// (r, g, b, a, opacity, size) + join — None when disabled.
            outer_glow: Option<([f32; 6], photonic_core::style::LineJoin)>,
            inner_glow: Option<([f32; 6], photonic_core::style::LineJoin)>,
            gaussian_glow: Option<([f32; 4], f32)>, // ([r,g,b,a*opacity], radius_doc)
            /// ([r,g,b,a], opacity, dx_doc, dy_doc, blur_doc) — None when disabled.
            drop_shadow: Option<([f32; 4], f32, f32, f32, f32)>,
            /// ([r,g,b,a], radius_doc) — soft fill-colored edge for object-blur /
            /// feather on solid fills. None when disabled or non-solid fill.
            soft_edge: Option<([f32; 4], f32)>,
            is_compound: bool,
            arrowhead_start: photonic_core::style::ArrowheadStyle,
            arrowhead_end: photonic_core::style::ArrowheadStyle,
            blend_mode: BlendMode,
            /// Enabled Color Overlay layer styles, as premultiplied-into-alpha
            /// RGBA (color.a × opacity × node opacity). Drawn over the fill.
            color_overlays: Vec<[f32; 4]>,
            /// Enabled solid Stroke layer styles: (width in doc units, RGBA).
            stroke_effects: Vec<(f32, [f32; 4])>,
            /// Dense ordinal of the owning layer among visible layers, in
            /// `layer_order`. Groups this node's geometry into a `LayerRun` so the
            /// layer can composite as an isolated unit (#226).
            layer_ordinal: u32,
        }

        let (artboard_w, artboard_h, artboards, layer_meta, nodes): (
            f32,
            f32,
            Vec<[f32; 4]>,
            Vec<(f32, BlendMode)>,
            Vec<NodeSnapshot>,
        ) = {
            // try_lock — never block; return cached geometry if lock is contended.
            let doc = match self.document.try_lock() {
                Ok(g) => g,
                Err(_) => {
                    tracing::debug!("render: doc lock contended — reusing cached geometry");
                    self.draw_segments = self.cached_segments.clone();
                    return (self.cached_vertices.clone(), self.cached_indices.clone());
                }
            };
            let w = doc.width as f32;
            let h = doc.height as f32;
            // Snapshot each artboard rect (x, y, w, h) in document space.
            let artboards: Vec<[f32; 4]> = doc
                .artboards
                .iter()
                .map(|a| [a.x as f32, a.y as f32, a.width as f32, a.height as f32])
                .collect();

            // Single pass: snapshot text nodes for glyphon and path nodes for
            // tessellation in one traversal, halving scene graph walk cost per frame.
            let zoom = self.view.zoom;
            let pan_x = self.view.pan_x;
            let pan_y = self.view.pan_y;
            self.pending_texts.clear();
            self.pending_path_text.clear();
            let mut nodes: Vec<NodeSnapshot> = Vec::new();

            // Tag every drawn node with its owning layer's dense ordinal (among
            // visible layers, in draw order) so build_geometry can group each
            // layer's geometry into a `LayerRun`. The isolated render path (#226)
            // then composites each non-trivial layer as its own unit — replacing
            // the old per-node opacity/blend fold approximation. `layer_meta[ord]`
            // holds that layer's (opacity, blend_mode).
            let mut node_ordinal: std::collections::HashMap<photonic_core::node::NodeId, u32> =
                std::collections::HashMap::new();
            let mut layer_meta: Vec<(f32, BlendMode)> = Vec::new();
            for lid in &doc.layer_order {
                if let Some(layer) = doc.layers.get(lid) {
                    if !layer.visible {
                        continue;
                    }
                    let ord = layer_meta.len() as u32;
                    layer_meta.push((layer.opacity, layer.blend_mode));
                    for n in doc.draw_nodes_in_layer(lid) {
                        node_ordinal.insert(n.id, ord);
                    }
                }
            }

            for node in doc.nodes_in_draw_order() {
                // Symbol instances render from the *current* master so master
                // edits and per-instance overrides take effect live.
                let orig_id = node.id;
                let node_layer_ord = node_ordinal.get(&orig_id).copied().unwrap_or(0);
                // Shapes get layer opacity/blend from the isolated composite path,
                // so they keep their own values. Text (glyphon flat text + vector
                // text-on-path) is drawn as an on-top overlay outside per-layer
                // isolation, so its layer's opacity is folded into its colour.
                let layer_opacity = layer_meta
                    .get(node_layer_ord as usize)
                    .map(|m| m.0)
                    .unwrap_or(1.0);
                let resolved = doc.resolve_render_node(node);
                let node = resolved.as_ref();
                match &node.kind {
                    SceneNodeKind::Text(text_node) => {
                        // Text-on-path: render glyph outlines along the spine as
                        // vector fills instead of flat glyphon text.
                        if let Some(spine_node) =
                            text_node.path_spine_id.and_then(|id| doc.get_node(&id))
                        {
                            if let SceneNodeKind::Path(spine) = &spine_node.kind {
                                let mut bez = spine.path_data.to_bez_path();
                                bez.apply_affine(kurbo::Affine::new(spine_node.transform.matrix));
                                let spine_doc = PathData::from_bez_path(&bez);

                                let opacity = text_node.fill.opacity * node.opacity * layer_opacity;
                                let rgba = match &text_node.fill.kind {
                                    FillKind::Solid(c) => [c.r, c.g, c.b, c.a * opacity],
                                    _ => [0.0, 0.0, 0.0, opacity],
                                };
                                let params = crate::text_path::TextOnPathParams {
                                    content: &text_node.content,
                                    font_family: &text_node.font_family,
                                    font_size: text_node.font_size,
                                    font_weight: text_node.font_weight,
                                    font_style: text_node.font_style,
                                    line_height: text_node.line_height,
                                    letter_spacing: text_node.letter_spacing,
                                    align: text_node.align,
                                    path_offset: text_node.path_offset,
                                };
                                let glyphs = crate::text_path::layout_text_on_path(
                                    &mut self.font_system,
                                    &params,
                                    &spine_doc,
                                );
                                if !glyphs.is_empty() {
                                    self.pending_path_text.push((glyphs, rgba));
                                }
                                continue; // handled — skip flat glyphon text
                            }
                        }
                        let (doc_x, doc_y) = node.transform.apply(0.0, 0.0);
                        let screen_x = (doc_x * zoom + pan_x) as f32;
                        let screen_y = (doc_y * zoom + pan_y) as f32;
                        let opacity = text_node.fill.opacity * node.opacity * layer_opacity;
                        let color = match &text_node.fill.kind {
                            FillKind::Solid(c) => [
                                (c.r * 255.0) as u8,
                                (c.g * 255.0) as u8,
                                (c.b * 255.0) as u8,
                                (c.a * opacity * 255.0) as u8,
                            ],
                            _ => [0, 0, 0, (opacity * 255.0) as u8],
                        };
                        let font_style_u8 = match text_node.font_style {
                            photonic_core::node::FontStyle::Normal => 0,
                            photonic_core::node::FontStyle::Italic => 1,
                            photonic_core::node::FontStyle::Oblique => 2,
                        };
                        // Advanced character metrics (node-level): super/subscript
                        // shrinks the whole node and offsets its baseline; an
                        // explicit baseline shift raises (positive) or lowers it.
                        let script = text_node.script_position;
                        let base_font_screen = text_node.font_size * zoom;
                        let effective_font_screen = base_font_screen * script.size_scale();
                        // Screen Y grows downward, so a *raise* is a negative offset.
                        let top_offset = (-(script.baseline_offset_em() * base_font_screen)
                            - text_node.baseline_shift * zoom)
                            as f32;
                        self.pending_texts.push(TextSnapshot {
                            content: text_node.content.clone(),
                            font_family: text_node.font_family.clone(),
                            font_size: effective_font_screen as f32,
                            line_height_mul: text_node.line_height as f32,
                            font_weight: text_node.font_weight,
                            font_style: font_style_u8,
                            color,
                            screen_x,
                            screen_y,
                            top_offset,
                        });
                    }
                    SceneNodeKind::Path(path_node) => {
                        let sc = &path_node.stroke;
                        let stroke_alpha = sc.color.a * sc.opacity * node.opacity;
                        nodes.push(NodeSnapshot {
                            node_id: orig_id,
                            matrix: node.transform.matrix,
                            fill_enabled: path_node.fill.enabled,
                            fill_kind: path_node.fill.kind.clone(),
                            fill_opacity: path_node.fill.opacity,
                            node_opacity: node.opacity,
                            fill_is_none: matches!(path_node.fill.kind, FillKind::None),
                            stroke_enabled: sc.enabled && sc.width > 0.0,
                            stroke_color: [sc.color.r, sc.color.g, sc.color.b, stroke_alpha],
                            stroke_paint: sc.paint.clone(),
                            stroke_paint_opacity: sc.opacity * node.opacity,
                            stroke_width: sc.width as f32,
                            stroke_cap: sc.line_cap,
                            stroke_join: sc.line_join,
                            stroke_miter: sc.miter_limit as f32,
                            stroke_align: sc.align,
                            stroke_widths: sc.width_profile_id.and_then(|id| {
                                doc.width_profiles
                                    .iter()
                                    .find(|p| p.id == id)
                                    .filter(|p| p.widths.len() >= 2)
                                    .map(|p| p.widths.clone())
                            }),
                            path_data: path_node.path_data.clone(),
                            is_compound: path_node.is_compound,
                            arrowhead_start: sc.arrowhead_start,
                            arrowhead_end: sc.arrowhead_end,
                            blend_mode: node.blend_mode,
                            color_overlays: node
                                .effects
                                .iter()
                                .filter_map(|e| match e {
                                    photonic_core::effects::LayerEffect::ColorOverlay(co)
                                        if co.enabled =>
                                    {
                                        let a = co.color.a * co.opacity * node.opacity;
                                        Some([co.color.r, co.color.g, co.color.b, a])
                                    }
                                    _ => None,
                                })
                                .collect(),
                            stroke_effects: node
                                .effects
                                .iter()
                                .filter_map(|e| match e {
                                    photonic_core::effects::LayerEffect::Stroke(st)
                                        if st.enabled =>
                                    {
                                        if let FillKind::Solid(c) = &st.fill.kind {
                                            let a =
                                                c.a * st.fill.opacity * st.opacity * node.opacity;
                                            Some((st.width, [c.r, c.g, c.b, a]))
                                        } else {
                                            None
                                        }
                                    }
                                    _ => None,
                                })
                                .collect(),
                            outer_glow: if node.outer_glow.enabled {
                                let c = &node.outer_glow.color;
                                Some((
                                    [
                                        c.r,
                                        c.g,
                                        c.b,
                                        c.a,
                                        node.outer_glow.opacity,
                                        node.outer_glow.size,
                                    ],
                                    node.outer_glow.join,
                                ))
                            } else {
                                None
                            },
                            inner_glow: if node.inner_glow.enabled {
                                let c = &node.inner_glow.color;
                                Some((
                                    [
                                        c.r,
                                        c.g,
                                        c.b,
                                        c.a,
                                        node.inner_glow.opacity,
                                        node.inner_glow.size,
                                    ],
                                    node.inner_glow.join,
                                ))
                            } else {
                                None
                            },
                            gaussian_glow: if node.gaussian_glow.enabled {
                                let c = &node.gaussian_glow.color;
                                let a = c.a * node.gaussian_glow.opacity * node.opacity;
                                Some(([c.r, c.g, c.b, a], node.gaussian_glow.radius))
                            } else {
                                None
                            },
                            drop_shadow: if node.drop_shadow.enabled {
                                let s = &node.drop_shadow;
                                Some((
                                    [s.color.r, s.color.g, s.color.b, s.color.a],
                                    s.opacity * node.opacity,
                                    s.dx,
                                    s.dy,
                                    s.blur,
                                ))
                            } else {
                                None
                            },
                            soft_edge: {
                                let radius = if node.object_blur.enabled {
                                    node.object_blur.radius
                                } else if node.feather.enabled {
                                    node.feather.radius
                                } else {
                                    0.0
                                };
                                match (&path_node.fill.kind, radius) {
                                    (FillKind::Solid(c), r) if r > 0.0 => {
                                        let a = c.a * path_node.fill.opacity * node.opacity;
                                        Some(([c.r, c.g, c.b, a], r))
                                    }
                                    _ => None,
                                }
                            },
                            layer_ordinal: node_layer_ord,
                        });
                    }
                    _ => {} // Group nodes and future kinds: no GPU geometry of their own
                }
            }
            (w, h, artboards, layer_meta, nodes)
        }; // doc lock released here

        // ── Tessellate phase: all CPU work happens with no locks held ─────────
        self.pending_gaussian_glows.clear();
        let mut blur_jobs: Vec<BlurJob> = Vec::new();
        let mut verts: Vec<Vertex> = Vec::new();
        let mut idxs: Vec<u32> = Vec::new();

        // White artboard rectangles — one per artboard (spatial multi-artboard
        // model). Falls back to the full document bounds when none are present.
        let white = [1.0f32, 1.0, 1.0, 1.0];
        let mut boards = artboards;
        // A transparent export suppresses the white board quads entirely so the
        // background reads through at alpha 0; every other frame (editor and
        // opaque-artboard export) keeps them.
        if matches!(self.export_bg, Some(ExportBackground::Transparent)) {
            boards.clear();
        } else if boards.is_empty() {
            boards.push([0.0, 0.0, artboard_w, artboard_h]);
        }
        for b in &boards {
            let (x0, y0, x1, y1) = (b[0], b[1], b[0] + b[2], b[1] + b[3]);
            let base = verts.len() as u32;
            verts.extend_from_slice(&[
                Vertex {
                    position: [x0, y0],
                    color: white,
                },
                Vertex {
                    position: [x1, y0],
                    color: white,
                },
                Vertex {
                    position: [x1, y1],
                    color: white,
                },
                Vertex {
                    position: [x0, y1],
                    color: white,
                },
            ]);
            idxs.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        // The artboard-background quads are the preamble drawn before any layer.
        let artboard_idx_end = idxs.len() as u32;

        // Document-space bounds of a local mesh under an affine transform.
        fn world_bounds(
            verts: &[[f32; 2]],
            a: f64,
            b: f64,
            c: f64,
            d: f64,
            e: f64,
            f: f64,
        ) -> (f64, f64, f64, f64) {
            let (mut minx, mut miny, mut maxx, mut maxy) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for p in verts {
                let x = a * p[0] as f64 + c * p[1] as f64 + e;
                let y = b * p[0] as f64 + d * p[1] as f64 + f;
                minx = minx.min(x);
                miny = miny.min(y);
                maxx = maxx.max(x);
                maxy = maxy.max(y);
            }
            if verts.is_empty() {
                (0.0, 0.0, 1.0, 1.0)
            } else {
                (minx, miny, maxx, maxy)
            }
        }
        // Local (pre-transform) bounds of a mesh.
        fn local_bounds(verts: &[[f32; 2]]) -> (f64, f64, f64, f64) {
            let (mut minx, mut miny, mut maxx, mut maxy) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for p in verts {
                minx = minx.min(p[0] as f64);
                miny = miny.min(p[1] as f64);
                maxx = maxx.max(p[0] as f64);
                maxy = maxy.max(p[1] as f64);
            }
            if verts.is_empty() {
                (0.0, 0.0, 1.0, 1.0)
            } else {
                (minx, miny, maxx, maxy)
            }
        }

        // Take the tessellation memo out of `self` so the append_* closures can
        // share it via interior mutability without each borrowing `&mut self`
        // (03 §2.2). `clear_cache` (a ring overflow / lost baseline) drops the
        // whole memo; otherwise content-addressing invalidates only what changed.
        let mut tess_owned = std::mem::take(&mut self.tess_cache);
        if clear_cache {
            tess_owned = TessCache::default();
        }
        tess_owned.begin_frame();
        let tess = RefCell::new(tess_owned);

        // Helpers for appending tessellated meshes to the vertex/index buffers.
        // Defined as closures to keep the loop body readable. Each fetches its
        // mesh from the memo (`svg_hash` = the node's resolved-path hash, cached
        // once per node) instead of tessellating inline.
        let append_fill = |node: &NodeSnapshot,
                           svg_hash: u64,
                           verts: &mut Vec<Vertex>,
                           idxs: &mut Vec<u32>| {
            if !node.fill_enabled || node.fill_is_none {
                return;
            }
            // Object-blur / feather replace the sharp fill with a blurred copy
            // in the effects layer — suppress the crisp fill here.
            if node.soft_edge.is_some() {
                return;
            }
            let [a, b, c, d, e, f] = node.matrix;
            let opacity = node.fill_opacity * node.node_opacity;
            let mesh_arc =
                tess.borrow_mut()
                    .fill(node.node_id, &node.path_data, svg_hash, node.is_compound);
            if mesh_arc.is_empty() {
                return;
            }
            // Non-linear fills (radial/fluid/mesh/pattern) sample per vertex, so
            // refine the coarse fill triangulation for a smooth result. The
            // coarse mesh is the cached one; refinement is per-frame (it depends
            // on the fill kind, not just the path).
            let refined;
            let mesh: &crate::tessellator::Mesh = if node.fill_kind.is_nonlinear() {
                let (lx, ly, lxx, lyy) = local_bounds(&mesh_arc.vertices);
                let maxdim = ((lxx - lx).max(lyy - ly)) as f32;
                refined = crate::tessellator::refine_mesh(&mesh_arc, (maxdim / 48.0).max(1.0));
                &refined
            } else {
                &mesh_arc
            };
            // Object-space gradients resolve against the fill's bbox so they
            // track the object. Rotation-following gradients resolve in the
            // object's local space (so they rotate/shear with it); others use
            // the document-space AABB.
            let rotated = node.fill_kind.gradient_follows_rotation();
            let (bx, by, bw, bh) = if rotated {
                let (lx, ly, lxx, lyy) = local_bounds(&mesh.vertices);
                (lx, ly, lxx - lx, lyy - ly)
            } else {
                let (minx, miny, maxx, maxy) = world_bounds(&mesh.vertices, a, b, c, d, e, f);
                (minx, miny, maxx - minx, maxy - miny)
            };
            let fill_kind = node.fill_kind.for_bbox(bx, by, bw, bh);

            // Mesh (spreadsheet-grid) fills: cut the triangles along the grid
            // lines so cell boundaries are clean straight edges, and sample each
            // triangle from just inside its own cell.
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
                let sample_coord = |p: &[f32; 2]| -> [f64; 2] {
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
                            sample_coord(&mesh.vertices[t[0] as usize]),
                            sample_coord(&mesh.vertices[t[1] as usize]),
                            sample_coord(&mesh.vertices[t[2] as usize]),
                        ]
                    })
                    .collect();
                crate::tessellator::cut_triangles(&mut tris, &xs, &ys);
                for tri in &tris {
                    let cx = (tri[0][0] + tri[1][0] + tri[2][0]) / 3.0;
                    let cy = (tri[0][1] + tri[1][1] + tri[2][1]) / 3.0;
                    let base = verts.len() as u32;
                    for p in tri {
                        // Nudge toward the centroid so boundary vertices sample
                        // inside their own cell (crisp edges at blend 0).
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
                return;
            }

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
        };

        // Render N layered strokes to approximate a Gaussian glow.
        // Drawing order: largest (faintest) first so smaller brighter layers overwrite near the edge.
        const GLOW_STEPS: usize = 10;
        let append_glow = |node_id: NodeId,
                           path_data: &photonic_core::path::PathData,
                           svg_hash: u64,
                           matrix: &[f64; 6],
                           glow: &[f32; 6],
                           join: photonic_core::style::LineJoin,
                           verts: &mut Vec<Vertex>,
                           idxs: &mut Vec<u32>| {
            let [gr, gg, gb, ga, go, gs] = *glow;
            let [a, b, c, d, e, f] = *matrix;
            for i in (0..GLOW_STEPS).rev() {
                // t goes from 1.0 (outermost, widest) down to 1/N (innermost)
                let t = (i + 1) as f32 / GLOW_STEPS as f32;
                let width = 2.0 * gs * t;
                // Gaussian falloff: alpha peaks at edge (small t) and fades outward
                let gaussian = (-4.5 * t * t).exp();
                let step_alpha = (go * gaussian * ga).min(1.0);
                let color = [gr, gg, gb, step_alpha];
                let mesh = tess.borrow_mut().stroke(
                    node_id,
                    path_data,
                    svg_hash,
                    width,
                    photonic_core::style::LineCap::Round,
                    join,
                    4.0,
                );
                if mesh.is_empty() {
                    continue;
                }
                let base = verts.len() as u32;
                for pos in &mesh.vertices {
                    let x = a * pos[0] as f64 + c * pos[1] as f64 + e;
                    let y = b * pos[0] as f64 + d * pos[1] as f64 + f;
                    verts.push(Vertex {
                        position: [x as f32, y as f32],
                        color,
                    });
                }
                for &idx in &mesh.indices {
                    idxs.push(base + idx);
                }
            }
        };

        let append_stroke = |node: &NodeSnapshot,
                             svg_hash: u64,
                             width: f32,
                             verts: &mut Vec<Vertex>,
                             idxs: &mut Vec<u32>| {
            if !node.stroke_enabled {
                return;
            }
            let [a, b, c, d, e, f] = node.matrix;
            // Non-scaling stroke: a stroke's width is an absolute property,
            // not part of the geometry, so it must NOT grow/shrink when the
            // object's transform is scaled (Illustrator with "Scale Strokes
            // & Effects" off). The mesh is built in local space and then
            // multiplied by `matrix`, so pre-divide the width by the
            // transform's uniform scale `sqrt(|det|)` to cancel it. This is
            // a no-op for unscaled or purely-rotated/translated objects
            // (det == 1) and leaves view zoom untouched.
            let obj_scale = (a * d - b * c).abs().sqrt().max(1e-6);
            let width = width / obj_scale as f32;
            let mesh = match &node.stroke_widths {
                // Variable-width profile: scale samples by the same factor the
                // caller applies to the uniform width (stroke-align doubling),
                // then build a filled ribbon outline.
                Some(widths) if node.stroke_width > 0.0 => {
                    let scale = (width / node.stroke_width) as f64;
                    let scaled: Vec<f64> = widths.iter().map(|w| w * scale).collect();
                    tess.borrow_mut().stroke_variable(
                        node.node_id,
                        &node.path_data,
                        svg_hash,
                        &scaled,
                    )
                }
                _ => tess.borrow_mut().stroke(
                    node.node_id,
                    &node.path_data,
                    svg_hash,
                    width,
                    node.stroke_cap,
                    node.stroke_join,
                    node.stroke_miter,
                ),
            };
            if mesh.is_empty() {
                return;
            }
            let base = verts.len() as u32;
            for pos in &mesh.vertices {
                let x = a * pos[0] as f64 + c * pos[1] as f64 + e;
                let y = b * pos[0] as f64 + d * pos[1] as f64 + f;
                // Gradient/pattern stroke paint (#201): sample per vertex,
                // exactly like the fill path. `None` = flat stroke color.
                let color = match &node.stroke_paint {
                    Some(kind) => kind.sample_at(x, y, node.stroke_paint_opacity),
                    None => node.stroke_color,
                };
                verts.push(Vertex {
                    position: [x as f32, y as f32],
                    color,
                });
            }
            for &i in &mesh.indices {
                idxs.push(base + i);
            }
        };

        // ── Arrowhead helper ──────────────────────────────────────────────────
        // Appends a filled triangular or open-V arrowhead at the given world-space
        // endpoint `(px, py)` oriented toward direction `(dx, dy)` (not normalized).
        let append_arrowhead_triangle =
            |px: f64,
             py: f64,
             dx: f64,
             dy: f64,
             half_w: f64,
             length: f64,
             color: [f32; 4],
             style: photonic_core::style::ArrowheadStyle,
             verts: &mut Vec<Vertex>,
             idxs: &mut Vec<u32>| {
                use photonic_core::style::ArrowheadStyle;
                let len = (dx * dx + dy * dy).sqrt();
                if len < 1e-9 {
                    return;
                }
                let (ux, uy) = (dx / len, dy / len); // unit tangent toward tip
                let (nx, ny) = (-uy, ux); // unit normal

                match style {
                    ArrowheadStyle::FilledArrow => {
                        // Solid filled triangle: tip at (px,py), base behind by `length`.
                        let tip = (px, py);
                        let base = (px - ux * length, py - uy * length);
                        let left = (base.0 + nx * half_w, base.1 + ny * half_w);
                        let right = (base.0 - nx * half_w, base.1 - ny * half_w);
                        let base_idx = verts.len() as u32;
                        verts.push(Vertex {
                            position: [tip.0 as f32, tip.1 as f32],
                            color,
                        });
                        verts.push(Vertex {
                            position: [left.0 as f32, left.1 as f32],
                            color,
                        });
                        verts.push(Vertex {
                            position: [right.0 as f32, right.1 as f32],
                            color,
                        });
                        idxs.extend_from_slice(&[base_idx, base_idx + 1, base_idx + 2]);
                    }
                    ArrowheadStyle::OpenArrow => {
                        // Two thin quads forming an open V. Each "line" is `line_w` wide.
                        let line_w = half_w * 0.25;
                        let tip = (px, py);
                        let left = (
                            px - ux * length + nx * half_w,
                            py - uy * length + ny * half_w,
                        );
                        let right = (
                            px - ux * length - nx * half_w,
                            py - uy * length - ny * half_w,
                        );

                        // Draw each arm as a thin quad (four verts, two tris).
                        let draw_arm =
                            |a: (f64, f64),
                             b: (f64, f64),
                             verts: &mut Vec<Vertex>,
                             idxs: &mut Vec<u32>| {
                                let adx = b.0 - a.0;
                                let ady = b.1 - a.1;
                                let al = (adx * adx + ady * ady).sqrt().max(1e-12);
                                let (anx, any) = (-ady / al, adx / al);
                                let p0 = (a.0 + anx * line_w, a.1 + any * line_w);
                                let p1 = (a.0 - anx * line_w, a.1 - any * line_w);
                                let p2 = (b.0 - anx * line_w, b.1 - any * line_w);
                                let p3 = (b.0 + anx * line_w, b.1 + any * line_w);
                                let bi = verts.len() as u32;
                                for (px2, py2) in [p0, p1, p2, p3] {
                                    verts.push(Vertex {
                                        position: [px2 as f32, py2 as f32],
                                        color,
                                    });
                                }
                                idxs.extend_from_slice(&[bi, bi + 1, bi + 2, bi, bi + 2, bi + 3]);
                            };
                        draw_arm(tip, left, verts, idxs);
                        draw_arm(tip, right, verts, idxs);
                    }
                    ArrowheadStyle::None => {}
                }
            };

        // Extract start and end endpoint + tangent from a BezPath.
        // Returns (start_pt, start_tangent, end_pt, end_tangent) as Option tuples.
        let path_endpoints =
            |bez: &kurbo::BezPath| -> Option<((f64, f64), (f64, f64), (f64, f64), (f64, f64))> {
                use kurbo::PathEl;
                let els: Vec<_> = bez.elements().to_vec();
                if els.is_empty() {
                    return None;
                }
                let mut start_pt = (0.0f64, 0.0f64);
                let mut start_tan = (1.0f64, 0.0f64);
                let mut end_pt = (0.0f64, 0.0f64);
                let mut end_tan = (1.0f64, 0.0f64);
                let mut cur = (0.0f64, 0.0f64);
                let mut found_start = false;
                for el in &els {
                    match el {
                        PathEl::MoveTo(p) => {
                            cur = (p.x, p.y);
                        }
                        PathEl::LineTo(p) => {
                            if !found_start {
                                start_pt = cur;
                                start_tan = (p.x - cur.0, p.y - cur.1);
                                found_start = true;
                            }
                            end_pt = (p.x, p.y);
                            end_tan = (p.x - cur.0, p.y - cur.1);
                            cur = (p.x, p.y);
                        }
                        PathEl::QuadTo(c, p) => {
                            if !found_start {
                                start_pt = cur;
                                start_tan = (c.x - cur.0, c.y - cur.1);
                                found_start = true;
                            }
                            end_pt = (p.x, p.y);
                            end_tan = (p.x - c.x, p.y - c.y);
                            cur = (p.x, p.y);
                        }
                        PathEl::CurveTo(c1, c2, p) => {
                            if !found_start {
                                start_pt = cur;
                                start_tan = (c1.x - cur.0, c1.y - cur.1);
                                found_start = true;
                            }
                            end_pt = (p.x, p.y);
                            end_tan = (p.x - c2.x, p.y - c2.y);
                            cur = (p.x, p.y);
                        }
                        PathEl::ClosePath => {}
                    }
                }
                if !found_start {
                    return None;
                }
                Some((start_pt, start_tan, end_pt, end_tan))
            };

        // Per-node index ranges tagged with their blend mode, coalesced into
        // `draw_segments` after the loop. The artboard rect (already appended
        // above) blends normally.
        let mut raw_segments: Vec<(BlendMode, u32, u32)> =
            vec![(BlendMode::Normal, 0, idxs.len() as u32)];

        // Build a blurred-silhouette effect job: tessellate the fill, transform
        // by the node matrix (+ offset), flat-colour it, tag with a blur radius.
        let make_blur_job = |node: &NodeSnapshot,
                             svg_hash: u64,
                             offset: (f64, f64),
                             color: [f32; 4],
                             radius_doc: f64|
         -> Option<BlurJob> {
            let mesh =
                tess.borrow_mut()
                    .fill(node.node_id, &node.path_data, svg_hash, node.is_compound);
            if mesh.is_empty() {
                return None;
            }
            let [a, b, c, d, e, f] = node.matrix;
            let (ox, oy) = offset;
            let mut jverts = Vec::with_capacity(mesh.vertices.len());
            for pos in &mesh.vertices {
                let x = a * pos[0] as f64 + c * pos[1] as f64 + e + ox;
                let y = b * pos[0] as f64 + d * pos[1] as f64 + f + oy;
                jverts.push(Vertex {
                    position: [x as f32, y as f32],
                    color,
                });
            }
            Some(BlurJob {
                verts: jverts,
                idxs: mesh.indices.clone(),
                radius_doc,
                layer_ordinal: node.layer_ordinal,
            })
        };

        // Group each layer's node geometry into a contiguous `LayerRun` (nodes are
        // emitted in draw order grouped by layer). Consumed by the isolated
        // render path when any layer is non-trivial (#226).
        let mut layer_runs: Vec<LayerRun> = Vec::new();
        let mut cur_ord: Option<u32> = None;
        let mut run_start = idxs.len() as u32;
        let close_run = |runs: &mut Vec<LayerRun>, ord: u32, start: u32, end: u32| {
            if end <= start {
                return;
            }
            let (opacity, blend) = layer_meta
                .get(ord as usize)
                .copied()
                .unwrap_or((1.0, BlendMode::Normal));
            runs.push(LayerRun {
                opacity,
                blend,
                idx_start: start,
                idx_end: end,
                ordinal: ord,
            });
        };

        for node in &nodes {
            let seg_start = idxs.len() as u32;
            // Hash the resolved path once per node; every fill/stroke/glow fetch
            // for this node reuses it as the cache-key prefix (03 §2.2).
            let svg_hash = hash_svg(node.path_data.as_svg());

            // Close the previous layer's run at this layer boundary.
            if cur_ord != Some(node.layer_ordinal) {
                if let Some(ord) = cur_ord {
                    close_run(&mut layer_runs, ord, run_start, seg_start);
                }
                cur_ord = Some(node.layer_ordinal);
                run_start = seg_start;
            }

            // ── Drop shadow → blurred offset silhouette in the effects layer ───
            if let Some(([sr, sg, sb, sa], opacity, dx, dy, blur)) = node.drop_shadow {
                let alpha = (sa * opacity).min(1.0);
                if let Some(job) = make_blur_job(
                    node,
                    svg_hash,
                    (dx as f64, dy as f64),
                    [sr, sg, sb, alpha],
                    blur as f64,
                ) {
                    blur_jobs.push(job);
                }
            }

            // ── Object blur / feather → blurred fill in the effects layer ──────
            // The sharp fill is suppressed in append_fill; this blurred copy
            // replaces it. (Gradient/image interior blur is a follow-up.)
            if let Some(([r, g, b, a], radius)) = node.soft_edge {
                if let Some(job) =
                    make_blur_job(node, svg_hash, (0.0, 0.0), [r, g, b, a], radius as f64)
                {
                    blur_jobs.push(job);
                }
            }

            // ── Outer glow: behind fill so fill clips the inward half ─────────
            if let Some((ref og, og_join)) = node.outer_glow {
                append_glow(
                    node.node_id,
                    &node.path_data,
                    svg_hash,
                    &node.matrix,
                    og,
                    og_join,
                    &mut verts,
                    &mut idxs,
                );
            }

            match node.stroke_align {
                // Outside: render doubled-width stroke first, then fill on top.
                // The fill paints over the inner half of the stroke, leaving only
                // the outer half visible.
                StrokeAlign::Outside => {
                    append_stroke(
                        node,
                        svg_hash,
                        node.stroke_width * 2.0,
                        &mut verts,
                        &mut idxs,
                    );
                    append_fill(node, svg_hash, &mut verts, &mut idxs);
                    // Inner glow: render after fill, then re-clip with fill
                    if let Some((ref ig, ig_join)) = node.inner_glow {
                        append_glow(
                            node.node_id,
                            &node.path_data,
                            svg_hash,
                            &node.matrix,
                            ig,
                            ig_join,
                            &mut verts,
                            &mut idxs,
                        );
                        append_fill(node, svg_hash, &mut verts, &mut idxs);
                    }
                }
                // Center: fill first, then stroke centred on the path edge.
                StrokeAlign::Center => {
                    append_fill(node, svg_hash, &mut verts, &mut idxs);
                    // Inner glow: rendered over fill, re-clipped before stroke
                    if let Some((ref ig, ig_join)) = node.inner_glow {
                        append_glow(
                            node.node_id,
                            &node.path_data,
                            svg_hash,
                            &node.matrix,
                            ig,
                            ig_join,
                            &mut verts,
                            &mut idxs,
                        );
                        append_fill(node, svg_hash, &mut verts, &mut idxs);
                    }
                    append_stroke(node, svg_hash, node.stroke_width, &mut verts, &mut idxs);
                }
                // Inside: fill, then doubled-width stroke, then fill again.
                // The second fill paints over the outer half of the stroke, leaving
                // only the inner half visible.
                StrokeAlign::Inside => {
                    append_fill(node, svg_hash, &mut verts, &mut idxs);
                    // Inner glow: before the inside stroke
                    if let Some((ref ig, ig_join)) = node.inner_glow {
                        append_glow(
                            node.node_id,
                            &node.path_data,
                            svg_hash,
                            &node.matrix,
                            ig,
                            ig_join,
                            &mut verts,
                            &mut idxs,
                        );
                        append_fill(node, svg_hash, &mut verts, &mut idxs);
                    }
                    append_stroke(
                        node,
                        svg_hash,
                        node.stroke_width * 2.0,
                        &mut verts,
                        &mut idxs,
                    );
                    // Re-draw fill to clip the outer half of the stroke.
                    if node.stroke_enabled {
                        append_fill(node, svg_hash, &mut verts, &mut idxs);
                    }
                }
            }

            // ── Color Overlay layer style ─────────────────────────────────────
            // The shape's silhouette recoloured, drawn over the fill. Part of the
            // node's blend segment (exact for Normal/opaque overlays — the common
            // case; exotic overlay blend modes preview approximately, like the rest
            // of the live path, but export exactly via the CPU compositor).
            for &rgba in &node.color_overlays {
                if rgba[3] <= 0.0 {
                    continue;
                }
                let mesh = tess.borrow_mut().fill(
                    node.node_id,
                    &node.path_data,
                    svg_hash,
                    node.is_compound,
                );
                if mesh.is_empty() {
                    continue;
                }
                let [a, b, c, d, e, f] = node.matrix;
                let base = verts.len() as u32;
                for v in &mesh.vertices {
                    let x = a * v[0] as f64 + c * v[1] as f64 + e;
                    let y = b * v[0] as f64 + d * v[1] as f64 + f;
                    verts.push(Vertex {
                        position: [x as f32, y as f32],
                        color: rgba,
                    });
                }
                for &i in &mesh.indices {
                    idxs.push(base + i);
                }
            }

            // ── Stroke layer style (solid, centred) ───────────────────────────
            for &(width, rgba) in &node.stroke_effects {
                if width <= 0.0 || rgba[3] <= 0.0 {
                    continue;
                }
                let [a, b, c, d, e, f] = node.matrix;
                let obj_scale = (a * d - b * c).abs().sqrt().max(1e-6);
                let mesh = tess.borrow_mut().stroke(
                    node.node_id,
                    &node.path_data,
                    svg_hash,
                    (width as f64 / obj_scale) as f32,
                    photonic_core::style::LineCap::Butt,
                    photonic_core::style::LineJoin::Miter,
                    4.0,
                );
                if mesh.is_empty() {
                    continue;
                }
                let base = verts.len() as u32;
                for v in &mesh.vertices {
                    let x = a * v[0] as f64 + c * v[1] as f64 + e;
                    let y = b * v[0] as f64 + d * v[1] as f64 + f;
                    verts.push(Vertex {
                        position: [x as f32, y as f32],
                        color: rgba,
                    });
                }
                for &i in &mesh.indices {
                    idxs.push(base + i);
                }
            }

            // ── Arrowheads ────────────────────────────────────────────────────
            if node.stroke_enabled
                && (node.arrowhead_start != photonic_core::style::ArrowheadStyle::None
                    || node.arrowhead_end != photonic_core::style::ArrowheadStyle::None)
            {
                let bez = node.path_data.to_bez_path();
                if let Some((s_pt, s_tan, e_pt, e_tan)) = path_endpoints(&bez) {
                    let [a, b, c, d, e, f] = node.matrix;
                    let transform_pt = |px: f64, py: f64| -> (f64, f64) {
                        (a * px + c * py + e, b * px + d * py + f)
                    };
                    let transform_dir =
                        |dx: f64, dy: f64| -> (f64, f64) { (a * dx + c * dy, b * dx + d * dy) };
                    let w = node.stroke_width as f64;
                    let arrow_len = w * 3.5;
                    let arrow_hw = w * 1.5;
                    let color = node.stroke_color;

                    if node.arrowhead_start != photonic_core::style::ArrowheadStyle::None {
                        let (wx, wy) = transform_pt(s_pt.0, s_pt.1);
                        // Negate tangent so arrow points outward (away from path interior).
                        let (tdx, tdy) = transform_dir(-s_tan.0, -s_tan.1);
                        append_arrowhead_triangle(
                            wx,
                            wy,
                            tdx,
                            tdy,
                            arrow_hw,
                            arrow_len,
                            color,
                            node.arrowhead_start,
                            &mut verts,
                            &mut idxs,
                        );
                    }
                    if node.arrowhead_end != photonic_core::style::ArrowheadStyle::None {
                        let (wx, wy) = transform_pt(e_pt.0, e_pt.1);
                        let (tdx, tdy) = transform_dir(e_tan.0, e_tan.1);
                        append_arrowhead_triangle(
                            wx,
                            wy,
                            tdx,
                            tdy,
                            arrow_hw,
                            arrow_len,
                            color,
                            node.arrowhead_end,
                            &mut verts,
                            &mut idxs,
                        );
                    }
                }
            }

            // ── Gaussian glow job ─────────────────────────────────────────────
            if let Some(([gr, gg, gb, ga], radius_doc)) = node.gaussian_glow {
                let [a, b, c, d, e, f] = node.matrix;
                let mesh = tess.borrow_mut().fill(
                    node.node_id,
                    &node.path_data,
                    svg_hash,
                    node.is_compound,
                );
                if !mesh.is_empty() {
                    let glow_color = [gr, gg, gb, ga];
                    let mut gverts = Vec::with_capacity(mesh.vertices.len());
                    for pos in &mesh.vertices {
                        let x = (a * pos[0] as f64 + c * pos[1] as f64 + e) as f32;
                        let y = (b * pos[0] as f64 + d * pos[1] as f64 + f) as f32;
                        gverts.push(Vertex {
                            position: [x, y],
                            color: glow_color,
                        });
                    }
                    let sigma_px = (radius_doc as f64 * self.view.zoom) as f32;
                    self.pending_gaussian_glows.push(GaussianGlowJob {
                        verts: gverts,
                        idxs: mesh.indices.clone(),
                        sigma_px,
                    });
                }
            }

            // Tag this node's fill/stroke/arrowhead geometry with its blend mode.
            // (Gaussian glow lives in a separate buffer and is unaffected.)
            raw_segments.push((node.blend_mode, seg_start, idxs.len() as u32));
        }
        // Close the final layer run (text-on-path glyphs below are drawn on top,
        // outside any layer, so the run ends here).
        if let Some(ord) = cur_ord {
            close_run(&mut layer_runs, ord, run_start, idxs.len() as u32);
        }

        // ── Text-on-path glyphs ───────────────────────────────────────────────
        // Glyph outlines are already in document coordinates, so they are
        // appended directly (no per-node matrix). Drawn last → on top of shapes,
        // consistent with how flat text sits above the fill layer.
        let text_seg_start = idxs.len() as u32;
        for (glyphs, rgba) in &self.pending_path_text {
            for glyph in glyphs {
                let mesh = tessellate_fill(
                    glyph,
                    false,
                    adaptive_tolerance(self.view.zoom, &[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
                );
                if mesh.is_empty() {
                    continue;
                }
                let base = verts.len() as u32;
                for pos in &mesh.vertices {
                    verts.push(Vertex {
                        position: [pos[0], pos[1]],
                        color: *rgba,
                    });
                }
                for &i in &mesh.indices {
                    idxs.push(base + i);
                }
            }
        }
        // Cover the appended glyph indices with a Normal-blend segment so the
        // separable-blend segment draw pass actually renders them (untagged
        // index ranges are skipped). Glyphs blend Normal, on top of everything.
        if (idxs.len() as u32) > text_seg_start {
            raw_segments.push((BlendMode::Normal, text_seg_start, idxs.len() as u32));
            // These glyphs sit *after* every layer run, so the isolated render
            // path (which draws only artboard + layer runs) would drop them.
            // Add them as a trailing Normal overlay "layer" that composites on
            // top (their per-layer opacity is already folded into the colour).
            layer_runs.push(LayerRun {
                opacity: 1.0,
                blend: BlendMode::Normal,
                idx_start: text_seg_start,
                idx_end: idxs.len() as u32,
                ordinal: u32::MAX, // sentinel: matches no blur-effect jobs
            });
        }

        let segments = coalesce_segments(raw_segments);
        self.pending_blur_jobs = blur_jobs;
        self.layer_runs = layer_runs;
        self.artboard_idx_end = artboard_idx_end;

        // Reclaim the tessellation memo: evict entries not used this frame, record
        // the perf instrumentation, and record the revision/view baseline so the
        // next frame's skip check has something to compare against. No closure is
        // executing here, so the runtime borrow is uncontended.
        {
            let mut cache = tess.borrow_mut();
            cache.sweep();
            let (nodes_retess, calls) = cache.stats();
            self.last_tess_nodes = nodes_retess;
            self.last_tess_calls = calls;
            self.tess_cache = std::mem::take(&mut cache);
        }
        self.last_revision = revision;
        self.last_view = Some(view_key);

        // Update cache for next frame (used when lock is contended).
        self.cached_vertices = verts.clone();
        self.cached_indices = idxs.clone();
        self.draw_segments = segments.clone();
        self.cached_segments = segments;

        (verts, idxs)
    }

    /// `(distinct_nodes_re_tessellated, tessellate_calls)` for the most recent
    /// `build_geometry` — zero on an idle frame-skip. Used by the render-perf
    /// statement and tests (03 §2.2).
    pub fn last_frame_tess_stats(&self) -> (u32, u32) {
        (self.last_tess_nodes, self.last_tess_calls)
    }
}

#[cfg(test)]
mod surface_config_tests {
    use super::*;

    fn caps(present_modes: Vec<wgpu::PresentMode>) -> wgpu::SurfaceCapabilities {
        wgpu::SurfaceCapabilities {
            formats: vec![
                wgpu::TextureFormat::Rgba8UnormSrgb,
                wgpu::TextureFormat::Bgra8Unorm,
            ],
            present_modes,
            alpha_modes: vec![wgpu::CompositeAlphaMode::Opaque],
            usages: wgpu::TextureUsages::RENDER_ATTACHMENT,
        }
    }

    #[test]
    fn surface_configuration_prefers_an_advertised_fifo_mode_and_linear_format() {
        let config = choose_surface_config(
            &caps(vec![wgpu::PresentMode::Immediate, wgpu::PresentMode::Fifo]),
            640,
            480,
        )
        .expect("compatible caps");

        assert_eq!(config.present_mode, wgpu::PresentMode::Fifo);
        assert_eq!(config.format, wgpu::TextureFormat::Bgra8Unorm);
    }

    #[test]
    fn surface_configuration_rejects_an_adapter_without_present_modes() {
        let error = choose_surface_config(&caps(vec![]), 640, 480).expect_err("no present mode");
        assert!(error.to_string().contains("presentation modes"));
    }

    #[test]
    fn adapter_fallback_order_prefers_hardware() {
        assert!(
            adapter_rank(wgpu::DeviceType::DiscreteGpu)
                < adapter_rank(wgpu::DeviceType::IntegratedGpu)
        );
        assert!(
            adapter_rank(wgpu::DeviceType::IntegratedGpu) < adapter_rank(wgpu::DeviceType::Cpu)
        );
    }
}

#[cfg(test)]
mod scene_format_tests {
    use super::*;

    /// Build a windowless renderer, or `None` on a machine without a GPU adapter
    /// (headless CI) so the test skips cleanly — the same convention as the
    /// offscreen capture tests (`capture.rs`'s `offscreen`).
    fn try_offscreen() -> Option<PhotonicRenderer> {
        let (_tx, rx) = std::sync::mpsc::channel();
        let doc = Document::new("scene_fmt", 8.0, 8.0);
        pollster::block_on(PhotonicRenderer::new_offscreen(
            8,
            8,
            Arc::new(Mutex::new(doc)),
            rx,
        ))
    }

    /// The document fill/blend pass MUST render in an sRGB format so blending runs
    /// in linear light (03 §4.5.1). This replaces `pipeline.rs`'s vacuous
    /// `windowed_scene_format_derivation_is_srgb` (which asserted a property of
    /// `TextureFormat::add_srgb_suffix()`, a stdlib function the crate never
    /// calls, and so guarded nothing): it pins the *renderer's* exposed scene
    /// format, failing if a future change ever points the document pass back at
    /// the non-sRGB presentation surface format.
    #[test]
    fn offscreen_document_target_is_srgb() {
        let Some(r) = try_offscreen() else {
            eprintln!("no GPU adapter — skipping scene_format invariant test");
            return;
        };
        assert_eq!(
            r.scene_format(),
            crate::pipeline::SCENE_FORMAT,
            "document pass must target pipeline::SCENE_FORMAT",
        );
        assert!(
            r.scene_format().is_srgb(),
            "scene_format must be sRGB so document blending runs in linear light",
        );
    }
}

pub(crate) fn align256(n: u32) -> u32 {
    (n + 255) & !255
}

pub(crate) fn create_msaa_texture(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("msaa_texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: MSAA_SAMPLES,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    (texture, view)
}

/// The offscreen document target (03 §4.5.4). Single-sample sRGB `scene_format`,
/// usable as a render-pass resolve target, a sampled source (for the blit) and a
/// readback source.
pub(crate) fn create_scene_texture(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("scene_texture"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    (texture, view)
}

pub(crate) fn create_glow_textures(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> (
    wgpu::Texture,
    wgpu::TextureView,
    wgpu::Texture,
    wgpu::TextureView,
) {
    let desc = wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    };
    let a = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("glow_a"),
        ..desc
    });
    let b = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("glow_b"),
        ..desc
    });
    let av = a.create_view(&Default::default());
    let bv = b.create_view(&Default::default());
    (a, av, b, bv)
}
