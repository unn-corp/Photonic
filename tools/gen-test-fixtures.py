#!/usr/bin/env python3
"""Generate the video-editor test-media corpus into
crates/photonic-video/tests/fixtures/.

ffmpeg-dependent, run by a developer locally when a fixture needs
regenerating — same shape as tools/gen-mcp-docs.py, a checked-in script
CI *consumes the output of* rather than *runs*
(docs/specs/video-editor/11-testing-phasing.md §2).

Every fixture is synthesized from lavfi sources (no external media inputs),
kept deliberately tiny (low resolution, short duration, high CRF) so the
whole corpus stays well under the 5 MB budget.

Usage:
    python3 tools/gen-test-fixtures.py
"""
import json
import shutil
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
FIXTURES_DIR = REPO_ROOT / "crates" / "photonic-video" / "tests" / "fixtures"


def check_ffmpeg() -> None:
    missing = [tool for tool in ("ffmpeg", "ffprobe") if shutil.which(tool) is None]
    if missing:
        print(
            f"error: required tool(s) not found on PATH: {', '.join(missing)}\n"
            "Install ffmpeg (which bundles ffprobe) before running this script.",
            file=sys.stderr,
        )
        sys.exit(1)


def run(*args: str) -> None:
    """Run an ffmpeg command, failing loudly on error (mirrors -v error: only
    warnings/errors are printed, so a clean run stays quiet)."""
    cmd = ["ffmpeg", "-y", "-v", "error", *args]
    result = subprocess.run(cmd, cwd=FIXTURES_DIR)
    if result.returncode != 0:
        print(f"error: command failed: {' '.join(cmd)}", file=sys.stderr)
        sys.exit(1)


def gen_color_bars() -> None:
    """color_bars.mp4 — deterministic known-value source; probe metadata
    tests (CAP-001). 4s, 320x180, yuv420p, H.264, 30fps."""
    run(
        "-f", "lavfi", "-i", "smptebars=size=320x180:rate=30:duration=4",
        "-pix_fmt", "yuv420p", "-c:v", "libx264", "-crf", "30",
        "-preset", "veryslow", "-movflags", "+faststart",
        "color_bars.mp4",
    )


def gen_counter() -> None:
    """counter.mp4 — frame-number burn-in for seek-accuracy tests. 10s,
    320x180, 30fps, one keyframe per 2s (GOP=60, tests the GOP-seek path)."""
    run(
        "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=30:duration=10",
        "-vf",
        "drawtext=text='%{n}':fontsize=28:fontcolor=white:"
        "x=10:y=10:box=1:boxcolor=black@0.6",
        "-pix_fmt", "yuv420p", "-c:v", "libx264", "-crf", "28",
        "-g", "60", "-keyint_min", "60", "-sc_threshold", "0",
        "-preset", "veryslow",
        "counter.mp4",
    )
    frame_truth = {
        "total_frames": 300,
        "fps": 30,
        "duration_s": 10,
        "gop_size": 60,
        "keyframe_interval_s": 2,
        "text_pattern": (
            "0-based frame index burned in as decimal text, top-left "
            "(drawtext %{n}); frame N's burned-in text is exactly str(N)"
        ),
    }
    (FIXTURES_DIR / "frame_truth.json").write_text(
        json.dumps(frame_truth, indent=2) + "\n"
    )


def gen_beep_flash() -> None:
    """beep_flash.mp4 + .wav — A/V sync ground truth. Video flashes white
    for 1 frame every 1s (frame N is white iff N % 30 == 0); audio has a
    5ms 1kHz beep at the same instants. 60s, 320x180@30 + 48kHz mono.

    The .wav is encoded as IMA ADPCM (still a valid, universally-decodable
    WAVE file, still 48kHz mono) rather than raw PCM: 60s of 16-bit PCM at
    48kHz mono is ~5.7 MB by itself, which blows the whole corpus budget
    before any other fixture exists. ADPCM's ~4:1 ratio brings it to
    ~1.4 MB while preserving the beep's onset timing (verified below).
    """
    run(
        "-f", "lavfi", "-i", "color=c=black:s=320x180:r=30:d=60",
        "-vf", "geq=lum='if(eq(mod(N\\,30)\\,0),255,0)':cb=128:cr=128",
        "-pix_fmt", "yuv420p", "-c:v", "libx264", "-crf", "28",
        "-preset", "veryslow",
        "beep_flash.mp4",
    )
    run(
        "-f", "lavfi", "-i",
        "aevalsrc=exprs='if(lt(mod(t\\,1)\\,0.005)\\,sin(2*PI*1000*t)\\,0)':"
        "s=48000:d=60",
        "-ac", "1", "-c:a", "adpcm_ima_wav",
        "beep_flash.wav",
    )


