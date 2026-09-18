#!/usr/bin/env python3
"""AS-2 "Short film" acceptance story — the FULL slice (CAP-019/SS-2).

Drives a running Photonic MCP server over HTTP JSON-RPC (the same `POST
/mcp` endpoint `crates/photonic-mcp/src/server.rs` exposes), exercising
every beat `docs/specs/video-editor/00-overview.md` §2 lists for "AS-2 Short
film": "Import several 4K clips -> generate proxies -> multi-track edit with
cross-dissolves -> one shot opened as per-clip node composition (keyed
overlay merge) -> full grade pass with scopes (CDL wheels + curves + LUT)
-> audio mix with keyframed automation, EQ on dialogue track, music ducking
-> export master (AV1 high-quality) + web H.264." Per
`11-testing-phasing.md` §3.4/§6.4/P8's "AS-2 and AS-3 both fully complete"
gate, this is that closing script — P6 (transitions), P7 (grade/scopes) and
P8 (node graph, mixer) capabilities are all committed now.

Two documented slices of the spec bullet above:
- The mixer beat exercises `set_track_audio` + `audio_fx` (EQ, compressor)
  only — keyframed gain automation and sidechain ducking are real capabilities
  (09-audio-mixer.md) but a separate, heavier acceptance concern than "the
  mixer tools work end-to-end"; out of scope for this script (see
  `s_mixer_touch` below).
- "Keyed overlay merge" is stood in for by a `SolidColor` + `Merge(screen)`
  node pair (`s_node_composition` below) rather than a real `ChromaKey` node
  — no keyed-subject fixture exists in the committed test-media corpus to
  key against, and the acceptance point here is the composition *mechanics*
  (create/add-node/add-edge/get_graph-compiles), not chroma-key correctness
  (that's `photonic-video`'s own eval tests' job).

No real 4K fixture exists in the committed test-media corpus
(`crates/photonic-video/tests/fixtures/README.md` — everything tops out at
320x180 to stay inside the 5 MB corpus budget). This script uses
`beep_flash.mp4`, the corpus's longest/heaviest real asset (60s), as a
stand-in for "a big imported clip" — the resolution doesn't matter for what
the proxy slice actually exercises: `generate_proxies` currently reports
NotSupportedV1 regardless of source size (see `s_generate_proxies` below),
and `set_proxy_mode`/editing behave identically at any resolution. The
multi-track/grade/mixer/export beats import a second, ordinary fixture
(`color_bars.mp4`) — see `s_import_second_fixture`.

Usage
-----
    # 1. Launch a headless Photonic MCP server (no GUI window):
    cargo run -p photonic-app -- --headless

    # 2. Run this script against it (default: http://127.0.0.1:7842/mcp):
    python3 tools/as2_proxy_edit.py

Exit code is 0 iff every step passed or was skipped (skips are reserved for
environment gaps — no GPU adapter, no ffmpeg — this script can't control);
any FAIL exits nonzero, so this is CI-runnable once a GPU+ffmpeg runner is
available.

Stdlib-only by design — no `requests` dependency, so this runs anywhere a
plain `python3` does. The HTTP client / step-runner below is intentionally
duplicated from `tools/as1_arrange_cut.py` rather than shared through a
local module — each story script stays a single self-contained file, same
convention as `tools/gen-mcp-docs.py` / `tools/gen-test-fixtures.py`.
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
import time
import uuid
from pathlib import Path
from typing import Any, Optional

REPO_ROOT = Path(__file__).resolve().parent.parent
FIXTURES_DIR = REPO_ROOT / "crates" / "photonic-video" / "tests" / "fixtures"

# crates/photonic-core/src/timeline/time.rs — exact ticks-per-second, chosen
# so every common frame rate (23.976 .. 120fps) divides it cleanly.
TICKS_PER_SECOND = 705_600_000

DEFAULT_URL = os.environ.get("PHOTONIC_MCP_URL", "http://127.0.0.1:7842/mcp")


def ticks(seconds: float) -> int:
    """Seconds -> exact ticks. Safe for this script's 30fps sequence (30
    divides TICKS_PER_SECOND with no remainder)."""
    return round(seconds * TICKS_PER_SECOND)


# ─── Minimal HTTP JSON-RPC client ──────────────────────────────────────────


class RpcError(RuntimeError):
    pass


class ToolResult:
    """Decodes an MCP `tools/call` result (content[] + isError) per
    `crates/photonic-mcp/src/protocol/args/c.rs::ToolResult`. Every
    `.with_data(...)` call appends a second `content` text block that is
    itself pretty-printed JSON (`ContentItem::json`) — this parses whichever
    blocks decode as a JSON object, later blocks winning on key collision."""

    def __init__(self, tool_name: str, raw: dict):
        self.tool_name = tool_name
        self.raw = raw
        self.is_error = bool(raw.get("isError"))
        texts = [c.get("text", "") for c in raw.get("content", []) if c.get("type") == "text"]
        self.message = texts[0] if texts else ""
        self.data: dict[str, Any] = {}
        for t in texts[1:]:
            try:
                parsed = json.loads(t)
            except (json.JSONDecodeError, TypeError):
                continue
            if isinstance(parsed, dict):
                self.data.update(parsed)

    @property
    def error_code(self) -> Optional[str]:
        return self.data.get("error_code")

    def __repr__(self) -> str:
        return (
            f"ToolResult({self.tool_name}, is_error={self.is_error}, "
            f"message={self.message!r}, data={self.data!r})"
        )


class Client:
    """Talks JSON-RPC 2.0 to the Photonic MCP server's single `POST /mcp`
    endpoint (server.rs::build_router)."""

    def __init__(self, url: str):
        self.url = url
        self._next_id = 1

    def call(self, method: str, params: Optional[dict] = None) -> dict:
        req_id = self._next_id
        self._next_id += 1
        body = json.dumps(
            {"jsonrpc": "2.0", "id": req_id, "method": method, "params": params or {}}
        ).encode("utf-8")
        req = urllib.request.Request(
            self.url, data=body, headers={"Content-Type": "application/json", "x-mcp-secret": os.environ["PHOTONIC_MCP_SECRET"]}, method="POST"
        )
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                payload = json.loads(resp.read().decode("utf-8"))
        except urllib.error.URLError as e:
            raise RpcError(
                f"cannot reach Photonic MCP server at {self.url} ({e}) — is it running? "
                "See this script's module docstring for how to launch one."
            ) from e
        if "error" in payload:
            err = payload["error"]
            raise RpcError(f"{method}: JSON-RPC error {err.get('code')}: {err.get('message')}")
        return payload["result"]

    def tool(self, name: str, arguments: Optional[dict] = None) -> ToolResult:
        result = self.call("tools/call", {"name": name, "arguments": arguments or {}})
        return ToolResult(name, result)


# ─── Step runner ────────────────────────────────────────────────────────────


class Skip(Exception):
    """Raise from a step to mark it skipped (environment gap, not a bug)."""


class Runner:
    def __init__(self, strict: bool = False):
        self.total = 0
        self.failed = 0
        self.skipped = 0
        # `--strict` / PHOTONIC_AS_STRICT=1: treat an environment skip as a
        # failure. Without it, a runner with no GPU adapter and no ffmpeg
        # skips every step that actually renders or encodes anything and
        # still exits 0 — a green CI run that verified nothing. CI gates on
        # a machine that IS supposed to have a GPU + ffmpeg must pass
        # `--strict` so a silently-degraded runner fails loudly instead.
        self.strict = strict

    def step(self, name: str, fn) -> Any:
        """Run one step, print its PASS/FAIL/SKIP line, and return whatever
        `fn` returns (so later steps can chain off earlier results) —
        `None` on failure/skip."""
        self.total += 1
        try:
            result = fn()
        except Skip as e:
            if self.strict:
                self.failed += 1
                print(f"[FAIL] {name}: {e} (skip promoted to failure by --strict)")
                return None
            self.skipped += 1
            print(f"[SKIP] {name}: {e}")
            return None
        except (AssertionError, RpcError) as e:
            self.failed += 1
            print(f"[FAIL] {name}: {e}")
            return None
        except Exception as e:  # noqa: BLE001 — last-resort net so one bad step doesn't kill the report
            self.failed += 1
            print(f"[FAIL] {name}: unexpected {type(e).__name__}: {e}")
            return None
        print(f"[PASS] {name}")
        return result

    def summary(self) -> int:
        passed = self.total - self.failed - self.skipped
        print(
            f"\n{passed}/{self.total} passed, {self.skipped} skipped, "
            f"{self.failed} failed"
        )
        return 1 if self.failed else 0


def expect_ok(r: ToolResult) -> ToolResult:
    if r.is_error:
        raise AssertionError(f"{r.tool_name} returned an error: {r.message!r} (data={r.data!r})")
    return r


def engine_available(client: Client) -> bool:
    """Mirrors `handlers/video.rs`'s own test-only `engine_available()` —
    engine-backed tools need a GPU adapter; skip cleanly if this machine
    doesn't have one rather than failing the whole run."""
    return not client.tool("get_engine_status", {}).is_error


