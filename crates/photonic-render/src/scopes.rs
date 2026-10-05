//! Video scopes (07 §5): waveform, vectorscope, histogram — compute-shader
//! accumulation into storage buffers with atomic adds, each with a CPU
//! reference (07 §6.3).
//!
//! **Scope signal domain.** Scopes read the graded working texture (03 §3.6
//! readback point: after the clip's `Grade`, before `CaptionOverlay`/fold) — a
//! premultiplied linear-Rec.709 `Rgba16Float` texture. Each pixel is
//! unpremultiplied to straight color and encoded through the **BT.709 OETF**
//! into the video-signal (gamma) domain before any scope math, which is the
//! conventional domain scopes measure (signal level, not scene light) and lets
//! the Rec.709 luma weights and the YCbCr denominators reuse `crate::color`'s
//! BT.709 constants. Luma is Rec.709 (07 §3).

use wgpu::util::DeviceExt;

use crate::color;
use crate::grade::luma709;

/// Luma/channel histogram bin count (07 §5, §6.4 fallback is 128).
pub const HIST_BINS: usize = 256;
/// Waveform vertical resolution (intensity bins per column).
pub const WAVEFORM_BINS: usize = 256;
/// Vectorscope plane resolution (Cb × Cr).
pub const VECTORSCOPE_SIZE: usize = 256;

/// Interpretation of the premultiplied texture supplied to SDR scopes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScopeSignal {
    #[default]
    LegacyLinearRec709,
    /// Display-referred sRGB code values; no second OETF is applied.
    SrgbDisplay,
}

/// Vertical signal scale for SDR waveform and parade display. Analysis always
/// retains the original full-range bins; selecting video legal only remaps the
/// plot. Values outside legal code levels remain visible at the plot edges and
/// are counted separately.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeScale {
    #[default]
    Full,
    VideoLegal,
}

impl ScopeScale {
    /// Map a full-range 8-bit signal bin to a 0..255 display row. Video-legal
    /// 16..235 code values map to 0..255; excursions clip to the edge.
    pub fn display_bin(self, signal_bin: usize) -> usize {
        let bin = signal_bin.min(255);
        match self {
            Self::Full => bin,
            Self::VideoLegal => ((bin.saturating_sub(16).min(219) * 255 + 109) / 219).min(255),
        }
    }
}

/// Six counts in R/G/B order: below zero, then above reference white.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClippingCounts {
    pub below: [u32; 3],
    pub above: [u32; 3],
}

/// Count out-of-range straight RGB values before the SDR scope display clamp.
/// Fully transparent pixels carry no visible signal and are excluded.
pub fn clipping_cpu(pixels: &[f32], width: u32, height: u32) -> ClippingCounts {
    let mut counts = ClippingCounts::default();
    for px in pixels
        .chunks_exact(4)
        .take(width as usize * height as usize)
    {
        let alpha = px[3];
        if alpha <= 0.0 || !alpha.is_finite() {
            continue;
        }
        for channel in 0..3 {
            let value = px[channel] / alpha;
            counts.below[channel] += u32::from(value < 0.0);
            counts.above[channel] += u32::from(value > 1.0);
        }
    }
    counts
}

// ── CPU signal encode ───────────────────────────────────────────────────────

/// Unpremultiply + BT.709 OETF one working pixel into signal-domain R'G'B'.
#[inline]
fn signal(px: &[f32], mode: ScopeSignal) -> [f32; 3] {
    let a = px[3].max(1e-6);
    let straight = std::array::from_fn(|c| (px[c] / a).clamp(0.0, 1.0));
    match mode {
        ScopeSignal::LegacyLinearRec709 => straight.map(color::bt709_oetf),
        ScopeSignal::SrgbDisplay => straight,
    }
}

#[inline]
fn bin256(v: f32) -> usize {
    (v * 255.0).round().clamp(0.0, 255.0) as usize
}

// ── CPU scope data ──────────────────────────────────────────────────────────

/// 256-bin luma + per-channel histogram (07 §5).
#[derive(Clone, Debug, PartialEq)]
pub struct Histogram {
    pub luma: [u32; HIST_BINS],
    pub red: [u32; HIST_BINS],
    pub green: [u32; HIST_BINS],
    pub blue: [u32; HIST_BINS],
}

impl Default for Histogram {
    fn default() -> Self {
        Histogram {
            luma: [0; HIST_BINS],
            red: [0; HIST_BINS],
            green: [0; HIST_BINS],
            blue: [0; HIST_BINS],
        }
    }
}

/// Per-column intensity histogram (07 §5). `count(x, bin) = data[x*bins + bin]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Waveform {
    pub width: usize,
    pub bins: usize,
    pub data: Vec<u32>,
}

impl Waveform {
    #[inline]
    pub fn count(&self, x: usize, bin: usize) -> u32 {
        self.data[x * self.bins + bin]
    }

    /// Counts measured samples below code 16 and above code 235. The count is
    /// derived from unmodified 256-bin signal data, independent of plot scale.
    pub fn video_legal_excursions(&self) -> (u64, u64) {
        let mut below = 0;
        let mut above = 0;
        for x in 0..self.width {
            for bin in 0..self.bins {
                let count = u64::from(self.count(x, bin));
                if bin < 16 {
                    below += count;
                } else if bin > 235 {
                    above += count;
                }
            }
        }
        (below, above)
    }
}