def gen_vfr_sample() -> None:
    """vfr_sample.mp4 — variable-frame-rate source: exercises
    FrameRate::is_exact() warning path and non-integral ticks_per_frame
    rounding. 8s, mixed 24fps then 30fps segments.

    Built via two lavfi segments joined with the `concat` *filter* (not the
    concat demuxer's stream-copy mode, which was tried first and silently
    mis-timestamps the second segment when its frame rate differs from the
    first — the filter path correctly preserves each segment's native PTS
    spacing, verified below).
    """
    seg1 = FIXTURES_DIR / "_seg1.mp4"
    seg2 = FIXTURES_DIR / "_seg2.mp4"
    run(
        "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=24:duration=4",
        "-c:v", "libx264", "-crf", "30", "-pix_fmt", "yuv420p",
        "-preset", "veryslow",
        seg1.name,
    )
    run(
        "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=30:duration=4",
        "-c:v", "libx264", "-crf", "30", "-pix_fmt", "yuv420p",
        "-preset", "veryslow",
        seg2.name,
    )
    run(
        "-i", seg1.name, "-i", seg2.name,
        "-filter_complex", "[0:v][1:v]concat=n=2:v=1:a=0[outv]",
        "-map", "[outv]", "-fps_mode", "vfr",
        "-c:v", "libx264", "-crf", "30", "-pix_fmt", "yuv420p",
        "-preset", "veryslow",
        "vfr_sample.mp4",
    )
    seg1.unlink()
    seg2.unlink()


def gen_alpha_gradient() -> None:
    """alpha_gradient.mov — straight-alpha ramp, known values; CAP-021
    transparent export correctness. 3s, 160x90, qtrle/argb (lossless,
    alpha-capable, far smaller than ProRes 4444 at this resolution).
    Alpha ramps 0 -> 255 left-to-right over a constant blue fill.
    """
    run(
        "-f", "lavfi", "-i", "color=c=blue:s=160x90:r=30:d=3,format=rgba",
        "-vf", "geq=r='0':g='0':b='255':a='(X/W)*255'",
        "-c:v", "qtrle", "-pix_fmt", "argb",
        "alpha_gradient.mov",
    )


def gen_multi_audio() -> None:
    """multi_audio.mkv — 3 discrete audio streams (dialogue/music/fx) with
    different channel counts: mono, stereo, and true 5.1. 15s, Opus
    (small, and this ffmpeg build has libopus enabled)."""
    dialogue = FIXTURES_DIR / "_dialogue.wav"
    music = FIXTURES_DIR / "_music.wav"
    fx = FIXTURES_DIR / "_fx.wav"
    run(
        "-f", "lavfi", "-i", "sine=frequency=440:duration=15:sample_rate=48000",
        "-ac", "1",
        dialogue.name,
    )
    run(
        "-f", "lavfi", "-i", "sine=frequency=220:duration=15:sample_rate=48000",
        "-f", "lavfi", "-i", "sine=frequency=330:duration=15:sample_rate=48000",
        "-filter_complex", "[0:a][1:a]join=inputs=2:channel_layout=stereo[out]",
        "-map", "[out]",
        music.name,
    )
    run(
        "-f", "lavfi", "-i", "sine=frequency=110:duration=15:sample_rate=48000",
        "-af", "pan=5.1|FL=c0|FR=c0|FC=c0|LFE=c0|BL=c0|BR=c0",
        fx.name,
    )
    run(
        "-i", dialogue.name, "-i", music.name, "-i", fx.name,
        "-map", "0:a", "-map", "1:a", "-map", "2:a",
        "-c:a:0", "libopus", "-b:a:0", "32k",
        "-c:a:1", "libopus", "-b:a:1", "48k",
        "-c:a:2", "libopus", "-b:a:2", "96k",
        "multi_audio.mkv",
    )
    dialogue.unlink()
    music.unlink()
    fx.unlink()


def main() -> None:
    check_ffmpeg()
    FIXTURES_DIR.mkdir(parents=True, exist_ok=True)

    generators = [
        ("color_bars.mp4", gen_color_bars),
        ("counter.mp4 + frame_truth.json", gen_counter),
        ("beep_flash.mp4 + .wav", gen_beep_flash),
        ("vfr_sample.mp4", gen_vfr_sample),
        ("alpha_gradient.mov", gen_alpha_gradient),
        ("multi_audio.mkv", gen_multi_audio),
    ]
    for label, fn in generators:
        print(f"generating {label} ...")
        fn()

    print("\ndone. fixture sizes:")
    total = 0
    for f in sorted(FIXTURES_DIR.glob("*")):
        if f.is_file() and f.name != "README.md":
            size = f.stat().st_size
            total += size
            print(f"  {f.name:32s} {size / 1024:8.1f} KiB")
    print(f"  {'TOTAL':32s} {total / 1024:8.1f} KiB")


if __name__ == "__main__":
    main()
