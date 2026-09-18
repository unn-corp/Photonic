# Playback measurement and hardware budgets

Run `scripts/check_playback_budget.sh` for the six default cases: 1080p and 4K, each at Full with originals, Draft with originals, and Draft with generated proxies. Fixtures are moving H.264 video at 30 fps with a 60-frame GOP and two B frames. Each case uses a fresh real engine session; playback is observed for eight seconds after the first exact frame is ready.

`observed_unique_fps` counts unique program-frame ticks actually published and observed by the polling consumer. A repeatedly held frame and the frame already displayed before timing add no frames. This measures the headless producer/consumer path; it does not measure GUI composition, display refresh, or physical scanout. `engine_publications` separately reports the session's publication-counter delta. Ring hits and worker pump counts are diagnostics and never count as presented FPS.

The JSON report includes GPU adapter name, backend, driver, OS, architecture, debug/release configuration, FFmpeg version, source format, explicit preview quality and selected input. Each case reports cold-inspection latency, three exact paused seek latencies and their p95, longest unchanged-frame interval, p95 frame interval, evaluation misses, clock drops, audio underruns and managed cache bytes. Polling has a four-millisecond resolution. `repeated_polls` is expected even during smooth playback; use unique FPS and interval measurements to assess cadence.

Set `PHOTONIC_BENCH_WORKLOAD=mixed` to add a four-pixel blur, caption overlay, and a quiet generated AAC sine source on an audio track. The default `video` workload isolates video. The report states whether a real audio device and feeder were active; a mixed workload with hardware budgets fails if no audio device is available, rather than treating absent audio as zero underruns.

To measure a single case:

```sh
PHOTONIC_BENCH_CASE=1080p_full_original \
PHOTONIC_BENCH_SECONDS=8 \
PHOTONIC_BENCH_REPORT=/tmp/photonic-playback.json \
scripts/check_playback_budget.sh
```

Valid case names are `1080p_full_original`, `1080p_draft_original`, `1080p_draft_proxy`, `4k_full_original`, `4k_draft_original` and `4k_draft_proxy`. The optional duration is from 1 to 30 seconds. Report output is printed with the `PHOTONIC_PLAYBACK_REPORT` prefix and can also be written to the configured file; its parent directory must exist.

Hardware gates require an explicit case and a meaningful local hardware profile. Set only budgets established by measurement on that machine; this example shows the variable names without inventing portable limits:

```sh
export PHOTONIC_BENCH_HARDWARE='lab-machine-gpu-driver-profile'
export PHOTONIC_BENCH_CASE=4k_full_original
export PHOTONIC_BENCH_EXPECT_ADAPTER='adapter-name-substring'
export PHOTONIC_BENCH_MIN_OBSERVED_FPS="$MEASURED_MIN_FPS"
export PHOTONIC_BENCH_MAX_HOLD_MS="$MEASURED_MAX_HOLD_MS"
export PHOTONIC_BENCH_MAX_SEEK_MS="$MEASURED_MAX_SEEK_MS"
scripts/check_playback_budget.sh
```

Any configured budget makes missing GPU or FFmpeg a failure. `PHOTONIC_REQUIRE_GPU=1` also enforces hardware availability in a report-only run. A mismatched adapter name fails when `PHOTONIC_BENCH_EXPECT_ADAPTER` is set. Reports are written before budget assertions so a failed gate retains evidence. Run the same case, driver, power configuration and build profile for a comparison; debug-build timings are diagnostic rather than release performance baselines.

Required queue, cache and source-readiness regressions do not depend on optional performance budgets:

```sh
cargo test -p photonic-video --lib t005 -- --test-threads=2
cargo test -p photonic-video --test playback_throughput_bench t011_
```

The GPU-backed T005 cases honor `PHOTONIC_REQUIRE_GPU`; GPU-free tests always exercise bounded command admission, sticky-setting restoration, snapshot ownership, per-cache bytes, ring pressure and texture recycling. Managed cache budgets are per session. FFmpeg, audio, renderer scratch, other sessions, and textures retained outside the caches are separate allocations.