def ffprobe_video_stream(path: str) -> dict:
    """`ffprobe -show_entries stream=... -of json`'s first video stream, as a
    dict — the same tag/field convention
    `crates/photonic-video/tests/export_synthetic.rs`'s own e2e export tests
    use (`ffprobe_json` / `video_stream` there) to verify real encoder
    output. Raises `Skip` if `ffprobe` isn't on `PATH` (a separate binary
    from `ffmpeg`, so a distinct environment gap from `FfmpegUnavailable`)."""
    try:
        proc = subprocess.run(
            [
                "ffprobe", "-v", "error", "-select_streams", "v:0",
                "-show_entries", "stream=codec_name,width,height",
                "-show_entries", "stream_tags=alpha_mode",
                "-of", "json", path,
            ],
            capture_output=True, text=True, timeout=30,
        )
    except FileNotFoundError as e:
        raise Skip(f"ffprobe not found ({e}) — install ffmpeg's ffprobe") from e
    assert proc.returncode == 0, f"ffprobe failed on {path}: {proc.stderr}"
    streams = json.loads(proc.stdout).get("streams", [])
    assert streams, f"ffprobe found no video stream in {path}"
    return streams[0]


def identity_lut_path(tmp_dir: str) -> str:
    """Write a minimal-but-valid 2x2x2 identity `.cube` LUT (`LUT_3D_SIZE 2`,
    red-fastest per `photonic_render::lut::parse_cube`'s doc comment) to a
    temp file and return its path — no `.cube` fixture exists in the
    committed test-media corpus, and an identity table is the simplest input
    that round-trips through `apply_lut`'s real `parse_cube` validation.
    PID-suffixed so concurrent runs of this script don't clobber each
    other's LUT file mid-run."""
    path = os.path.join(tmp_dir, f"photonic_as2_identity_{os.getpid()}.cube")
    with open(path, "w", encoding="utf-8") as f:
        f.write("LUT_3D_SIZE 2\n")
        for b in (0.0, 1.0):
            for g in (0.0, 1.0):
                for r in (0.0, 1.0):
                    f.write(f"{r} {g} {b}\n")
    return path