/// Cb×Cr scatter histogram (07 §5). `count(cb, cr) = data[cr*size + cb]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Vectorscope {
    pub size: usize,
    pub data: Vec<u32>,
}

impl Vectorscope {
    #[inline]
    pub fn count(&self, cb: usize, cr: usize) -> u32 {
        self.data[cr * self.size + cb]
    }
}

/// All three scopes for one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Scopes {
    pub histogram: Histogram,
    pub waveform: Waveform,
    pub vectorscope: Vectorscope,
}

// ── CPU references (07 §6.3) ────────────────────────────────────────────────

/// 256-bin luma + per-channel histogram over an RGBA `f32` buffer.
pub fn histogram_cpu(pixels: &[f32], _width: u32, _height: u32) -> Histogram {
    histogram_cpu_signal(pixels, _width, _height, ScopeSignal::LegacyLinearRec709)
}

pub fn histogram_cpu_signal(
    pixels: &[f32],
    _width: u32,
    _height: u32,
    mode: ScopeSignal,
) -> Histogram {
    let mut h = Histogram::default();
    for px in pixels.chunks_exact(4) {
        let s = signal(px, mode);
        h.luma[bin256(luma709(s))] += 1;
        h.red[bin256(s[0])] += 1;
        h.green[bin256(s[1])] += 1;
        h.blue[bin256(s[2])] += 1;
    }
    h
}

/// Per-column luma waveform over an RGBA `f32` buffer.
pub fn waveform_cpu(pixels: &[f32], width: u32, height: u32) -> Waveform {
    waveform_cpu_signal(pixels, width, height, ScopeSignal::LegacyLinearRec709)
}

pub fn waveform_cpu_signal(pixels: &[f32], width: u32, height: u32, mode: ScopeSignal) -> Waveform {
    let w = width.max(1) as usize;
    let mut data = vec![0u32; w * WAVEFORM_BINS];
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        let x = i % w;
        let s = signal(px, mode);
        let bin = bin256(luma709(s));
        data[x * WAVEFORM_BINS + bin] += 1;
    }
    let _ = height;
    Waveform {
        width: w,
        bins: WAVEFORM_BINS,
        data,
    }
}

/// Spatial RGB parade: one waveform per channel, preserving source columns.
pub fn parade_cpu(pixels: &[f32], width: u32, height: u32) -> [Waveform; 3] {
    parade_cpu_signal(pixels, width, height, ScopeSignal::LegacyLinearRec709)
}

pub fn parade_cpu_signal(
    pixels: &[f32],
    width: u32,
    height: u32,
    mode: ScopeSignal,
) -> [Waveform; 3] {
    let w = width.max(1) as usize;
    let mut channels = std::array::from_fn(|_| Waveform {
        width: w,
        bins: WAVEFORM_BINS,
        data: vec![0; w * WAVEFORM_BINS],
    });
    for (i, px) in pixels
        .chunks_exact(4)
        .take(width as usize * height as usize)
        .enumerate()
    {
        for (channel, value) in signal(px, mode).into_iter().enumerate() {
            channels[channel].data[(i % w) * WAVEFORM_BINS + bin256(value)] += 1;
        }
    }
    channels
}

/// Rec.709 Cb×Cr vectorscope over an RGBA `f32` buffer. The YCbCr denominators
/// reuse `crate::color::{BT709_CB_B, BT709_CR_R}` (07 §5).
pub fn vectorscope_cpu(pixels: &[f32], _width: u32, _height: u32) -> Vectorscope {
    let mut data = vec![0u32; VECTORSCOPE_SIZE * VECTORSCOPE_SIZE];
    for px in pixels.chunks_exact(4) {
        let s = signal(px, ScopeSignal::LegacyLinearRec709);
        let (cb_bin, cr_bin) = cbcr_bins(s);
        data[cr_bin * VECTORSCOPE_SIZE + cb_bin] += 1;
    }
    Vectorscope {
        size: VECTORSCOPE_SIZE,
        data,
    }
}

/// Rec.709 (Cb, Cr) bin indices for a signal-domain R'G'B'.
#[inline]
fn cbcr_bins(s: [f32; 3]) -> (usize, usize) {
    cbcr_bins_matrix(s, color::Matrix::Bt709)
}

/// K-E1: Cb/Cr bins with selectable matrix (BT.709 HD default / BT.601 SD).
#[inline]
pub fn cbcr_bins_matrix(s: [f32; 3], matrix: color::Matrix) -> (usize, usize) {
    let (y, cb_b, cr_r) = match matrix {
        color::Matrix::Bt709 => (luma709(s), color::BT709_CB_B, color::BT709_CR_R),
        color::Matrix::Bt601 => {
            // BT.601 luma weights Kr=0.299, Kg=0.587, Kb=0.114
            let y = 0.299 * s[0] + 0.587 * s[1] + 0.114 * s[2];
            (y, color::BT601_CB_B, color::BT601_CR_R)
        }
    };
    let cb = (s[2] - y) / cb_b;
    let cr = (s[0] - y) / cr_r;
    let cb_bin = ((cb + 0.5) * 255.0).round().clamp(0.0, 255.0) as usize;
    let cr_bin = ((cr + 0.5) * 255.0).round().clamp(0.0, 255.0) as usize;
    let _ = cr_r; // used above
    (cb_bin, cr_bin)
}

