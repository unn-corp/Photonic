//! Real session playback measurement. FPS counts unique published program
//! ticks observed by the headless consumer; decode-ring hits are diagnostics.
//! Hardware budgets are opt-in and require a named hardware profile. See
//! docs/development/playback-benchmark.md for invocation and field meanings.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use photonic_core::timeline::{
    AssetKind, CaptionCue, CaptionTrack, CaptionWord, Clip, ClipAudio, ClipEffect, ClipSource,
    EffectKind, FrameRate, MediaAsset, PropValue, ProxyRef, Sequence, Tick, TimelineProject, Track,
    TrackKind,
};
use photonic_core::{CommandHistory, Document};
use photonic_video::graph::compile::ScopeTapPoint;
use photonic_video::media::ffmpeg_locate::locate_for_test;
use photonic_video::media::proxy::generate_proxy;
use photonic_video::{
    EngineCmd, EngineSession, GpuContext, PreviewQuality, ProxyMode, VideoEngine,
};
use serde::Serialize;

const POLL: Duration = Duration::from_millis(4);

#[derive(Default)]
struct FrameObservation {
    ticks: BTreeSet<i64>,
    unique_new: u64,
    repeated_polls: u64,
    last_change: Duration,
    intervals_ms: Vec<f64>,
}
impl FrameObservation {
    fn with_initial(tick: Tick) -> Self {
        Self {
            ticks: BTreeSet::from([tick.0]),
            ..Self::default()
        }
    }
    fn observe(&mut self, tick: Tick, elapsed: Duration) {
        if self.ticks.insert(tick.0) {
            self.unique_new += 1;
            self.intervals_ms
                .push(elapsed.saturating_sub(self.last_change).as_secs_f64() * 1000.0);
            self.last_change = elapsed;
        } else {
            self.repeated_polls += 1;
        }
    }
    fn longest_hold_ms(&self, elapsed: Duration) -> f64 {
        self.intervals_ms.iter().copied().fold(
            elapsed.saturating_sub(self.last_change).as_secs_f64() * 1000.0,
            f64::max,
        )
    }
}

#[derive(Serialize)]
struct BenchResult {
    case: String,
    source: String,
    workload: String,
    source_dimensions: [u32; 2],
    source_fps: f64,
    quality: String,
    proxy_policy: String,
    selected_input: String,
    window_seconds: f64,
    observed_unique_frames: u64,
    observed_unique_fps: f64,
    engine_publications: u64,
    evaluations: u64,
    incomplete_evaluations: u64,
    cover_interval_drops: u64,
    repeated_polls: u64,
    longest_hold_ms: f64,
    frame_interval_p95_ms: f64,
    cold_inspection_ms: f64,
    exact_seek_ms: Vec<f64>,
    seek_p95_ms: f64,
    audio_device_active: bool,
    audio_underrun_frames: u64,
    decode_ring_hits: u64,
    decode_ring_misses: u64,
    worker_seeks: u64,
    worker_pumped: u64,
    managed_gpu_cache_bytes: u64,
    decoded_ring_bytes: u64,
}

#[derive(Default)]
struct Budgets {
    min_fps: Option<f64>,
    max_hold_ms: Option<f64>,
    max_seek_ms: Option<f64>,
}
impl Budgets {
    fn from_env() -> Self {
        let budgets = Self {
            min_fps: optional_number("PHOTONIC_BENCH_MIN_OBSERVED_FPS"),
            max_hold_ms: optional_number("PHOTONIC_BENCH_MAX_HOLD_MS"),
            max_seek_ms: optional_number("PHOTONIC_BENCH_MAX_SEEK_MS"),
        };
        if budgets.enabled() {
            assert!(
                std::env::var("PHOTONIC_BENCH_HARDWARE").is_ok_and(|name| !name.trim().is_empty()),
                "hardware budgets require PHOTONIC_BENCH_HARDWARE"
            );
            assert!(
                std::env::var("PHOTONIC_BENCH_CASE").is_ok_and(|name| !name.trim().is_empty()),
                "hardware budgets require an explicit PHOTONIC_BENCH_CASE"
            );
        }
        budgets
    }
    fn enabled(&self) -> bool {
        self.min_fps.is_some() || self.max_hold_ms.is_some() || self.max_seek_ms.is_some()
    }
    fn assert_result(&self, result: &BenchResult) {
        if self.enabled() && result.workload == "mixed" {
            assert!(
                result.audio_device_active,
                "mixed hardware budget requires a working audio device"
            );
        }
        if let Some(min) = self.min_fps {
            assert!(
                result.observed_unique_fps >= min,
                "{} observed {:.2} FPS below budget {min}",
                result.case,
                result.observed_unique_fps
            );
        }
        if let Some(max) = self.max_hold_ms {
            assert!(
                result.longest_hold_ms <= max,
                "{} held {:.2}ms above budget {max}ms",
                result.case,
                result.longest_hold_ms
            );
        }
        if let Some(max) = self.max_seek_ms {
            assert!(
                result.exact_seek_ms.iter().all(|ms| *ms <= max),
                "{} exact seek {:?} exceeds {max}ms",
                result.case,
                result.exact_seek_ms
            );
        }
    }
}
fn optional_number(name: &str) -> Option<f64> {
    std::env::var(name).ok().map(|raw| {
        let value = raw
            .parse::<f64>()
            .unwrap_or_else(|_| panic!("invalid {name}: {raw}"));
        assert!(
            value.is_finite() && value >= 0.0,
            "{name} must be finite and nonnegative"
        );
        value
    })
}
fn percentile(values: &[f64], fraction: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[((sorted.len() - 1) as f64 * fraction).ceil() as usize]
}

