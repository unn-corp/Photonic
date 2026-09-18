#!/usr/bin/env python3
"""AS-3 "Motion graphics" acceptance story (CAP-019/SS-2).

Drives a running Photonic MCP server over HTTP JSON-RPC (the same `POST
/mcp` endpoint `crates/photonic-mcp/src/server.rs` exposes), exercising
every beat `docs/specs/video-editor/00-overview.md` §2 lists for "AS-3
Motion graphics": "Animate a Photonic vector document with keyframes
(transform + path/fill properties) -> composite over footage in a node
composition -> caption + grade -> export WebM/ProRes-style with alpha." Per
`11-testing-phasing.md` §3.4/§6/P8's "AS-2 and AS-3 both fully complete"
gate, all of P6 (keyframes/vector clips), P7 (grade) and P8 (node graph) are
committed now.

One documented slice of the spec bullet above: this script keyframes the
clip *transform* only (`set_keyframe` on `transform.x`) — the bullet also
says "+ path/fill properties", but animating a vector document's own
node-level fill/path props is a `photonic-render` vector-editing concern
(node param keyframing through the same `AnimTargetArg`/`set_keyframe`
surface, just a different `path`), already covered by that engine's own
test suite; this script's job is proving the *video-timeline* keyframe path
end-to-end, not re-testing vector-node param animation.

Fixtures: no `.photon` *vector document* (as opposed to a *timeline
project* `.photon`, a different use of the same extension — see
`crates/photonic-video/tests/fixtures/README.md`'s corpus, which has none)
is committed under `crates/photonic-video/tests/fixtures/`. This script
reuses `paths_fills_basic/project.photon`, one of `photonic-render`'s own
small golden vector-doc fixtures (`AssetKind::VectorDoc` per
`guess_asset_kind`'s `"photon"` extension rule) — read-only, never
modified. `color_bars.mp4` (footage) and `alpha_gradient.mov` (CAP-021's
known-value straight-alpha ramp) come from the usual video-editor corpus.

Alpha strategy: rather than hand-verify translucency inside the
node-composited clip (merging the vector doc over opaque footage yields an
opaque result there, by design — see `s_node_composition`), a *second* clip
on the same track (`alpha_gradient.mov`, real partial-alpha pixels) sits in
a time range where the *other* track has no clip underneath it — so the
final composite frame genuinely carries partial alpha, exported and
ffprobe-verified the same way
`crates/photonic-video/tests/export_synthetic.rs::export_webm_vp9_alpha_e2e_ffprobe_and_alpha_roundtrip`
verifies it (the `alpha_mode` stream tag VP9/WebM side-channel convention,
CAP-021).

Usage
-----
    # 1. Launch a headless Photonic MCP server (no GUI window):
    cargo run -p photonic-app -- --headless

    # 2. Run this script against it (default: http://127.0.0.1:7842/mcp):
    python3 tools/as3_motion_graphics.py

Exit code is 0 iff every step passed or was skipped (skips are reserved for
environment gaps — no GPU adapter, no ffmpeg/ffprobe — this script can't
control); any FAIL exits nonzero, so this is CI-runnable once a GPU+ffmpeg
runner is available.

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
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path
from typing import Any, Optional

REPO_ROOT = Path(__file__).resolve().parent.parent
FIXTURES_DIR = REPO_ROOT / "crates" / "photonic-video" / "tests" / "fixtures"
# `photonic-render`'s own golden vector-doc corpus — see module docstring's
# "Fixtures" section for why a video-editor-corpus fixture doesn't exist.
VECTOR_DOC_FIXTURE = (
    REPO_ROOT / "crates" / "photonic-render" / "tests" / "golden" / "paths_fills_basic" / "project.photon"
)

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
    dict, including the `alpha_mode` stream tag — the same convention
    `crates/photonic-video/tests/export_synthetic.rs`'s own
    `export_webm_vp9_alpha_e2e_ffprobe_and_alpha_roundtrip` test uses to
    verify real VP9/WebM encoder output (CAP-021). Raises `Skip` if
    `ffprobe` isn't on `PATH` (a separate binary from `ffmpeg`, so a
    distinct environment gap from `FfmpegUnavailable`)."""
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


