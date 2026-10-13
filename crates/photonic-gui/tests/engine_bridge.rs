//! `EngineBridge` integration: the GUI-side mirror (tokio-app ↔ std-mutex
//! engine lock-flavor bridge in `app/engine.rs`) really feeds the engine, and
//! the 03 §5 present path registers an egui native texture — all against a
//! REAL headless `VideoEngine`. Skips (never fails) without a GPU adapter,
//! matching the photonic-video suite's convention.

use std::time::{Duration, Instant};

use photonic_core::timeline::{
    Clip, ClipSource, FrameRate, Sequence, Tick, TimelineProject, Track, TrackKind,
};
use photonic_core::{Color, CommandHistory, Document};
use photonic_gui::EngineBridge;
use photonic_video::{EngineCmd, VideoEngine};

fn solid_project() -> TimelineProject {
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("seq", FrameRate::FPS_30, 320, 180);
    let seq_id = seq.id;
    let mut v1 = Track::new(TrackKind::Video, "V1");
    v1.clips.push(Clip::new(
        ClipSource::SolidColor {
            color: Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        },
        Tick(0),
        Tick::from_seconds(5),
    ));
    seq.video_tracks.push(v1);
    project.insert_sequence(seq);
    project.active_sequence = Some(seq_id);
    project
}

