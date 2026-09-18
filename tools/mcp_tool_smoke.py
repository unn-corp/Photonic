#!/usr/bin/env python3
"""Full MCP catalog registration smoke.

Lists every tool via tools/list (params.full=true), then tools/call each name
with empty arguments. Pass = no "Unknown tool" responses.

Validation / domain errors are expected and count as REGISTERED.
Does not prove every tool completes a successful media workflow.

Usage:
  python3 tools/mcp_tool_smoke.py --url http://127.0.0.1:17842/mcp --token SECRET
"""

from __future__ import annotations

import argparse
import json
import sys
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor, TimeoutError as FuturesTimeout
from typing import Any


def rpc(
    url: str,
    token: str | None,
    method: str,
    params: dict[str, Any] | None = None,
    *,
    timeout: float = 30.0,
    req_id: int = 1,
    mcp_name: str | None = None,
) -> dict[str, Any]:
    params_obj: dict[str, Any] = dict(params) if params else {}
    # Protocol 2026-07-28: _meta lives under params (not top-level JSON-RPC).
    params_obj.setdefault(
        "_meta",
        {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": {
                "name": "mcp_tool_smoke",
                "version": "1.0.0",
            },
        },
    )
    body: dict[str, Any] = {
        "jsonrpc": "2.0",
        "id": req_id,
        "method": method,
        "params": params_obj,
    }
    data = json.dumps(body).encode("utf-8")
    headers = {
        "Content-Type": "application/json",
        "Mcp-Method": method,
    }
    if mcp_name:
        headers["Mcp-Name"] = mcp_name
    if token:
        headers["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(url, data=data, headers=headers, method="POST")
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        raw = resp.read().decode("utf-8")
    return json.loads(raw)


def extract_error_text(resp: dict[str, Any]) -> str:
    if "error" in resp and resp["error"] is not None:
        err = resp["error"]
        if isinstance(err, dict):
            return str(err.get("message") or err)
        return str(err)
    result = resp.get("result")
    if not isinstance(result, dict):
        return ""
    content = result.get("content")
    if content is None and isinstance(result.get("result"), dict):
        content = result["result"].get("content")
    if not isinstance(content, list):
        if result.get("isError"):
            return json.dumps(result)[:500]
        return ""
    texts = []
    for item in content:
        if isinstance(item, dict) and item.get("type") == "text":
            texts.append(str(item.get("text") or ""))
    return "\n".join(texts)


def classify(resp: dict[str, Any], name: str) -> str:
    text = extract_error_text(resp)
    if f"Unknown tool: {name}" in text:
        return "UNKNOWN"
    if "Unknown tool:" in text and name in text:
        return "UNKNOWN"
    err = resp.get("error")
    if isinstance(err, dict):
        msg = str(err.get("message") or "")
        if "Unknown tool" in msg and name in msg:
            return "UNKNOWN"
        if "Method not found" in msg:
            return "UNKNOWN"
    return "REGISTERED"


def call_one(
    url: str,
    token: str | None,
    name: str,
    timeout: float,
    req_id: int,
) -> tuple[str, str, str]:
    """Returns (name, class, detail)."""
    try:
        resp = rpc(
            url,
            token,
            "tools/call",
            {"name": name, "arguments": {}},
            timeout=timeout,
            req_id=req_id,
            mcp_name=name,
        )
        kind = classify(resp, name)
        detail = extract_error_text(resp)[:200] if kind != "REGISTERED" else ""
        if kind == "REGISTERED" and resp.get("error"):
            detail = str(resp.get("error"))[:200]
        return name, kind, detail
    except FuturesTimeout:
        return name, "TIMEOUT", "future timeout"
    except TimeoutError:
        return name, "TIMEOUT", "socket timeout"
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", errors="replace")[:300]
        if "Unknown tool" in body:
            return name, "UNKNOWN", body
        return name, "REGISTERED", f"HTTP {e.code}: {body[:120]}"
    except Exception as e:  # noqa: BLE001 — smoke script
        return name, "ERROR", str(e)[:200]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--url", required=True, help="MCP endpoint, e.g. http://127.0.0.1:17842/mcp")
    ap.add_argument("--token", default=None, help="Bearer token (optional if server has no secret)")
    ap.add_argument("--timeout", type=float, default=8.0, help="Per-tool call timeout seconds")
    ap.add_argument("--workers", type=int, default=4, help="Parallel tools/call workers")
    ap.add_argument("--limit", type=int, default=0, help="Only first N tools (0 = all)")
    ap.add_argument(
        "--protocol-only",
        action="store_true",
        help="Only run discover + compact/full list + search/execute, no full catalog",
    )
    args = ap.parse_args()

    print(f"[smoke] url={args.url}")

    disc = rpc(args.url, args.token, "server/discover", {}, timeout=15, req_id=1)
    if "error" in disc and disc["error"]:
        print(f"[smoke] FAIL discover: {disc['error']}", file=sys.stderr)
        return 2
    print("[smoke] discover OK")

    compact = rpc(args.url, args.token, "tools/list", {}, timeout=30, req_id=2)
    compact_tools = None
    r = compact.get("result")
    if isinstance(r, dict):
        compact_tools = r.get("tools")
        if compact_tools is None and isinstance(r.get("result"), dict):
            compact_tools = r["result"].get("tools")
    if not isinstance(compact_tools, list):
        print(
            f"[smoke] FAIL tools/list compact shape: {json.dumps(compact)[:400]}",
            file=sys.stderr,
        )
        return 2
    cnames = [t.get("name") for t in compact_tools if isinstance(t, dict)]
    print(f"[smoke] compact tools/list: {len(cnames)} tools")
    for need in ("search_actions", "execute_action"):
        if need not in cnames:
            print(f"[smoke] WARN compact missing {need}: {cnames}", file=sys.stderr)

    full = rpc(args.url, args.token, "tools/list", {"full": True}, timeout=60, req_id=3)
    full_tools = None
    r = full.get("result")
    if isinstance(r, dict):
        full_tools = r.get("tools")
        if full_tools is None and isinstance(r.get("result"), dict):
            full_tools = r["result"].get("tools")
    if not isinstance(full_tools, list):
        print(
            f"[smoke] FAIL tools/list full shape: {json.dumps(full)[:400]}",
            file=sys.stderr,
        )
        return 2
    names = [t["name"] for t in full_tools if isinstance(t, dict) and "name" in t]
    print(f"[smoke] full tools/list: {len(names)} tools")

    search = rpc(
        args.url,
        args.token,
        "tools/call",
        {"name": "search_actions", "arguments": {"query": "document", "limit": 5}},
        timeout=15,
        req_id=4,
        mcp_name="search_actions",
    )
    if classify(search, "search_actions") == "UNKNOWN":
        print(f"[smoke] FAIL search_actions unknown: {search}", file=sys.stderr)
        return 2
    print("[smoke] search_actions OK")

    exe = rpc(
        args.url,
        args.token,
        "tools/call",
        {
            "name": "execute_action",
            "arguments": {"name": "get_document_info", "arguments": {}},
        },
        timeout=15,
        req_id=5,
        mcp_name="execute_action",
    )
    text = extract_error_text(exe)
    if "Unknown tool: execute_action" in text:
        print(f"[smoke] FAIL execute_action: {exe}", file=sys.stderr)
        return 2
    print("[smoke] execute_action OK")

    if args.protocol_only:
        print("[smoke] protocol-only done — PASS")
        return 0

    if args.limit and args.limit > 0:
        names = names[: args.limit]
        print(f"[smoke] limited to first {len(names)} tools")

    registered = 0
    unknown: list[str] = []
    timeouts: list[str] = []
    errors: list[tuple[str, str]] = []
    samples: list[tuple[str, str]] = []

    def work(item: tuple[int, str]) -> tuple[str, str, str]:
        i, name = item
        return call_one(args.url, args.token, name, args.timeout, 1000 + i)

    with ThreadPoolExecutor(max_workers=max(1, args.workers)) as pool:
        futs = [pool.submit(work, (i, n)) for i, n in enumerate(names)]
        for fut in futs:
            try:
                name, kind, detail = fut.result(timeout=args.timeout + 5)
            except FuturesTimeout:
                name, kind, detail = "?", "TIMEOUT", "worker"
            if kind == "REGISTERED":
                registered += 1
            elif kind == "UNKNOWN":
                unknown.append(name)
            elif kind == "TIMEOUT":
                timeouts.append(name)
            else:
                errors.append((name, detail))
            if detail and len(samples) < 8 and kind != "REGISTERED":
                samples.append((name, f"{kind}: {detail}"))

    total = len(names)
    print()
    print("=== Catalog registration smoke ===")
    print(f"total:       {total}")
    print(f"registered:  {registered}")
    print(f"unknown:     {len(unknown)}")
    print(f"timeout:     {len(timeouts)}")
    print(f"error:       {len(errors)}")
    if unknown:
        print("UNKNOWN tools:", ", ".join(unknown[:40]))
    if timeouts:
        print("TIMEOUT tools (first 20):", ", ".join(timeouts[:20]))
    if samples:
        print("sample non-registered:")
        for n, d in samples:
            print(f"  - {n}: {d[:160]}")

    if unknown:
        print("[smoke] FAIL: unknown tools present")
        return 1
    if timeouts:
        print(
            f"[smoke] WARN: {len(timeouts)} timeouts "
            "(not treated as unknown; investigate GPU/export hangs)"
        )
    print("[smoke] PASS: unknown == 0")
    return 0


if __name__ == "__main__":
    sys.exit(main())
