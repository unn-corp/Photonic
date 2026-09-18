#!/usr/bin/env python3
"""AS-1 "Social clip" acceptance story — arrange + cut slice.

Drives a running Photonic MCP server over HTTP JSON-RPC (the same `POST
/mcp` endpoint `crates/photonic-mcp/src/server.rs` exposes) through the
timeline-EDIT + P3-engine tool surface, exercising the story slice
available through P2 (arrange + cut) and P4 (export): create a sequence,
import fixture media, insert/split/move/trim/ripple clips on the timeline,
spot-check the result with `render_frame_at`, and export.

This intentionally stops short of AS-1's full story (00-overview.md §2 —
reframe, captions, grading are later phases); see
`docs/specs/video-editor/11-testing-phasing.md` §3.4/§6 for the
phase-by-phase story-slice plan this script tracks.

Usage
-----
    # 1. Launch a headless Photonic MCP server (no GUI window):
    cargo run -p photonic-app -- --headless

    # 2. Run this script against it (default: http://127.0.0.1:7842/mcp):
    python3 tools/as1_arrange_cut.py

    # Point at a different port/host, or an already-running GUI instance
    # (the GUI also serves the same MCP endpoint):
    python3 tools/as1_arrange_cut.py --url http://127.0.0.1:7842/mcp

Exit code is 0 iff every step passed or was skipped (skips are reserved for
environment gaps this script can't control — no GPU adapter, no ffmpeg — the
same "adapter-skip convention" `crates/photonic-mcp/src/handlers/video.rs`'s
own engine-backed tests use); any FAIL exits nonzero, so this is CI-runnable
once a GPU+ffmpeg runner is available (11 §3.4).

Stdlib-only by design — no `requests` dependency, so this runs anywhere a
plain `python3` does.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
import time
import urllib.error
import urllib.request
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


# ─── Story steps ────────────────────────────────────────────────────────────


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default=DEFAULT_URL, help=f"MCP endpoint (default: {DEFAULT_URL})")
    parser.add_argument(
        "--export-out",
        default=None,
        help="Export destination (default: a temp file, deleted-on-success not required)",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        default=os.environ.get("PHOTONIC_AS_STRICT") == "1",
        help="Fail (rather than skip) on an environment gap — no GPU adapter, "
        "no ffmpeg. Use in CI so a degraded runner can't report a green run "
        "that verified nothing. [env: PHOTONIC_AS_STRICT=1]",
    )
    args = parser.parse_args()

    client = Client(args.url)
    run = Runner(strict=args.strict)

    # Shared state threaded between steps via a plain dict — this script is a
    # linear story script, not a test framework; a dict keeps step functions
    # small closures over `client`/`state` without a class per step.
    state: dict[str, Any] = {}

    def s_connect():
        result = client.call(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "as1_arrange_cut.py", "version": "1"},
            },
        )
        assert result.get("serverInfo", {}).get("name") == "photonic", result

    run.step("connect (initialize)", s_connect)

    def s_create_sequence():
        r = expect_ok(
            client.tool(
                "create_sequence",
                {
                    "name": "AS-1 Sequence",
                    "frame_rate": {"num": 30, "den": 1},
                    "formats": [{"name": "16:9", "width": 320, "height": 180}],
                },
            )
        )
        state["sequence_id"] = r.data["sequence_id"]

    run.step("create_sequence", s_create_sequence)

    def s_import_media():
        paths = [str(FIXTURES_DIR / "color_bars.mp4"), str(FIXTURES_DIR / "counter.mp4")]
        for p in paths:
            assert Path(p).exists(), f"fixture missing: {p} — run tools/gen-test-fixtures.py"
        r = expect_ok(client.tool("import_media", {"paths": paths, "bin": "AS-1 Footage"}))
        assets = {a["path"]: a["asset_id"] for a in r.data["assets"]}
        assert len(assets) == 2, r.data
        state["asset_color_bars"] = assets[paths[0]]
        state["asset_counter"] = assets[paths[1]]

    run.step("import_media (fixtures)", s_import_media)

    def s_add_track():
        seq_id = state["sequence_id"]
        v1 = expect_ok(client.tool("add_track", {"sequence_id": seq_id, "kind": "video", "name": "V1"}))
        v2 = expect_ok(client.tool("add_track", {"sequence_id": seq_id, "kind": "video", "name": "V2"}))
        state["track_v1"] = v1.data["track_id"]
        state["track_v2"] = v2.data["track_id"]

    run.step("add_track (V1 + V2)", s_add_track)

    def s_insert_clips():
        # A[0,2s) color_bars, B[2s,5s) counter, C[5s,7s) color_bars — all on V1,
        # contiguous, no gaps.
        a = expect_ok(
            client.tool(
                "insert_clip",
                {
                    "track_id": state["track_v1"],
                    "name": "A",
                    "start_ticks": ticks(0),
                    "source": {"kind": "asset", "asset_id": state["asset_color_bars"]},
                    "duration_ticks": ticks(2),
                },
            )
        )
        b = expect_ok(
            client.tool(
                "insert_clip",
                {
                    "track_id": state["track_v1"],
                    "name": "B",
                    "start_ticks": ticks(2),
                    "source": {"kind": "asset", "asset_id": state["asset_counter"]},
                    "duration_ticks": ticks(3),
                },
            )
        )
        c = expect_ok(
            client.tool(
                "insert_clip",
                {
                    "track_id": state["track_v1"],
                    "name": "C",
                    "start_ticks": ticks(5),
                    "source": {"kind": "asset", "asset_id": state["asset_color_bars"]},
                    "duration_ticks": ticks(2),
                },
            )
        )
        state["clip_a"] = a.data["clip_id"]
        state["clip_b"] = b.data["clip_id"]
        state["clip_c"] = c.data["clip_id"]

    run.step("insert_clip (A, B, C on V1)", s_insert_clips)

    def s_ripple_edit():
        # Shrink B's out-point by 1s in one undo step; C (the only later clip
        # on V1) must shift left by exactly 1s to close the gap.
        expect_ok(
            client.tool(
                "ripple_edit",
                {"clip_id": state["clip_b"], "edge": "out", "delta_ticks": -ticks(1)},
            )
        )
        clips = {
            c["clip_id"]: c
            for c in expect_ok(
                client.tool("list_clips", {"sequence_id": state["sequence_id"]})
            ).data["clips"]
        }
        b, c = clips[state["clip_b"]], clips[state["clip_c"]]
        assert b["duration_ticks"] == ticks(2), b
        assert c["start_ticks"] == ticks(4), c

    run.step("ripple_edit (shrink B, C ripples left)", s_ripple_edit)

    def s_split_clip():
        # Split C (now [4s,6s)) at its 1s mark -> C=[4s,5s), D=[5s,6s).
        r = expect_ok(
            client.tool("split_clip", {"clip_id": state["clip_c"], "at_ticks": ticks(5)})
        )
        state["clip_d"] = r.data["new_clip_id"]

    run.step("split_clip (C -> C, D)", s_split_clip)

    def s_move_clip():
        # Cross-track move: D onto V2 at the same start tick (V2 is empty,
        # so this cannot overlap).
        expect_ok(
            client.tool(
                "move_clip",
                {
                    "clip_id": state["clip_d"],
                    "new_start_ticks": ticks(5),
                    "new_track_id": state["track_v2"],
                },
            )
        )
        clips = {
            c["clip_id"]: c
            for c in expect_ok(
                client.tool("list_clips", {"sequence_id": state["sequence_id"]})
            ).data["clips"]
        }
        d = clips[state["clip_d"]]
        assert d["track_id"] == state["track_v2"], d

    run.step("move_clip (D -> V2, cross-track)", s_move_clip)

    def s_trim_clip():
        # Trim A's out edge from 2s to 1.5s (opens a 0.5s gap before B — gaps
        # are legal, only overlaps aren't).
        expect_ok(
            client.tool("trim_clip", {"clip_id": state["clip_a"], "edge": "out", "new_ticks": ticks(1.5)})
        )
        clips = {
            c["clip_id"]: c
            for c in expect_ok(
                client.tool("list_clips", {"sequence_id": state["sequence_id"]})
            ).data["clips"]
        }
        a = clips[state["clip_a"]]
        assert a["duration_ticks"] == ticks(1.5), a

    run.step("trim_clip (shorten A's out edge)", s_trim_clip)

    def s_render_frame_at():
        if not engine_available(client):
            raise Skip("no GPU adapter (EngineUnavailable) — engine-backed tools need one")
        for label, at in (("inside B", 3.0), ("inside C", 4.5)):
            r = expect_ok(
                client.tool(
                    "render_frame_at",
                    {
                        "sequence_id": state["sequence_id"],
                        "at_seconds": at,
                        "quality": "preview",
                    },
                )
            )
            has_image = any(c.get("type") == "image" for c in r.raw.get("content", []))
            assert has_image, f"render_frame_at ({label}) returned no image content: {r}"

    run.step("render_frame_at (spot-check B and C)", s_render_frame_at)

    def s_export_sequence():
        if not engine_available(client):
            raise Skip("no GPU adapter (EngineUnavailable) — export needs the engine")
        out_path = args.export_out or str(
            Path(tempfile.gettempdir()) / f"photonic_as1_export_{os.getpid()}.mp4"
        )
        r = client.tool("export_sequence", {"sequence_id": state["sequence_id"], "out_path": out_path})
        if r.is_error:
            if r.error_code == "FfmpegUnavailable":
                raise Skip(f"{r.message} (set PHOTONIC_FFMPEG_DIR or install ffmpeg)")
            raise AssertionError(f"export_sequence failed: {r.message!r} (data={r.data!r})")
        job_id = r.data["job_id"]

        deadline = time.monotonic() + 120
        while True:
            status = expect_ok(client.tool("get_job_status", {"job_id": job_id})).data["status"]
            state_name = status["state"]
            if state_name not in ("queued", "running"):
                break
            assert time.monotonic() < deadline, f"export did not finish in time (last: {status})"
            time.sleep(0.25)

        assert state_name == "done", f"export job ended in state {state_name!r}: {status}"
        assert Path(out_path).exists() and Path(out_path).stat().st_size > 0, out_path
        state["export_path"] = out_path

    run.step("export_sequence (poll to done)", s_export_sequence)

    return run.summary()


if __name__ == "__main__":
    sys.exit(main())