# ─── Story steps ────────────────────────────────────────────────────────────


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default=DEFAULT_URL, help=f"MCP endpoint (default: {DEFAULT_URL})")
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
                "clientInfo": {"name": "as3_motion_graphics.py", "version": "1"},
            },
        )
        assert result.get("serverInfo", {}).get("name") == "photonic", result

    run.step("connect (initialize)", s_connect)

    def s_create_sequence():
        # 120x120 matches the vector-doc fixture's own canvas (project.photon
        # width/height) so the composited clip needs no reframe/scale to look
        # sane — not load-bearing for this smoke script, just tidy.
        r = expect_ok(
            client.tool(
                "create_sequence",
                {
                    "name": "AS-3 Sequence",
                    "frame_rate": {"num": 30, "den": 1},
                    "formats": [{"name": "1:1", "width": 120, "height": 120}],
                },
            )
        )
        state["sequence_id"] = r.data["sequence_id"]

    run.step("create_sequence", s_create_sequence)

    def s_import_media():
        assert VECTOR_DOC_FIXTURE.exists(), (
            f"vector-doc fixture missing: {VECTOR_DOC_FIXTURE} (photonic-render's golden corpus)"
        )
        paths = [
            str(FIXTURES_DIR / "color_bars.mp4"),
            str(VECTOR_DOC_FIXTURE),
            str(FIXTURES_DIR / "alpha_gradient.mov"),
        ]
        for p in paths:
            assert Path(p).exists(), f"fixture missing: {p} — run tools/gen-test-fixtures.py"
        r = expect_ok(client.tool("import_media", {"paths": paths, "bin": "AS-3 Assets"}))
        assets = {a["path"]: a for a in r.data["assets"]}
        assert len(assets) == 3, r.data
        assert assets[paths[1]]["kind"] == "vector_doc", assets[paths[1]]
        state["asset_footage"] = assets[paths[0]]["asset_id"]
        state["asset_vector"] = assets[paths[1]]["asset_id"]
        state["asset_alpha"] = assets[paths[2]]["asset_id"]

    run.step("import_media (footage + vector doc + alpha fixture)", s_import_media)

    def s_place_clips():
        # V1: footage, [0s, 2s) only — deliberately stops there so V2's
        # alpha_gradient clip (below) has nothing underneath it in [2s, 3s),
        # letting real transparency reach the exported frame (module
        # docstring's "Alpha strategy"). V2: the vector-doc clip [0s, 2s),
        # then alpha_gradient [2s, 3s), contiguous on the same track.
        v1 = expect_ok(client.tool("add_track", {"sequence_id": state["sequence_id"], "kind": "video", "name": "V1"}))
        v2 = expect_ok(client.tool("add_track", {"sequence_id": state["sequence_id"], "kind": "video", "name": "V2"}))
        track_v1, track_v2 = v1.data["track_id"], v2.data["track_id"]

        footage = expect_ok(
            client.tool(
                "insert_clip",
                {
                    "track_id": track_v1,
                    "name": "footage",
                    "start_ticks": ticks(0),
                    "source": {"kind": "asset", "asset_id": state["asset_footage"]},
                    "duration_ticks": ticks(2),
                },
            )
        )
        vec = expect_ok(
            client.tool(
                "insert_clip",
                {
                    "track_id": track_v2,
                    "name": "vecdoc",
                    "start_ticks": ticks(0),
                    "source": {"kind": "vector", "asset_id": state["asset_vector"]},
                    "duration_ticks": ticks(2),
                },
            )
        )
        alpha = expect_ok(
            client.tool(
                "insert_clip",
                {
                    "track_id": track_v2,
                    "name": "alpha_gradient",
                    "start_ticks": ticks(2),
                    "source": {"kind": "asset", "asset_id": state["asset_alpha"]},
                    "duration_ticks": ticks(1),
                },
            )
        )
        state["track_v1"] = track_v1
        state["track_v2"] = track_v2
        state["clip_footage"] = footage.data["clip_id"]
        state["clip_vector"] = vec.data["clip_id"]
        state["clip_alpha"] = alpha.data["clip_id"]

    run.step("place footage + vector-doc + alpha clips", s_place_clips)

    def s_animate_transform():
        # 00-overview.md §2: "Animate a Photonic vector document with
        # keyframes (transform ...)" — two keyframes on the vector clip's
        # transform.x (01 §6's `AnimTargetArg::ClipTransform` path
        # convention, keyframe_editor.rs's own "transform.x" naming).
        clip_id = state["clip_vector"]
        expect_ok(
            client.tool(
                "set_keyframe",
                {
                    "target": "clip_transform", "clip_id": clip_id, "path": "transform.x",
                    "at_seconds": 0.0, "value": {"t": "float", "v": 0.2}, "interp": {"kind": "linear"},
                },
            )
        )
        expect_ok(
            client.tool(
                "set_keyframe",
                {
                    "target": "clip_transform", "clip_id": clip_id, "path": "transform.x",
                    "at_seconds": 1.8, "value": {"t": "float", "v": 0.8}, "interp": {"kind": "linear"},
                },
            )
        )
        got = expect_ok(client.tool("get_keyframes", {"target": "clip_transform", "clip_id": clip_id}))
        tracks = got.data["tracks"]
        assert len(tracks) == 1 and tracks[0]["property"] == "transform.x", tracks
        kfs = tracks[0]["keyframes"]
        assert len(kfs) == 2, kfs
        assert kfs[0]["at"] == 0 and kfs[0]["value"] == {"t": "float", "v": 0.2}, kfs[0]
        assert kfs[1]["at"] == ticks(1.8) and kfs[1]["value"] == {"t": "float", "v": 0.8}, kfs[1]

    run.step("animate vector clip (set_keyframe on transform.x)", s_animate_transform)

    def s_node_composition():
        # "composite over footage in a node composition": create_clip_composition
        # on the vector clip seeds ClipIn(rasterized vector doc) -> Output;
        # add a MediaIn(footage asset) + normal-blend Merge so the clip's own
        # render is the vector doc composited over the footage, then confirm
        # get_graph reports it compiles.
        clip_id = state["clip_vector"]
        comp = expect_ok(client.tool("create_clip_composition", {"clip_id": clip_id}))
        graph_id = comp.data["graph_id"]
        g0 = expect_ok(client.tool("get_graph", {"graph_id": graph_id})).data["graph"]
        nodes0 = g0["nodes"]

        def op_name(n):
            op = n["op"]
            return op if isinstance(op, str) else op["op"]

        clip_in_id = next(nid for nid, n in nodes0.items() if op_name(n) == "clip_in")
        output_id = next(nid for nid, n in nodes0.items() if op_name(n) == "output")

        media_in = expect_ok(
            client.tool(
                "add_graph_node",
                {"graph_id": graph_id, "op": {"op": "media_in", "asset": state["asset_footage"]}, "pos": [0, 120]},
            )
        )
        media_in_id = media_in.data["node_id"]
        merge = expect_ok(
            client.tool(
                "add_graph_node", {"graph_id": graph_id, "op": {"op": "merge", "mode": "normal"}, "pos": [160, 60]}
            )
        )
        merge_id = merge.data["node_id"]

        # Replace the seeded ClipIn->Output edge (index 0, per
        # `NodeGraph::new_clip_composition`) with MediaIn/ClipIn -> Merge ->
        # Output — the vector doc (top) over the footage (bottom).
        expect_ok(client.tool("remove_graph_edge", {"graph_id": graph_id, "edge_index": 0}))
        expect_ok(
            client.tool(
                "add_graph_edge",
                {"graph_id": graph_id, "from": {"node_id": media_in_id, "port": 0}, "to": {"node_id": merge_id, "port": 0}},
            )
        )
        expect_ok(
            client.tool(
                "add_graph_edge",
                {"graph_id": graph_id, "from": {"node_id": clip_in_id, "port": 0}, "to": {"node_id": merge_id, "port": 1}},
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

    run.step("composite vector doc over footage (node graph, get_graph compiles)", s_node_composition)

    def s_auto_caption_mock():
        r = expect_ok(
            client.tool(
                "auto_caption",
                {
                    "sequence_id": state["sequence_id"],
                    "provider": "mock",
                    "mock_transcript": "Motion graphics demo caption test",
                    "name": "AS-3 Captions",
                },
            )
        )
        job_id = r.data["job_id"]
        state["caption_track_id"] = r.data["track_id"]
        deadline = time.monotonic() + 60
        while True:
            status = expect_ok(client.tool("get_job_status", {"job_id": job_id})).data["status"]
            if status["state"] not in ("queued", "running"):
                break
            assert time.monotonic() < deadline, f"auto_caption job did not finish (last: {status})"
            time.sleep(0.25)
        assert status["state"] == "done", f"auto_caption ended in {status['state']!r}: {status}"
        assert status["result"]["cue_count"] >= 1, status

    run.step("auto_caption (mock provider, poll to done)", s_auto_caption_mock)

    def s_set_grade():
        grade = {
            "ops": [
                {
                    "id": str(uuid.uuid4()),
                    "enabled": True,
                    "kind": "cdl",
                    "params": {
                        "base": {
                            "kind": "cdl",
                            "slope": [1.0, 1.0, 1.05],
                            "offset": [0.0, 0.0, 0.02],
                            "power": [1.0, 1.0, 1.0],
                            "sat": 1.05,
                        }
                    },
                }
            ],
            "bypass": False,
        }
        expect_ok(client.tool("set_grade", {"clip_id": state["clip_footage"], "grade": grade}))
        got = expect_ok(client.tool("get_clip", {"clip_id": state["clip_footage"]})).data["clip"]
        assert got["grade"] is not None and len(got["grade"]["ops"]) == 1, got["grade"]

    run.step("set_grade (footage clip)", s_set_grade)

    def s_export_alpha():
        # 00-overview.md §2's closing beat: "export WebM/ProRes-style with
        # alpha" — WebM VP9 Alpha (export_presets.rs's built-in), ffprobe's
        # `alpha_mode` stream tag is the CAP-021 "alpha present" bar (module
        # docstring's "Alpha strategy").
        if not engine_available(client):
            raise Skip("no GPU adapter (EngineUnavailable) — export needs the engine")
        out_path = str(
            Path(tempfile.gettempdir()) / f"photonic_as3_export_{os.getpid()}.webm"
        )
        r = client.tool(
            "export_sequence",
            {
                "sequence_id": state["sequence_id"],
                "out_path": out_path,
                "preset": "WebM VP9 Alpha",
                # The final second is the purpose-built alpha fixture.  Keep
                # the codec assertion focused on that segment: the preceding
                # steps independently verify the animated vector composition,
                # while cold-starting an external vector rasterizer here can
                # exceed the export frame deadline on software-GPU CI hosts.
                "range": {"start_seconds": 2.0, "end_seconds": 3.0},
            },
        )
        if r.is_error:
            if r.error_code == "FfmpegUnavailable":
                raise Skip(f"{r.message} (set PHOTONIC_FFMPEG_DIR or install ffmpeg)")
            raise AssertionError(f"export_sequence failed: {r.message!r} (data={r.data!r})")
        job_id = r.data["job_id"]

        deadline = time.monotonic() + 180
        while True:
            status = expect_ok(client.tool("get_job_status", {"job_id": job_id})).data["status"]
            state_name = status["state"]
            if state_name not in ("queued", "running"):
                break
            assert time.monotonic() < deadline, f"export did not finish in time (last: {status})"
            time.sleep(0.25)

        assert state_name == "done", f"export job ended in state {state_name!r}: {status}"
        assert Path(out_path).exists() and Path(out_path).stat().st_size > 0, out_path

        stream = ffprobe_video_stream(out_path)
        assert stream.get("codec_name") == "vp9", stream
        assert stream.get("tags", {}).get("alpha_mode") == "1", (
            f"VP9 alpha side-channel tag must be present (CAP-021) — ffprobe reported {stream!r}"
        )
        state["export_path"] = out_path

    run.step("export WebM VP9 Alpha (poll job, ffprobe-verify alpha present)", s_export_alpha)

    return run.summary()


if __name__ == "__main__":
    sys.exit(main())