/// CPU vectorscope with selectable matrix (K-E1 YUV / YPbPr labelling switch).
pub fn vectorscope_cpu_matrix(
    pixels: &[f32],
    _width: u32,
    _height: u32,
    matrix: color::Matrix,
) -> Vectorscope {
    vectorscope_cpu_matrix_signal(
        pixels,
        _width,
        _height,
        matrix,
        ScopeSignal::LegacyLinearRec709,
    )
}

pub fn vectorscope_cpu_matrix_signal(
    pixels: &[f32],
    _width: u32,
    _height: u32,
    matrix: color::Matrix,
    mode: ScopeSignal,
) -> Vectorscope {
    let mut data = vec![0u32; VECTORSCOPE_SIZE * VECTORSCOPE_SIZE];
    for px in pixels.chunks_exact(4) {
        let s = signal(px, mode);
        let (cb_bin, cr_bin) = cbcr_bins_matrix(s, matrix);
        data[cr_bin * VECTORSCOPE_SIZE + cb_bin] += 1;
    }
    Vectorscope {
        size: VECTORSCOPE_SIZE,
        data,
    }
}

/// All three CPU scopes for one frame.
pub fn scopes_from_pixels_cpu(pixels: &[f32], width: u32, height: u32) -> Scopes {
    scopes_from_pixels_cpu_matrix(pixels, width, height, color::Matrix::Bt709)
}

/// CPU scope bundle with selectable vectorscope matrix. Histogram and waveform
/// continue to measure the Legacy SDR BT.709 signal; only Cb/Cr bins change.
pub fn scopes_from_pixels_cpu_matrix(
    pixels: &[f32],
    width: u32,
    height: u32,
    matrix: color::Matrix,
) -> Scopes {
    scopes_from_pixels_cpu_matrix_signal(
        pixels,
        width,
        height,
        matrix,
        ScopeSignal::LegacyLinearRec709,
    )
}

pub fn scopes_from_pixels_cpu_matrix_signal(
    pixels: &[f32],
    width: u32,
    height: u32,
    matrix: color::Matrix,
    mode: ScopeSignal,
) -> Scopes {
    Scopes {
        histogram: histogram_cpu_signal(pixels, width, height, mode),
        waveform: waveform_cpu_signal(pixels, width, height, mode),
        vectorscope: vectorscope_cpu_matrix_signal(pixels, width, height, matrix, mode),
    }
}

// ── compute shaders (atomic accumulation, 07 §5) ────────────────────────────

/// Shared compute prelude: unpremultiply + BT.709 OETF signal encode, Rec.709
/// luma, and the 256-bin quantizer. Constants match `crate::color` (asserted by
/// `contains_token`).
const COMPUTE_PRELUDE: &str = r#"
@group(0) @binding(0) var t_src: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> bins: array<atomic<u32>>;
// K-E2: the LOGICAL extent to measure. A working texture out of the node pool is
// bucket-padded (dimensions rounded up to a 64px multiple, 03 §3.4), and that
// padding is transparent black — measuring it puts a phantom spike in bin 0 of
// every scope. `logical.xy` is the real image size; `.z` selects vectorscope
// matrix and `.w` selects source signal interpretation.
@group(0) @binding(2) var<uniform> logical: vec4<u32>;

// True when this invocation is inside both the logical extent and the texture.
fn in_scope(gid: vec3<u32>) -> bool {
    let dims = textureDimensions(t_src);
    return gid.x < min(logical.x, dims.x) && gid.y < min(logical.y, dims.y);
}

fn oetf709(e: f32) -> f32 {
    let x = clamp(e, 0.0, 1.0);
    if (x < 0.018) { return 4.5 * x; }
    return 1.099 * pow(x, 0.45) - 0.099;
}
fn signal(px: vec4<f32>) -> vec3<f32> {
    let a = max(px.a, 1e-6);
    let s = clamp(px.rgb / a, vec3<f32>(0.0), vec3<f32>(1.0));
    if (logical.w == 1u) { return s; }
    return vec3<f32>(oetf709(s.r), oetf709(s.g), oetf709(s.b));
}
fn luma709(c: vec3<f32>) -> f32 { return 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b; }
fn bin256(v: f32) -> u32 { return u32(clamp(round(v * 255.0), 0.0, 255.0)); }
"#;

/// Histogram compute kernel: luma into `[0,256)`, R/G/B into
/// `[256,512)/[512,768)/[768,1024)` (07 §5).
pub const HISTOGRAM_SHADER: &str = r#"
__PRELUDE__
@compute @workgroup_size(8, 8, 1)
fn cs_hist(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (!in_scope(gid)) { return; }
    let px = textureLoad(t_src, vec2<i32>(i32(gid.x), i32(gid.y)), 0);
    let s = signal(px);
    atomicAdd(&bins[bin256(luma709(s))], 1u);
    atomicAdd(&bins[256u + bin256(s.r)], 1u);
    atomicAdd(&bins[512u + bin256(s.g)], 1u);
    atomicAdd(&bins[768u + bin256(s.b)], 1u);
    if (px.a > 0.0) {
        let straight = px.rgb / px.a;
        for (var c = 0u; c < 3u; c = c + 1u) {
            if (straight[c] < 0.0) { atomicAdd(&bins[1024u + c], 1u); }
            if (straight[c] > 1.0) { atomicAdd(&bins[1027u + c], 1u); }
        }
    }
}
"#;