fn gen_clip(ffmpeg: &Path, out: &Path, w: u32, h: u32, seconds: u32, mixed: bool) {
    let source = format!("testsrc2=size={w}x{h}:rate=30:duration={seconds}");
    let mut command = Command::new(ffmpeg);
    command
        .args(["-y", "-nostdin", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg(source);
    if mixed {
        command.args(["-f", "lavfi", "-i"]).arg(format!(
            "sine=frequency=440:sample_rate=48000:duration={seconds}"
        ));
    }
    command.args([
        "-c:v", "libx264", "-preset", "medium", "-pix_fmt", "yuv420p", "-g", "60", "-bf", "2",
    ]);
    if mixed {
        command.args(["-c:a", "aac", "-af", "volume=0.05", "-shortest"]);
    }
    let status = command.arg(out).status().expect("spawn fixture ffmpeg");
    assert!(status.success(), "fixture encoding failed");
}
fn wait_inspection(session: &EngineSession, request: u64) -> Duration {
    let start = Instant::now();
    loop {
        if session
            .latest_frame()
            .is_some_and(|frame| frame.inspection_request_id == Some(request))
        {
            return start.elapsed();
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "inspection {request} timed out; status: {:?}",
            session.status()
        );
        std::thread::sleep(POLL);
    }
}

fn play_and_measure(
    gpu: &GpuContext,
    clip: &Path,
    proxy: Option<&Path>,
    w: u32,
    h: u32,
    case: &str,
    quality: PreviewQuality,
    seconds: f64,
    mixed: bool,
) -> BenchResult {
    let mut project = TimelineProject::new();
    let mut asset = MediaAsset::from_file(AssetKind::Video, clip.to_path_buf());
    if let Some(proxy) = proxy {
        asset.proxy = Some(ProxyRef::ready_generated(proxy.to_path_buf()));
    }
    let asset = project.media.insert(asset);
    let rate = FrameRate::FPS_30;
    let mut sequence = Sequence::new("throughput", rate, w, h);
    let sequence_id = sequence.id;
    let mut track = Track::new(TrackKind::Video, "V1");
    track.clips.push(Clip::new(
        ClipSource::Asset { asset },
        Tick::ZERO,
        Tick::from_seconds(seconds.ceil() as i64 + 2),
    ));
    if mixed {
        let mut blur = ClipEffect::new(EffectKind::Blur);
        blur.params.base.set("params.radius", PropValue::Float(4.0));
        track.clips[0].effects.push(blur);
        let duration = Tick::from_seconds(seconds.ceil() as i64 + 2);
        let mut captions = CaptionTrack::new("benchmark captions");
        captions.cues.push(CaptionCue::new(
            Tick::ZERO,
            duration,
            vec![CaptionWord::new(
                "Photonic playback benchmark",
                Tick::ZERO,
                duration,
            )],
        ));
        sequence.caption_tracks.push(captions);
        let mut audio_track = Track::new(TrackKind::Audio, "A1");
        let mut audio_clip = Clip::new(ClipSource::Asset { asset }, Tick::ZERO, duration);
        audio_clip.audio = Some(ClipAudio::new());
        audio_track.clips.push(audio_clip);
        sequence.audio_tracks.push(audio_track);
    }
    sequence.video_tracks.push(track);
    project.insert_sequence(sequence);
    project.active_sequence = Some(sequence_id);
    let mut document = Document::new("throughput", f64::from(w), f64::from(h));
    document.timeline = Some(project);
    let session = VideoEngine::new(gpu.clone()).open_session(
        Arc::new(Mutex::new(document)),
        Arc::new(Mutex::new(CommandHistory::new(64))),
    );
    let proxy_mode = if proxy.is_some() {
        ProxyMode::ForceProxy
    } else {
        ProxyMode::ForceOriginal
    };
    assert!(session.send(EngineCmd::SetPreviewQuality(quality)));
    assert!(session.send(EngineCmd::SetProxyMode(proxy_mode)));
    assert!(session.send(EngineCmd::InspectFrame {
        request_id: 1,
        sequence: sequence_id,
        time: Tick::ZERO,
        proxy_mode,
        quality,
        scope_tap: ScopeTapPoint::Program
    }));
    let cold_inspection_ms = wait_inspection(&session, 1).as_secs_f64() * 1000.0;
    // Record a coherent published baseline after the initial frame/status pair.
    let ready_deadline = Instant::now() + Duration::from_secs(1);
    while session.status().frames_published == 0 && Instant::now() < ready_deadline {
        std::thread::sleep(POLL);
    }
    let before = session.preview_telemetry();
    let dropped_before = session.status().dropped;
    let underruns_before = session.status().audio_xruns;
    let worker_before = photonic_video::decode::worker::worker_stats();
    let mut observed = FrameObservation::with_initial(session.latest_frame().unwrap().time);
    let started = Instant::now();
    assert!(session.send(EngineCmd::Play));
    while started.elapsed().as_secs_f64() < seconds {
        if let Some(frame) = session.latest_frame() {
            if frame.sequence == sequence_id
                && frame.preview_asset.is_none()
                && frame.preview_quality == quality
                && frame.proxy_mode == proxy_mode
            {
                observed.observe(frame.time, started.elapsed());
            }
        }
        std::thread::sleep(POLL);
    }
    let elapsed = started.elapsed();
    let after = session.preview_telemetry();
    let status = session.status();
    let worker_after = photonic_video::decode::worker::worker_stats();
    assert!(session.send(EngineCmd::Pause));
    let mut exact_seek_ms = Vec::new();
    for (index, source_seconds) in [seconds * 0.75, 0.25, seconds * 0.5]
        .into_iter()
        .enumerate()
    {
        let time = rate.frame_start((source_seconds * 30.0).floor() as i64);
        let request_id = index as u64 + 2;
        assert!(session.send(EngineCmd::InspectFrame {
            request_id,
            sequence: sequence_id,
            time,
            proxy_mode,
            quality,
            scope_tap: ScopeTapPoint::Program
        }));
        exact_seek_ms.push(wait_inspection(&session, request_id).as_secs_f64() * 1000.0);
    }
    let result = BenchResult {
        case: case.to_owned(),
        source: "generated testsrc2 H264 yuv420p GOP60 B2".into(),
        workload: if mixed { "mixed" } else { "video" }.into(),
        source_dimensions: [w, h],
        source_fps: 30.0,
        quality: format!("{quality:?}"),
        proxy_policy: format!("{proxy_mode:?}"),
        selected_input: if proxy.is_some() {
            "generated proxy"
        } else {
            "original"
        }
        .into(),
        window_seconds: elapsed.as_secs_f64(),
        observed_unique_frames: observed.unique_new,
        observed_unique_fps: observed.unique_new as f64 / elapsed.as_secs_f64(),
        engine_publications: after
            .frames_published
            .saturating_sub(before.frames_published),
        evaluations: after.evaluations.saturating_sub(before.evaluations),
        incomplete_evaluations: after
            .evaluation_misses
            .saturating_sub(before.evaluation_misses),
        cover_interval_drops: status.dropped.saturating_sub(dropped_before),
        repeated_polls: observed.repeated_polls,
        longest_hold_ms: observed.longest_hold_ms(elapsed),
        frame_interval_p95_ms: percentile(&observed.intervals_ms, 0.95),
        cold_inspection_ms,
        seek_p95_ms: percentile(&exact_seek_ms, 0.95),
        exact_seek_ms,
        audio_device_active: mixed && status.master_level.is_some(),
        audio_underrun_frames: status.audio_xruns.saturating_sub(underruns_before),
        decode_ring_hits: after.ring_hits.saturating_sub(before.ring_hits),
        decode_ring_misses: after.decode_misses.saturating_sub(before.decode_misses),
        worker_seeks: worker_after.0.saturating_sub(worker_before.0),
        worker_pumped: worker_after.1.saturating_sub(worker_before.1),
        managed_gpu_cache_bytes: status.memory.gpu_cache_bytes,
        decoded_ring_bytes: status.memory.decoded_ring_bytes,
    };
    session.shutdown();
    result
}

struct TempFixtures(PathBuf);
impl Drop for TempFixtures {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
#[ignore = "real GPU/FFmpeg benchmark; run explicitly or with a named hardware budget"]
fn playback_throughput_full_vs_proxy() {
    let budgets = Budgets::from_env();
    let required = budgets.enabled() || std::env::var_os("PHOTONIC_REQUIRE_GPU").is_some();
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });
    let Some(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
    else {
        assert!(!required, "required GPU adapter unavailable");
        eprintln!("SKIP: no GPU adapter");
        return;
    };
    let adapter_info = adapter.get_info();
    if let Ok(expected) = std::env::var("PHOTONIC_BENCH_EXPECT_ADAPTER") {
        assert!(
            adapter_info
                .name
                .to_lowercase()
                .contains(&expected.to_lowercase()),
            "adapter {} does not match {expected}",
            adapter_info.name
        );
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default(), None))
        .expect("request benchmark GPU");
    let gpu = GpuContext::new(Arc::new(device), Arc::new(queue));
    let Some(tools) = locate_for_test() else {
        assert!(!required, "required ffmpeg/ffprobe unavailable");
        eprintln!("SKIP: no ffmpeg/ffprobe");
        return;
    };
    let seconds = optional_number("PHOTONIC_BENCH_SECONDS").unwrap_or(8.0);
    assert!(
        (1.0..=30.0).contains(&seconds),
        "benchmark window must be 1..=30 seconds"
    );
    let selected = std::env::var("PHOTONIC_BENCH_CASE").ok();
    let workload = std::env::var("PHOTONIC_BENCH_WORKLOAD").unwrap_or_else(|_| "video".into());
    assert!(
        matches!(workload.as_str(), "video" | "mixed"),
        "unknown PHOTONIC_BENCH_WORKLOAD"
    );
    let mixed = workload == "mixed";
    let fixtures = TempFixtures(
        std::env::temp_dir().join(format!("photonic-playback-bench-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir_all(&fixtures.0).unwrap();
    let mut results = Vec::new();
    for (resolution, w, h) in [("1080p", 1920, 1080), ("4k", 3840, 2160)] {
        let cases = [
            ("full_original", PreviewQuality::Full, false),
            ("draft_original", PreviewQuality::Draft, false),
            ("draft_proxy", PreviewQuality::Draft, true),
        ];
        let selected_cases: Vec<_> = cases
            .into_iter()
            .filter(|(suffix, _, _)| {
                selected
                    .as_ref()
                    .is_none_or(|selected| selected == &format!("{resolution}_{suffix}"))
            })
            .collect();
        if selected_cases.is_empty() {
            continue;
        }
        let clip = fixtures.0.join(format!("{resolution}.mp4"));
        gen_clip(&tools.ffmpeg, &clip, w, h, seconds.ceil() as u32 + 3, mixed);
        let proxy = fixtures.0.join(format!("{resolution}.proxy.mp4"));
        if selected_cases.iter().any(|(_, _, proxy)| *proxy) {
            generate_proxy(&tools, &clip, &proxy, &|| false).expect("generate proxy");
        }
        for (suffix, quality, use_proxy) in selected_cases {
            let result = play_and_measure(
                &gpu,
                &clip,
                use_proxy.then_some(proxy.as_path()),
                w,
                h,
                &format!("{resolution}_{suffix}"),
                quality,
                seconds,
                mixed,
            );
            println!(
                "PHOTONIC_PLAYBACK_CASE {}",
                serde_json::to_string(&result).unwrap()
            );
            results.push(result);
        }
    }
    assert!(!results.is_empty(), "unknown PHOTONIC_BENCH_CASE");
    let ffmpeg_version = Command::new(&tools.ffmpeg)
        .arg("-version")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| text.lines().next().map(str::to_owned));
    let report = serde_json::json!({"hardware_profile":std::env::var("PHOTONIC_BENCH_HARDWARE").ok(),"debug_assertions":cfg!(debug_assertions),"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"adapter":{"name":adapter_info.name,"vendor":adapter_info.vendor,"device":adapter_info.device,"device_type":format!("{:?}",adapter_info.device_type),"backend":format!("{:?}",adapter_info.backend),"driver":adapter_info.driver,"driver_info":adapter_info.driver_info},"ffmpeg":ffmpeg_version,"poll_ms":POLL.as_millis(),"results":results});
    println!(
        "PHOTONIC_PLAYBACK_REPORT {}",
        serde_json::to_string(&report).unwrap()
    );
    if let Ok(path) = std::env::var("PHOTONIC_BENCH_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap())
            .expect("write benchmark report");
    }
    for result in &results {
        budgets.assert_result(result);
    }
}

#[test]
fn t011_unique_publications_exclude_repeated_ticks_and_initial_frame() {
    let mut observed = FrameObservation::with_initial(Tick(0));
    for _ in 0..100 {
        observed.observe(Tick(0), Duration::from_millis(20));
    }
    observed.observe(Tick(1), Duration::from_millis(33));
    observed.observe(Tick(2), Duration::from_millis(66));
    observed.observe(Tick(2), Duration::from_millis(100));
    assert_eq!(observed.unique_new, 2);
    assert_eq!(observed.repeated_polls, 101);
    assert_eq!(observed.longest_hold_ms(Duration::from_millis(200)), 134.0);
    assert_eq!(percentile(&observed.intervals_ms, 0.95), 33.0);
}