# ─── Story steps ────────────────────────────────────────────────────────────


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default=DEFAULT_URL, help=f"MCP endpoint (default: {DEFAULT_URL})")
    parser.add_argument(
        "--out-dir",
        default=None,
        help="Persist the AV1/H.264 exports to this directory instead of an "
        "auto-deleted temp dir (the two deliverable acceptance videos).",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        default=os.environ.get("PHOTONIC_AS_STRICT") == "1",
        help="Fail (rather than skip) on an environment gap — no GPU adapter, "
        "no ffmpeg/ffprobe. Use in CI so a degraded runner can't report a "
        "green run that verified nothing. [env: PHOTONIC_AS_STRICT=1]",
    )
    args = parser.parse_args()

    client = Client(args.url)
    run = Runner(strict=args.strict)
    state: dict[str, Any] = {}

    def s_connect():
        result = client.call(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "as2_proxy_edit.py", "version": "1"},
            },
        )
        assert result.get("serverInfo", {}).get("name") == "photonic", result

    run.step("connect (initialize)", s_connect)

    def s_create_sequence():
        r = expect_ok(
            client.tool(
                "create_sequence",
                {
                    "name": "AS-2 Sequence",
                    "frame_rate": {"num": 30, "den": 1},
                    "formats": [{"name": "16:9", "width": 320, "height": 180}],
                },
            )
        )
        state["sequence_id"] = r.data["sequence_id"]

    run.step("create_sequence", s_create_sequence)

    def s_import_4k_ish():
        # See module docstring — beep_flash.mp4 stands in for "a big
        # imported clip"; no real 4K fixture exists in the corpus.
        path = str(FIXTURES_DIR / "beep_flash.mp4")
        assert Path(path).exists(), f"fixture missing: {path} — run tools/gen-test-fixtures.py"
        r = expect_ok(client.tool("import_media", {"paths": [path], "bin": "AS-2 4K-ish stand-in"}))
        assets = r.data["assets"]
        assert len(assets) == 1, r.data
        state["asset_id"] = assets[0]["asset_id"]

    run.step("import_media (4K-ish stand-in fixture)", s_import_4k_ish)

    def s_generate_proxies():
        # CAP-014 (546260e): generate_proxies runs a real half-res all-intra
        # transcode as a background job. Start it, poll to completion, then
        # confirm the asset reports a ready proxy via list_media.
        r = client.tool("generate_proxies", {"asset_ids": [state["asset_id"]]})
        if r.is_error and r.error_code == "FfmpegUnavailable":
            raise Skip(f"{r.message} (set PHOTONIC_FFMPEG_DIR or install ffmpeg)")
        expect_ok(r)
        job_id = r.data["job_id"]
        deadline = time.monotonic() + 120
        while True:
            status = expect_ok(client.tool("get_job_status", {"job_id": job_id})).data["status"]
            if status["state"] not in ("queued", "running"):
                break
            assert time.monotonic() < deadline, f"proxy job did not finish (last: {status})"
            time.sleep(0.25)
        assert status["state"] == "done", f"proxy job ended in {status['state']!r}: {status}"
        assets = expect_ok(client.tool("list_media", {})).data["assets"]
        proxied = next(a for a in assets if a["asset_id"] == state["asset_id"])
        assert proxied["proxy_status"] == "ready", f"proxy not ready: {proxied}"

    run.step("generate_proxies (real transcode -> ready)", s_generate_proxies)

    def s_toggle_proxy_mode():
        # `set_proxy_mode`'s response echoes the Rust enum's Debug form
        # (session.rs::ProxyMode) — PascalCase, no underscore.
        expected = {"force_proxy": "ForceProxy", "force_original": "ForceOriginal", "auto": "Auto"}
        for mode in ("force_proxy", "force_original", "auto"):
            r = expect_ok(client.tool("set_proxy_mode", {"mode": mode}))
            assert r.data["mode"] == expected[mode], r.data

    run.step("set_proxy_mode (toggle force_proxy -> force_original -> auto)", s_toggle_proxy_mode)

    def s_edit_under_proxy_mode():
        # Editing must work the same regardless of proxy mode (proxies are
        # never required for correctness, CAP-014) — insert, split, trim on
        # the imported asset while the session is still on whatever mode the
        # previous step left it in (auto).
        v1 = expect_ok(client.tool("add_track", {"sequence_id": state["sequence_id"], "kind": "video", "name": "V1"}))
        track_id = v1.data["track_id"]
        clip = expect_ok(
            client.tool(
                "insert_clip",
                {
                    "track_id": track_id,
                    "name": "beep_flash",
                    "start_ticks": ticks(0),
                    "source": {"kind": "asset", "asset_id": state["asset_id"]},
                    "duration_ticks": ticks(4),
                },
            )
        )
        clip_id = clip.data["clip_id"]
        split = expect_ok(client.tool("split_clip", {"clip_id": clip_id, "at_ticks": ticks(2.5)}))
        new_clip_id = split.data["new_clip_id"]
        expect_ok(client.tool("trim_clip", {"clip_id": clip_id, "edge": "out", "new_ticks": ticks(2)}))

        clips = {
            c["clip_id"]: c
            for c in expect_ok(
                client.tool("list_clips", {"sequence_id": state["sequence_id"]})
            ).data["clips"]
        }
        assert clips[clip_id]["duration_ticks"] == ticks(2), clips[clip_id]
        assert clips[new_clip_id]["start_ticks"] == ticks(2.5), clips[new_clip_id]
        state["track_v1"] = track_id
        state["clip_id"] = clip_id
        state["new_clip_id"] = new_clip_id

    run.step("edit (insert/split/trim while proxy mode toggled)", s_edit_under_proxy_mode)

    def s_import_second_fixture():
        # A second, ordinary-size fixture for the multi-track/grade/mixer/
        # export beats below — see module docstring for why beep_flash.mp4
        # alone doesn't cover those.
        path = str(FIXTURES_DIR / "color_bars.mp4")
        assert Path(path).exists(), f"fixture missing: {path} — run tools/gen-test-fixtures.py"
        r = expect_ok(client.tool("import_media", {"paths": [path], "bin": "AS-2 Footage"}))
        assets = r.data["assets"]
        assert len(assets) == 1, r.data
        state["asset_color_bars"] = assets[0]["asset_id"]

    run.step("import_media (second fixture for multi-track edit)", s_import_second_fixture)

    def s_multitrack_transition():
        # A second video track, occupied concurrently with V1's beep_flash
        # clips (multi-track edit), with a cross-dissolve on its in-edge
        # (00-overview.md §2's "multi-track edit with cross-dissolves").
        v2 = expect_ok(
            client.tool("add_track", {"sequence_id": state["sequence_id"], "kind": "video", "name": "V2"})
        )
        track_v2 = v2.data["track_id"]
        clip = expect_ok(
            client.tool(
                "insert_clip",
                {
                    "track_id": track_v2,
                    "name": "color_bars",
                    "start_ticks": ticks(1),
                    "source": {"kind": "asset", "asset_id": state["asset_color_bars"]},
                    "duration_ticks": ticks(2),
                },
            )
        )
        clip_id = clip.data["clip_id"]
        expect_ok(
            client.tool(
                "set_transition",
                {
                    "clip_id": clip_id,
                    "edge": "in",
                    "transition": {"kind": "cross_dissolve", "duration_ticks": ticks(0.5)},
                },
            )
        )
        got = expect_ok(client.tool("get_clip", {"clip_id": clip_id})).data["clip"]
        assert got["transition_in"] is not None, got
        assert got["transition_in"]["kind"] == "cross_dissolve", got["transition_in"]
        assert got["transition_in"]["duration"] == ticks(0.5), got["transition_in"]
        state["track_v2"] = track_v2
        state["clip_v2"] = clip_id

    run.step("multi-track edit + cross-dissolve transition (set_transition)", s_multitrack_transition)

    def s_node_composition():
        # Per-clip node composition (08 §4): create_clip_composition seeds a
        # ClipIn -> Output graph; layer a SolidColor over it through a
        # screen-blend Merge (stand-in for "keyed overlay merge" — see
        # module docstring), then confirm get_graph reports it compiles.
        comp = expect_ok(client.tool("create_clip_composition", {"clip_id": state["clip_v2"]}))
        graph_id = comp.data["graph_id"]
        g0 = expect_ok(client.tool("get_graph", {"graph_id": graph_id})).data["graph"]
        nodes0 = g0["nodes"]

        def op_name(n):
            op = n["op"]
            return op if isinstance(op, str) else op["op"]

        clip_in_id = next(nid for nid, n in nodes0.items() if op_name(n) == "clip_in")
        output_id = next(nid for nid, n in nodes0.items() if op_name(n) == "output")

        solid = expect_ok(
            client.tool("add_graph_node", {"graph_id": graph_id, "op": {"op": "solid_color"}, "pos": [0, 120]})
        )
        solid_id = solid.data["node_id"]
        merge = expect_ok(
            client.tool(
                "add_graph_node", {"graph_id": graph_id, "op": {"op": "merge", "mode": "screen"}, "pos": [160, 60]}
            )
        )
        merge_id = merge.data["node_id"]

        # Replace the seeded ClipIn->Output edge (index 0, per
        # `NodeGraph::new_clip_composition`) with ClipIn/SolidColor -> Merge
        # -> Output.
        expect_ok(client.tool("remove_graph_edge", {"graph_id": graph_id, "edge_index": 0}))
        expect_ok(
            client.tool(
                "add_graph_edge",
                {"graph_id": graph_id, "from": {"node_id": clip_in_id, "port": 0}, "to": {"node_id": merge_id, "port": 0}},
            )
        )
        expect_ok(
            client.tool(
                "add_graph_edge",
                {"graph_id": graph_id, "from": {"node_id": solid_id, "port": 0}, "to": {"node_id": merge_id, "port": 1}},
            )
        )
        expect_ok(
            client.tool(
                "add_graph_edge",
                {"graph_id": graph_id, "from": {"node_id": merge_id, "port": 0}, "to": {"node_id": output_id, "port": 0}},
            )
        )

        g1 = expect_ok(client.tool("get_graph", {"graph_id": graph_id}))
        assert g1.data["compiles"] is True, g1.data
        assert g1.data["diagnostics"] == [], g1.data
        assert len(g1.data["graph"]["nodes"]) == 4, g1.data["graph"]["nodes"]
        state["graph_id"] = graph_id

    run.step("per-clip node composition (add_graph_node/edge, get_graph compiles)", s_node_composition)

    def s_full_grade():
        # A full grade stack (07 §1): CDL + curves ops, then a 3D LUT layered
        # on top (apply_lut) — `state["clip_v2"]` already carries a node
        # composition from the previous step; grade + composition are
        # independent so this also proves they compose.
        grade = {
            "ops": [
                {
                    "id": str(uuid.uuid4()),
                    "enabled": True,
                    "kind": "cdl",
                    "params": {
                        "base": {
                            "kind": "cdl",
                            "slope": [1.05, 1.0, 0.95],
                            "offset": [0.01, 0.0, -0.01],
                            "power": [1.0, 1.0, 1.0],
                            "sat": 1.1,
                        }
                    },
                },
                {
                    "id": str(uuid.uuid4()),
                    "enabled": True,
                    "kind": "curves",
                    "params": {
                        "base": {
                            "kind": "curves",
                            "master": [[0.0, 0.0], [0.5, 0.55], [1.0, 1.0]],
                            "red": [],
                            "green": [],
                            "blue": [],
                            "hue_vs_hue": [],
                            "hue_vs_sat": [],
                        }
                    },
                },
            ],
            "bypass": False,
        }
        expect_ok(client.tool("set_grade", {"clip_id": state["clip_v2"], "grade": grade}))
        got = expect_ok(client.tool("get_clip", {"clip_id": state["clip_v2"]})).data["clip"]
        assert got["grade"] is not None and len(got["grade"]["ops"]) == 2, got["grade"]

        # NOT a context-managed tempdir: the LUT asset is referenced by path
        # and re-read at render time (get_scopes below, export later) — it
        # must outlive this step, same "no cleanup required" convention as
        # `as1_arrange_cut.py`'s export-out tempfile.
        lut_path = identity_lut_path(tempfile.gettempdir())
        expect_ok(client.tool("apply_lut", {"clip_id": state["clip_v2"], "lut_path": lut_path, "intensity": 0.8}))
        got = expect_ok(client.tool("get_clip", {"clip_id": state["clip_v2"]})).data["clip"]
        assert len(got["grade"]["ops"]) == 3, got["grade"]
        assert got["grade"]["ops"][-1]["kind"] == "lut3d", got["grade"]["ops"][-1]

        if not engine_available(client):
            raise Skip("no GPU adapter (EngineUnavailable) — get_scopes needs the engine")
        sc = expect_ok(client.tool("get_scopes", {"clip_id": state["clip_v2"], "at_seconds": 0.5}))
        h = sc.data["histogram"]
        assert h["bins"] > 0, sc.data
        assert sum(h["luma"]) > 0, "scopes histogram is all-zero — no pixel data reached it"

    run.step("full grade (CDL + curves, apply_lut, get_scopes returns data)", s_full_grade)

    def s_mixer_touch():
        # Track-level mixer touch (09 §4/§6): a volume/pan move plus an
        # EQ + compressor unit added to V2's fx chain. `set_track_audio` /
        # `audio_fx` return no read-back data (no `get_track` tool exists
        # yet) — success of the calls themselves is the acceptance bar here.
        expect_ok(client.tool("set_track_audio", {"track_id": state["track_v2"], "volume_db": -3.0, "pan": 0.15}))
        expect_ok(client.tool("audio_fx", {"track_id": state["track_v2"], "op": "add", "kind": "eq"}))
        expect_ok(client.tool("audio_fx", {"track_id": state["track_v2"], "op": "add", "kind": "compressor"}))

    run.step("mixer touch (set_track_audio + audio_fx EQ/compressor)", s_mixer_touch)

    def s_export_master_and_web():
        # 00-overview.md §2's closing beat: "export master (AV1 high-quality)
        # + web H.264" — the two builtin presets by name (export_presets.rs).
        if not engine_available(client):
            raise Skip("no GPU adapter (EngineUnavailable) — export needs the engine")
        import contextlib
        out_dir = getattr(args, "out_dir", None)
        if out_dir:
            os.makedirs(out_dir, exist_ok=True)
            dir_ctx = contextlib.nullcontext(out_dir)
        else:
            dir_ctx = tempfile.TemporaryDirectory()
        with dir_ctx as tmp:
            for preset, ext, codec_name in (
                ("Master AV1 High", "mkv", "av1"),
                ("Web H.264", "mp4", "h264"),
            ):
                name = f"AS2_short_film_{codec_name}.{ext}" if out_dir else f"as2_export.{ext}"
                out_path = os.path.join(tmp, name)
                r = client.tool(
                    "export_sequence", {"sequence_id": state["sequence_id"], "out_path": out_path, "preset": preset}
                )
                if r.is_error:
                    if r.error_code == "FfmpegUnavailable":
                        raise Skip(f"{r.message} (set PHOTONIC_FFMPEG_DIR or install ffmpeg)")
                    raise AssertionError(f"export_sequence({preset}) failed: {r.message!r} (data={r.data!r})")
                job_id = r.data["job_id"]
                deadline = time.monotonic() + 180
                while True:
                    status = expect_ok(client.tool("get_job_status", {"job_id": job_id})).data["status"]
                    if status["state"] not in ("queued", "running"):
                        break
                    assert time.monotonic() < deadline, f"export({preset}) did not finish (last: {status})"
                    time.sleep(0.25)
                assert status["state"] == "done", f"export({preset}) ended in {status['state']!r}: {status}"
                assert Path(out_path).exists() and Path(out_path).stat().st_size > 0, out_path

                stream = ffprobe_video_stream(out_path)
                assert stream.get("codec_name") == codec_name, (
                    f"{preset}: expected codec_name={codec_name!r}, ffprobe reported {stream!r}"
                )
                assert int(stream.get("width", 0)) > 0 and int(stream.get("height", 0)) > 0, stream

    run.step("export AV1 master + web H.264 (poll jobs, ffprobe-verify each)", s_export_master_and_web)

    return run.summary()


if __name__ == "__main__":
    sys.exit(main())