/// Waveform compute kernel: per-column luma histogram,
/// `bins[x*256 + luma_bin]` (07 §5).
pub const WAVEFORM_SHADER: &str = r#"
__PRELUDE__
@compute @workgroup_size(8, 8, 1)
fn cs_wave(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (!in_scope(gid)) { return; }
    let px = textureLoad(t_src, vec2<i32>(i32(gid.x), i32(gid.y)), 0);
    let s = signal(px);
    atomicAdd(&bins[gid.x * 256u + bin256(luma709(s))], 1u);
}
"#;

/// Channel-major, per-column RGB waveform accumulation.
pub const PARADE_SHADER: &str = r#"
__PRELUDE__
@compute @workgroup_size(8, 8, 1)
fn cs_parade(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (!in_scope(gid)) { return; }
    let s = signal(textureLoad(t_src, vec2<i32>(gid.xy), 0));
    let width = max(1u, min(logical.x, textureDimensions(t_src).x));
    for (var c = 0u; c < 3u; c = c + 1u) {
        atomicAdd(&bins[(c * width + gid.x) * 256u + bin256(s[c])], 1u);
    }
}
"#;

/// Vectorscope compute kernel: Cb×Cr scatter with selectable matrix,
/// `bins[cr*256 + cb]` (07 §5 / K-E1). `logical.z == 1` selects BT.601;
/// otherwise BT.709. Denominators match `crate::color::{BT709_*, BT601_*}`.
pub const VECTORSCOPE_SHADER: &str = r#"
__PRELUDE__
fn cbcr(s: vec3<f32>, use_601: bool) -> vec2<u32> {
    var y: f32;
    var cb_b: f32;
    var cr_r: f32;
    if (use_601) {
        y = 0.299 * s.r + 0.587 * s.g + 0.114 * s.b;
        cb_b = 1.772;
        cr_r = 1.402;
    } else {
        y = luma709(s);
        cb_b = 1.8556;
        cr_r = 1.5748;
    }
    let cb = (s.b - y) / cb_b;
    let cr = (s.r - y) / cr_r;
    let cbn = u32(clamp(round((cb + 0.5) * 255.0), 0.0, 255.0));
    let crn = u32(clamp(round((cr + 0.5) * 255.0), 0.0, 255.0));
    return vec2<u32>(cbn, crn);
}
@compute @workgroup_size(8, 8, 1)
fn cs_vector(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (!in_scope(gid)) { return; }
    let px = textureLoad(t_src, vec2<i32>(i32(gid.x), i32(gid.y)), 0);
    let s = signal(px);
    let b = cbcr(s, logical.z != 0u);
    atomicAdd(&bins[b.y * 256u + b.x], 1u);
}
"#;

fn expand(src: &str) -> String {
    src.replace("__PRELUDE__", COMPUTE_PRELUDE)
}

// Expanded sources for the pipeline shader-validation test (pipeline.rs).
#[cfg(test)]
pub(crate) fn expanded_histogram_shader() -> String {
    expand(HISTOGRAM_SHADER)
}
#[cfg(test)]
pub(crate) fn expanded_waveform_shader() -> String {
    expand(WAVEFORM_SHADER)
}
#[cfg(test)]
pub(crate) fn expanded_vectorscope_shader() -> String {
    expand(VECTORSCOPE_SHADER)
}

// ── GPU runner (readback-point API) ─────────────────────────────────────────

/// Run one scope compute shader over `tex` and read back `out_len` `u32` bins.
/// `tex` is any `Rgba16Float` texture — the 03 §3.6 readback point (the graded,
/// pre-composite clip texture), or any working texture a caller hands in.
fn run_scope(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    shader_src: &str,
    entry: &str,
    out_len: usize,
    logical: (u32, u32),
) -> Vec<u32> {
    run_scope_ex(device, queue, tex, shader_src, entry, out_len, logical, 0)
}

/// Like [`run_scope`], with an extra uniform `z` packed into the logical
/// buffer (vectorscope matrix: 0 = BT.709, 1 = BT.601).
fn run_scope_ex(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    shader_src: &str,
    entry: &str,
    out_len: usize,
    logical: (u32, u32),
    extra_z: u32,
) -> Vec<u32> {
    let request = begin_scope_readback(
        device, queue, tex, shader_src, entry, out_len, logical, extra_z,
    );
    device.poll(wgpu::Maintain::Wait);
    request.finish().expect("scope readback failed")
}

/// Submitted GPU scope measurement. Polling never waits for GPU completion.
pub struct ScopeReadback {
    staging: wgpu::Buffer,
    ready: std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
}

impl ScopeReadback {
    pub fn poll(&self, device: &wgpu::Device) -> Option<Result<Vec<u32>, String>> {
        device.poll(wgpu::Maintain::Poll);
        match self.ready.try_recv() {
            Ok(Ok(())) => Some(Ok(self.read())),
            Ok(Err(error)) => Some(Err(error.to_string())),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(error) => Some(Err(error.to_string())),
        }
    }

    fn read(&self) -> Vec<u32> {
        let raw = self.staging.slice(..).get_mapped_range();
        let data = bytemuck::cast_slice(&raw).to_vec();
        drop(raw);
        self.staging.unmap();
        data
    }

    fn finish(self) -> Result<Vec<u32>, String> {
        self.ready
            .recv()
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        Ok(self.read())
    }
}

/// Submit a scope kernel and start asynchronous readback. `extra_z` selects
/// the vectorscope matrix (0 = BT.709, 1 = BT.601).
#[allow(clippy::too_many_arguments)]
pub fn begin_scope_readback(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    shader_src: &str,
    entry: &str,
    out_len: usize,
    logical: (u32, u32),
    extra_z: u32,
) -> ScopeReadback {
    begin_scope_readback_signal(
        device,
        queue,
        tex,
        shader_src,
        entry,
        out_len,
        logical,
        extra_z,
        ScopeSignal::LegacyLinearRec709,
    )
}