#[test]
fn bridge_mirrors_document_and_presents_to_egui_texture() {
    let Some(engine) = VideoEngine::headless() else {
        eprintln!("no GPU adapter — skipping engine bridge test");
        return;
    };
    // Grab owned device/queue handles for the egui renderer before the bridge
    // takes ownership of the engine.
    let device = std::sync::Arc::clone(engine.gpu().device());
    let queue = std::sync::Arc::clone(engine.gpu().queue());

    let mut bridge = EngineBridge::new(engine);

    // The app-side document/history — deliberately NOT the mirror pair.
    let mut doc = Document::new("bridge-test", 320.0, 180.0);
    doc.timeline = Some(solid_project());
    let history = CommandHistory::new(8);

    // 1. Mirror sync: one call must be enough for the engine to observe the
    //    timeline (revision-bumped mirror → engine re-snapshot → present).
    bridge.sync_document(&doc, &history);
    bridge.session().send(EngineCmd::Seek(Tick(0)));

    let deadline = Instant::now() + Duration::from_secs(5);
    let frame = loop {
        if let Some(f) = bridge.session().latest_frame() {
            break f;
        }
        assert!(
            Instant::now() < deadline,
            "engine never produced a frame from the mirrored document"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(frame.time, Tick(0));
    let active = doc.timeline.as_ref().unwrap().active_sequence.unwrap();
    assert_eq!(frame.sequence, active);

    // 2. Present path (03 §5): present_latest must register an egui native
    //    texture sized to the frame's physical (bucket-padded) texture.
    let mut egui_renderer =
        egui_wgpu::Renderer::new(&device, wgpu::TextureFormat::Rgba8Unorm, None, 1, false);
    bridge.present_latest(&device, &queue, &mut egui_renderer);
    let tex = bridge
        .monitor_tex_info()
        .expect("present_latest registers the monitor texture");
    assert_eq!(tex.0, (frame.texture.width(), frame.texture.height()));
    // Logical 320x180 in a 64px-bucket physical texture: width matches, height
    // is padded up (192) — exactly what the monitor's UV crop compensates for.
    assert!(tex.0 .0 >= 320 && tex.0 .1 >= 180);

    // 3. Status flows: the engine observed the immutable snapshot published
    // by the bridge. A freshly-created history legitimately starts at
    // revision zero; generation is the snapshot-observation barrier.
    let status = bridge.status();
    assert!(
        status.snapshot_generation > 0,
        "engine snapshot not observed"
    );
    assert_eq!(status.doc_revision, history.revision());
}

#[test]
fn native_bridge_uses_exact_logical_targets_across_quality_and_alpha_views() {
    use photonic_core::timeline::color::{NativeManagedColorConfig, SequenceColorConfig};
    use photonic_video::{graph::ir::FrameColorEncoding, PreviewQuality};
    let engine = VideoEngine::headless().expect("GPU required for native monitor qualification");
    let device = std::sync::Arc::clone(engine.gpu().device());
    let queue = std::sync::Arc::clone(engine.gpu().queue());
    let mut bridge = EngineBridge::new(engine);
    let mut p = solid_project();
    let seq = p.active_sequence.unwrap();
    let s = p.sequences.get_mut(&seq).unwrap();
    s.formats[0].width = 17;
    s.formats[0].height = 9;
    s.color = SequenceColorConfig::NativeManaged(Box::new(NativeManagedColorConfig::sdr_draft()));
    let mut doc = Document::new("logical target", 17.0, 9.0);
    doc.timeline = Some(p);
    let history = CommandHistory::new(8);
    let before = serde_json::to_value(&doc).unwrap();
    bridge.sync_document(&doc, &history);
    let mut renderer =
        egui_wgpu::Renderer::new(&device, wgpu::TextureFormat::Rgba8Unorm, None, 1, false);
    for quality in [
        PreviewQuality::Full,
        PreviewQuality::Draft,
        PreviewQuality::Full,
    ] {
        bridge.session().send(EngineCmd::SetPreviewQuality(quality));
        bridge.session().send(EngineCmd::Seek(Tick::ZERO));
        let deadline = Instant::now() + Duration::from_secs(10);
        let frame = loop {
            if let Some(f) = bridge.session().latest_frame() {
                if f.sequence == seq
                    && f.preview_quality == quality
                    && f.output_encoding == FrameColorEncoding::SrgbDisplay
                    && f.color_errors.is_empty()
                {
                    break f;
                }
            }
            assert!(Instant::now() < deadline, "native frame unavailable");
            std::thread::sleep(Duration::from_millis(10));
        };
        if quality == PreviewQuality::Full {
            assert_eq!(frame.logical_size, (17, 9));
        }
        assert!(
            frame.texture.width() > frame.logical_size.0
                && frame.texture.height() > frame.logical_size.1,
            "fixture must have transparent pool padding"
        );
        bridge.present_latest(&device, &queue, &mut renderer);
        let first = bridge
            .monitor_tex_info()
            .expect("registered native monitor");
        assert_eq!(first.0, frame.logical_size);
        bridge.present_latest(&device, &queue, &mut renderer);
        assert_eq!(
            bridge.monitor_tex_info(),
            Some(first),
            "unchanged frame must reuse the target"
        );
        bridge.toggle_alpha_view();
        bridge.present_latest(&device, &queue, &mut renderer);
        assert_eq!(
            bridge.monitor_tex_info(),
            Some(first),
            "alpha view reuses the exact-sized allocation"
        );
        bridge.toggle_alpha_view();
        bridge.present_latest(&device, &queue, &mut renderer);
        assert_eq!(bridge.monitor_tex_info(), Some(first));
    }
    assert_eq!(serde_json::to_value(&doc).unwrap(), before);
    assert_eq!(history.revision(), 0);
}

/// Exercise the actual egui blend pipeline, not just the intermediate texture.
/// Registered sRGB textures must contain gamma-premultiplied color because
/// egui uses source factor One, destination OneMinusSrcAlpha.
#[test]
fn native_monitor_egui_composites_translucent_pixels_without_bright_fringe() {
    use photonic_render::video::{PresentChannel, VideoPresenter};
    let engine = VideoEngine::headless().expect("GPU required for monitor alpha qualification");
    let device = engine.gpu().device();
    let queue = engine.gpu().queue();
    let make = |format, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("monitor alpha fixture"),
            size: wgpu::Extent3d {
                width: 4,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let source = make(
        wgpu::TextureFormat::Rgba16Float,
        wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
    );
    // Exact binary16 premultiplied encoded RGB [.5,.25,.125], alpha0/.25/.5/1.
    let half: [[u16; 4]; 4] = [
        [0; 4],
        [0x3000, 0x2c00, 0x2800, 0x3400],
        [0x3400, 0x3000, 0x2c00, 0x3800],
        [0x3800, 0x3400, 0x3000, 0x3c00],
    ];
    let bytes: Vec<u8> = half
        .iter()
        .flatten()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    queue.write_texture(
        source.as_image_copy(),
        &bytes,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(32),
            rows_per_image: Some(1),
        },
        source.size(),
    );
    let logical = make(
        wgpu::TextureFormat::Rgba8UnormSrgb,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
    );
    let output = make(
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let presenter = VideoPresenter::new(device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let mut renderer =
        egui_wgpu::Renderer::new(device, wgpu::TextureFormat::Rgba8Unorm, None, 1, false);
    let texture = renderer.register_native_texture(
        device,
        &logical.create_view(&Default::default()),
        wgpu::FilterMode::Nearest,
    );
    let mut mesh = egui::Mesh::with_texture(texture);
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(4.0, 1.0));
    mesh.add_rect_with_uv(
        rect,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    let jobs = vec![egui::ClippedPrimitive {
        clip_rect: rect,
        primitive: egui::epaint::Primitive::Mesh(mesh),
    }];
    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [4, 1],
        pixels_per_point: 1.0,
    };
    for channel in [PresentChannel::Color, PresentChannel::Alpha] {
        for background in [0.0_f64, 0.125] {
            let mut encoder = device.create_command_encoder(&Default::default());
            presenter
                .present_encoded_engine_frame_logical_channel(
                    device,
                    &mut encoder,
                    &source,
                    &logical,
                    channel,
                )
                .unwrap();
            let uploads = renderer.update_buffers(device, queue, &mut encoder, &jobs, &screen);
            assert!(uploads.is_empty());
            {
                let view = output.create_view(&Default::default());
                let mut pass = encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("actual egui alpha blend"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color {
                                    r: background,
                                    g: background,
                                    b: background,
                                    a: 1.0,
                                }),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    })
                    .forget_lifetime();
                renderer.render(&mut pass, &jobs, &screen);
            }
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("egui readback"),
                size: 256,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                output.as_image_copy(),
                wgpu::ImageCopyBuffer {
                    buffer: &buffer,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(256),
                        rows_per_image: Some(1),
                    },
                },
                output.size(),
            );
            queue.submit([encoder.finish()]);
            let slice = buffer.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
            device.poll(wgpu::Maintain::Wait);
            rx.recv().unwrap().unwrap();
            let mapped = slice.get_mapped_range();
            for (i, alpha) in [0.0_f64, 0.25, 0.5, 1.0].into_iter().enumerate() {
                for (c, color) in [0.5, 0.25, 0.125].into_iter().enumerate() {
                    let expected = if channel == PresentChannel::Color {
                        color * alpha + background * (1.0 - alpha)
                    } else if alpha <= 0.0031308 {
                        12.92 * alpha
                    } else {
                        1.055 * alpha.powf(1.0 / 2.4) - 0.055
                    };
                    let want = (expected * 255.0).round() as u8;
                    let got = mapped[i * 4 + c];
                    assert!(
                        got.abs_diff(want) <= 2,
                        "{channel:?}, alpha{alpha}, bg{background}, channel{c}: {got} vs {want}"
                    );
                }
                assert_eq!(mapped[i * 4 + 3], 255);
            }
            drop(mapped);
            buffer.unmap();
        }
    }
}