/// Submit a scope readback with an explicit source signal interpretation.
#[allow(clippy::too_many_arguments)]
pub fn begin_scope_readback_signal(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    shader_src: &str,
    entry: &str,
    out_len: usize,
    logical: (u32, u32),
    extra_z: u32,
    signal: ScopeSignal,
) -> ScopeReadback {
    let bins = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("scope_bins"),
        contents: bytemuck::cast_slice(&vec![0u32; out_len]),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
    });
    let logical_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("scope_logical"),
        contents: bytemuck::cast_slice(&[
            logical.0,
            logical.1,
            extra_z,
            u32::from(signal == ScopeSignal::SrgbDisplay),
        ]),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("scope_bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("scope_shader"),
        source: wgpu::ShaderSource::Wgsl(expand(shader_src).into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("scope_layout"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("scope_pipeline"),
        layout: Some(&layout),
        module: &shader,
        entry_point: entry,
        compilation_options: Default::default(),
        cache: None,
    });
    let view = tex.create_view(&Default::default());
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("scope_bg"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: bins.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: logical_buf.as_entire_binding(),
            },
        ],
    });
    // Dispatch over the logical extent only — the shader still bounds-checks, but
    // there is no reason to launch invocations over pool padding.
    let (w, h) = (
        logical.0.min(tex.width()).max(1),
        logical.1.min(tex.height()).max(1),
    );
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("scope_rb"),
        size: (out_len * 4) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    {
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("scope_pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
    }
    enc.copy_buffer_to_buffer(&bins, 0, &staging, 0, (out_len * 4) as u64);
    queue.submit([enc.finish()]);

    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    ScopeReadback { staging, ready: rx }
}

/// GPU histogram at the readback point (07 §5), measuring the whole texture.
///
/// Prefer [`histogram_gpu_logical`] for any texture that came out of the video
/// node pool: those are bucket-padded and this form counts the padding.
pub fn histogram_gpu(device: &wgpu::Device, queue: &wgpu::Queue, tex: &wgpu::Texture) -> Histogram {
    histogram_gpu_logical(device, queue, tex, tex.width(), tex.height())
}

/// [`histogram_gpu`] restricted to the image's logical `w × h` (K-E2).
pub fn histogram_gpu_logical(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    w: u32,
    h: u32,
) -> Histogram {
    let raw = run_scope(
        device,
        queue,
        tex,
        HISTOGRAM_SHADER,
        "cs_hist",
        HIST_BINS * 4 + 6,
        (w, h),
    );
    let mut h = Histogram::default();
    h.luma.copy_from_slice(&raw[0..256]);
    h.red.copy_from_slice(&raw[256..512]);
    h.green.copy_from_slice(&raw[512..768]);
    h.blue.copy_from_slice(&raw[768..1024]);
    h
}

/// GPU waveform at the readback point (07 §5), measuring the whole texture.
pub fn waveform_gpu(device: &wgpu::Device, queue: &wgpu::Queue, tex: &wgpu::Texture) -> Waveform {
    waveform_gpu_logical(device, queue, tex, tex.width(), tex.height())
}

/// [`waveform_gpu`] restricted to the image's logical `w × h` (K-E2). The
/// returned [`Waveform::width`] is the logical width, so column *x* on the plot
/// is column *x* of the image and not of the pool bucket.
pub fn waveform_gpu_logical(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    w: u32,
    h: u32,
) -> Waveform {
    let cols = w.min(tex.width()).max(1) as usize;
    let data = run_scope(
        device,
        queue,
        tex,
        WAVEFORM_SHADER,
        "cs_wave",
        cols * WAVEFORM_BINS,
        (w, h),
    );
    Waveform {
        width: cols,
        bins: WAVEFORM_BINS,
        data,
    }
}

/// GPU RGB parade over the logical image, excluding pooled texture padding.
pub fn parade_gpu_logical(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    w: u32,
    h: u32,
) -> [Waveform; 3] {
    let cols = w.min(tex.width()).max(1) as usize;
    let stride = cols * WAVEFORM_BINS;
    let data = run_scope(
        device,
        queue,
        tex,
        PARADE_SHADER,
        "cs_parade",
        stride * 3,
        (w, h),
    );
    std::array::from_fn(|channel| Waveform {
        width: cols,
        bins: WAVEFORM_BINS,
        data: data[channel * stride..(channel + 1) * stride].to_vec(),
    })
}

/// GPU vectorscope at the readback point (07 §5), measuring the whole texture.
pub fn vectorscope_gpu(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
) -> Vectorscope {
    vectorscope_gpu_logical(device, queue, tex, tex.width(), tex.height())
}

/// [`vectorscope_gpu`] restricted to the image's logical `w × h` (K-E2).
/// Defaults to BT.709 (HD).
pub fn vectorscope_gpu_logical(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    w: u32,
    h: u32,
) -> Vectorscope {
    vectorscope_gpu_logical_matrix(device, queue, tex, w, h, color::Matrix::Bt709)
}

/// Vectorscope over logical `w × h` with a selectable YCbCr matrix (K-E1).
pub fn vectorscope_gpu_logical_matrix(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    w: u32,
    h: u32,
    matrix: color::Matrix,
) -> Vectorscope {
    let use_601 = matches!(matrix, color::Matrix::Bt601) as u32;
    let data = run_scope_ex(
        device,
        queue,
        tex,
        VECTORSCOPE_SHADER,
        "cs_vector",
        VECTORSCOPE_SIZE * VECTORSCOPE_SIZE,
        (w, h),
        use_601,
    );
    Vectorscope {
        size: VECTORSCOPE_SIZE,
        data,
    }
}

/// All three GPU scopes for the readback-point texture (07 §5).
pub fn scopes_from_texture_gpu(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
) -> Scopes {
    scopes_from_texture_gpu_logical(device, queue, tex, tex.width(), tex.height())
}

/// All three GPU scopes over the logical `w × h` of a (possibly bucket-padded)
/// working texture — the K-E2 scope-tap read path.
pub fn scopes_from_texture_gpu_logical(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    w: u32,
    h: u32,
) -> Scopes {
    Scopes {
        histogram: histogram_gpu_logical(device, queue, tex, w, h),
        waveform: waveform_gpu_logical(device, queue, tex, w, h),
        vectorscope: vectorscope_gpu_logical(device, queue, tex, w, h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::WORKING_FORMAT;

    #[test]
    fn video_legal_scale_maps_endpoints_and_counts_excursions() {
        assert_eq!(ScopeScale::Full.display_bin(16), 16);
        assert_eq!(ScopeScale::VideoLegal.display_bin(0), 0);
        assert_eq!(ScopeScale::VideoLegal.display_bin(15), 0);
        assert_eq!(ScopeScale::VideoLegal.display_bin(16), 0);
        assert_eq!(ScopeScale::VideoLegal.display_bin(235), 255);
        assert_eq!(ScopeScale::VideoLegal.display_bin(236), 255);
        assert_eq!(ScopeScale::VideoLegal.display_bin(255), 255);
        assert!(ScopeScale::VideoLegal.display_bin(125) < ScopeScale::VideoLegal.display_bin(126));
        let mut waveform = Waveform {
            width: 2,
            bins: 256,
            data: vec![0; 2 * 256],
        };
        for (x, bin) in [(0, 0), (0, 15), (0, 16), (1, 235), (1, 236), (1, 255)] {
            waveform.data[x * 256 + bin] = 1;
        }
        assert_eq!(waveform.video_legal_excursions(), (2, 2));
        assert_eq!(waveform.data.iter().sum::<u32>(), 6);
    }

    #[test]
    fn bundled_scope_matrix_changes_only_vectorscope_bins() {
        let pixels = [0.8, 0.2, 0.1, 1.0, 0.1, 0.8, 0.2, 1.0];
        let default = scopes_from_pixels_cpu(&pixels, 2, 1);
        let hd = scopes_from_pixels_cpu_matrix(&pixels, 2, 1, color::Matrix::Bt709);
        let sd = scopes_from_pixels_cpu_matrix(&pixels, 2, 1, color::Matrix::Bt601);
        assert_eq!(default, hd);
        assert_eq!(hd.histogram, sd.histogram);
        assert_eq!(hd.waveform, sd.waveform);
        assert_ne!(hd.vectorscope, sd.vectorscope);
    }

    #[test]
    fn srgb_scope_signal_preserves_display_code_values() {
        let pixels = [0.25, 0.25, 0.25, 1.0];
        let display = histogram_cpu_signal(&pixels, 1, 1, ScopeSignal::SrgbDisplay);
        let legacy = histogram_cpu(&pixels, 1, 1);
        assert_eq!(display.red[64], 1);
        assert_eq!(display.luma[64], 1);
        assert_eq!(legacy.red[125], 1);
        assert_eq!(
            parade_cpu_signal(&pixels, 1, 1, ScopeSignal::SrgbDisplay)[0].count(0, 64),
            1
        );
        let Some((device, queue)) = try_device() else {
            return;
        };
        let tex = upload(&device, &queue, &pixels, 1, 1);
        let request = begin_scope_readback_signal(
            &device,
            &queue,
            &tex,
            HISTOGRAM_SHADER,
            "cs_hist",
            1030,
            (1, 1),
            0,
            ScopeSignal::SrgbDisplay,
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let bins = loop {
            if let Some(result) = request.poll(&device) {
                break result.unwrap();
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        assert_eq!(bins[64], 1);
        assert_eq!(bins[256 + 64], 1);
    }

    // ── CPU references (07 §6.3) ────────────────────────────────────────────

    /// A gray ramp whose *signal-domain* luma is exactly `i/(N-1)`: pixel i is
    /// the linear value that BT.709-encodes back to `i/(N-1)`.
    fn signal_ramp(n: usize) -> Vec<f32> {
        let mut v = Vec::with_capacity(n * 4);
        for i in 0..n {
            let sig = i as f32 / (n - 1) as f32;
            let lin = color::bt709_eotf(sig);
            v.extend_from_slice(&[lin, lin, lin, 1.0]);
        }
        v
    }

    #[test]
    fn asynchronous_scope_readback_matches_reference() {
        let Some((device, queue)) = try_device() else {
            return;
        };
        let px = [1., 0., 0., 1., 0., 1., 0., 1.];
        let tex = upload(&device, &queue, &px, 2, 1);
        let request = begin_scope_readback(
            &device,
            &queue,
            &tex,
            HISTOGRAM_SHADER,
            "cs_hist",
            1030,
            (2, 1),
            0,
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let bins = loop {
            if let Some(result) = request.poll(&device) {
                break result.unwrap();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "async scope never completed"
            );
            std::thread::yield_now();
        };
        let reference = histogram_cpu(&px, 2, 1);
        assert_eq!(&bins[256..512], &reference.red);
        assert_eq!(&bins[512..768], &reference.green);
    }

    #[test]
    fn parade_preserves_spatial_position_and_channel_separation() {
        let px = [1., 0., 0., 1., 0., 1., 0., 1., 0., 0., 1., 1.];
        let channels = parade_cpu(&px, 3, 1);
        for (channel, waveform) in channels.iter().enumerate() {
            for x in 0..3 {
                assert_eq!(waveform.count(x, if x == channel { 255 } else { 0 }), 1);
            }
            assert_eq!(waveform.data.iter().sum::<u32>(), 3);
        }
        if let Some((device, queue)) = try_device() {
            let tex = upload(&device, &queue, &px, 3, 1);
            assert_eq!(parade_gpu_logical(&device, &queue, &tex, 3, 1), channels);
        }
    }

    #[test]
    fn waveform_of_gradient_is_diagonal() {
        // 07 §6.3: a monotone ramp produces a diagonal waveform — column x lights
        // exactly bin round(x/(N-1) * 255).
        let n = 64;
        let px = signal_ramp(n);
        let wf = waveform_cpu(&px, n as u32, 1);
        for x in 0..n {
            let expect = ((x as f32 / (n - 1) as f32) * 255.0).round() as usize;
            assert_eq!(wf.count(x, expect), 1, "col {x} bin {expect}");
            let total: u32 = (0..WAVEFORM_BINS).map(|b| wf.count(x, b)).sum();
            assert_eq!(total, 1, "one sample per column");
        }
    }

    #[test]
    fn histogram_counts_are_exact() {
        // Four black + four white opaque pixels → 4 in bin 0, 4 in bin 255.
        let mut px = Vec::new();
        for _ in 0..4 {
            px.extend_from_slice(&[0.0, 0.0, 0.0, 1.0]);
        }
        for _ in 0..4 {
            px.extend_from_slice(&[1.0, 1.0, 1.0, 1.0]);
        }
        let h = histogram_cpu(&px, 8, 1);
        assert_eq!(h.luma[0], 4);
        assert_eq!(h.luma[255], 4);
        let total: u32 = h.luma.iter().sum();
        assert_eq!(total, 8);
    }

    #[test]
    fn clipping_counts_straight_visible_signal_before_histogram_clamp() {
        let pixels = [
            -0.25, 0.75, 0.0, 0.5, // straight R=-0.5, G=1.5
            0.0, 0.0, 2.0, 1.0, // B=2
            -1.0, 3.0, 3.0, 0.0, // invisible transparent payload
        ];
        assert_eq!(
            clipping_cpu(&pixels, 3, 1),
            ClippingCounts {
                below: [1, 0, 0],
                above: [0, 1, 1],
            }
        );
    }

    #[test]
    fn gpu_histogram_reports_unclamped_channel_counts() {
        let Some((device, queue)) = try_device() else {
            return;
        };
        let pixels = [-0.25, 0.75, 0.0, 0.5, 0.0, 0.0, 2.0, 1.0];
        let tex = upload(&device, &queue, &pixels, 2, 1);
        let bins = run_scope(
            &device,
            &queue,
            &tex,
            HISTOGRAM_SHADER,
            "cs_hist",
            1030,
            (2, 1),
        );
        let expected = clipping_cpu(&pixels, 2, 1);
        assert_eq!(&bins[1024..1027], &expected.below);
        assert_eq!(&bins[1027..1030], &expected.above);
    }

    #[test]
    fn vectorscope_primaries_cluster_at_known_points() {
        // 07 §6.3: pure red signal → Cr max (255), Cb ≈ 98; the neutral gray at
        // the centre (128,128).
        let red_lin = [
            color::bt709_eotf(1.0),
            color::bt709_eotf(0.0),
            color::bt709_eotf(0.0),
            1.0,
        ];
        let (cb, cr) = cbcr_bins([1.0, 0.0, 0.0]);
        assert_eq!(cr, 255, "red at Cr max");
        assert_eq!(cb, 98, "red Cb bin");
        let vs = vectorscope_cpu(&red_lin, 1, 1);
        assert_eq!(vs.count(cb, cr), 1);

        // Neutral gray sits at plane centre.
        let gray = [color::bt709_eotf(0.5); 3];
        let (gcb, gcr) = cbcr_bins(gray);
        assert_eq!((gcb, gcr), (128, 128), "gray centred");
    }

    // ── GPU parity (07 §6.3), adapter-skip ──────────────────────────────────

    fn try_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))?;
        pollster::block_on(adapter.request_device(&Default::default(), None)).ok()
    }

    fn f32_to_f16_bits(v: f32) -> u16 {
        let b = v.to_bits();
        let sign = ((b >> 16) & 0x8000) as u16;
        let e = ((b >> 23) & 0xff) as i32 - 112;
        let m = b & 0x7fffff;
        if e <= 0 {
            sign
        } else if e >= 0x1f {
            sign | 0x7c00
        } else {
            sign | ((e as u16) << 10) | ((m >> 13) as u16)
        }
    }

    fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        px: &[f32],
        w: u32,
        h: u32,
    ) -> wgpu::Texture {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scope_in"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORKING_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut data = Vec::new();
        for c in px {
            data.extend_from_slice(&f32_to_f16_bits(*c).to_le_bytes());
        }
        queue.write_texture(
            tex.as_image_copy(),
            &data,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(w * 8),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        tex
    }

    /// L1 difference between two bin arrays — each boundary-crossing pixel
    /// contributes 2 (one bin −1, its neighbour +1).
    fn l1(a: &[u32], b: &[u32]) -> u64 {
        a.iter().zip(b).map(|(x, y)| x.abs_diff(*y) as u64).sum()
    }

    #[test]
    fn gpu_histogram_matches_cpu() {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping scope histogram parity");
            return;
        };
        let (w, h) = (16u32, 16u32);
        let mut px = Vec::new();
        for i in 0..(w * h) {
            let g = (i % 32) as f32 / 31.0;
            px.extend_from_slice(&[
                color::bt709_eotf(g),
                color::bt709_eotf(g),
                color::bt709_eotf(g),
                1.0,
            ]);
        }
        let tex = upload(&device, &queue, &px, w, h);
        let gpu = histogram_gpu(&device, &queue, &tex);
        let cpu = histogram_cpu(&px, w, h);
        assert_eq!(gpu.luma.iter().sum::<u32>(), w * h);
        assert!(l1(&gpu.luma, &cpu.luma) <= 4, "luma drift");
        assert!(l1(&gpu.red, &cpu.red) <= 4, "red drift");
    }

    /// K-E2: a working texture out of the node pool is bucket-padded, and the
    /// padding is transparent black. The logical-extent form must measure only
    /// the image; the whole-texture form (which the tap must never use) is
    /// asserted here to visibly disagree, so this test cannot pass if the
    /// bounds uniform is dropped and both forms collapse to the same thing.
    #[test]
    fn logical_extent_excludes_the_pool_padding() {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping scope logical-extent test");
            return;
        };
        // A 64×64 "bucket" whose top-left 8×8 is the real image (mid grey); the
        // rest stays the zeroed transparent-black padding.
        const BUCKET: u32 = 64;
        const LOGICAL: u32 = 8;
        let grey = color::bt709_eotf(0.5);
        let mut px = vec![0.0f32; (BUCKET * BUCKET * 4) as usize];
        for y in 0..LOGICAL {
            for x in 0..LOGICAL {
                let i = ((y * BUCKET + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&[grey, grey, grey, 1.0]);
            }
        }
        let tex = upload(&device, &queue, &px, BUCKET, BUCKET);

        let cropped = histogram_gpu_logical(&device, &queue, &tex, LOGICAL, LOGICAL);
        assert_eq!(
            cropped.luma.iter().sum::<u32>(),
            LOGICAL * LOGICAL,
            "only the logical pixels are sampled"
        );
        assert_eq!(
            cropped.luma[0], 0,
            "no phantom black spike from the padding"
        );
        // The populated bin is derived, not asserted as a literal: f16 storage
        // may land the mid-grey one bin either side of `bin256(0.5)`.
        let populated: Vec<usize> = cropped
            .luma
            .iter()
            .enumerate()
            .filter(|(_, c)| **c > 0)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(populated.len(), 1, "a uniform grey fills exactly one bin");
        let mid = populated[0];
        assert!(
            mid.abs_diff(bin256(0.5)) <= 1,
            "…and that bin is the mid-grey one, got {mid}"
        );
        assert_eq!(cropped.luma[mid], LOGICAL * LOGICAL, "all grey in one bin");

        // Sensitivity: the whole-texture form really does count the padding.
        let whole = histogram_gpu(&device, &queue, &tex);
        assert_eq!(whole.luma.iter().sum::<u32>(), BUCKET * BUCKET);
        assert_eq!(
            whole.luma[0],
            BUCKET * BUCKET - LOGICAL * LOGICAL,
            "the un-cropped read is dominated by padding — this is the defect the \
             logical form exists to avoid"
        );

        // The waveform's column axis must be the image's, not the bucket's.
        let wf = waveform_gpu_logical(&device, &queue, &tex, LOGICAL, LOGICAL);
        assert_eq!(wf.width, LOGICAL as usize);
        assert_eq!(wf.count(0, mid), LOGICAL);
    }

    #[test]
    fn gpu_waveform_matches_cpu() {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping scope waveform parity");
            return;
        };
        let n = 32u32;
        let px = signal_ramp(n as usize);
        let tex = upload(&device, &queue, &px, n, 1);
        let gpu = waveform_gpu(&device, &queue, &tex);
        let cpu = waveform_cpu(&px, n, 1);
        assert!(l1(&gpu.data, &cpu.data) <= 4, "waveform drift");
    }

    #[test]
    fn gpu_vectorscope_matches_cpu() {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping scope vectorscope parity");
            return;
        };
        // A block of pure red + a block of gray.
        let mut px = Vec::new();
        for _ in 0..64 {
            px.extend_from_slice(&[color::bt709_eotf(1.0), 0.0, 0.0, 1.0]);
        }
        for _ in 0..64 {
            let g = color::bt709_eotf(0.5);
            px.extend_from_slice(&[g, g, g, 1.0]);
        }
        let tex = upload(&device, &queue, &px, 128, 1);
        let gpu = vectorscope_gpu(&device, &queue, &tex);
        let cpu = vectorscope_cpu(&px, 128, 1);
        assert_eq!(gpu.data.iter().sum::<u32>(), 128);
        assert!(l1(&gpu.data, &cpu.data) <= 8, "vectorscope drift");
    }
}
