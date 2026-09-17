#!/usr/bin/env bash
# run-acceptance-stories.sh — the CAP-019 acceptance-story gate (QA-1).
#
# Single entry point for the three MCP acceptance-story scripts that
# `docs/specs/video-editor/29-qa-spec.md` §3 defines (AS-1 "Social clip",
# AS-2 "Short film", AS-3 "Motion graphics"). Each script drives a *running*
# Photonic MCP server over HTTP JSON-RPC, so before this file existed there
# was no way to run them except by hand-launching a server in one terminal
# and the scripts in another — which is why they were never gated on
# anything. This script owns that plumbing:
#
#   1. builds `photonic-app` (skip with --no-build / --bin),
#   2. for each story: starts its OWN headless MCP server on its own free
#      port, waits for `/mcp` to answer `initialize`, runs the story,
#      tears the server down (each story gets a clean document — a story
#      must never inherit another story's timeline state),
#   3. prints a per-story PASS/FAIL table and exits non-zero if any story
#      failed, so CI can gate on it directly.
#
# Usage
# -----
#   scripts/run-acceptance-stories.sh                  # all three stories
#   scripts/run-acceptance-stories.sh as2 as3          # a subset
#   scripts/run-acceptance-stories.sh --strict         # CI mode (see below)
#   scripts/run-acceptance-stories.sh --release
#   scripts/run-acceptance-stories.sh --bin target/release/photonic --no-build
#   scripts/run-acceptance-stories.sh --out-dir artifacts/as2  # keep AS-2's exports
#
# CI mode (--strict, or PHOTONIC_AS_STRICT=1)
# -------------------------------------------
# The story scripts SKIP (not fail) every engine-backed step when the
# machine has no GPU adapter, and every encode step when ffmpeg/ffprobe is
# missing. That is right for a laptop; it is a trap for CI, where a runner
# that silently lost its GPU would skip everything that renders or encodes
# and still exit 0. `--strict` promotes those skips to failures. A CI job
# that intends to gate on this harness MUST pass `--strict`; a CI job that
# only wants smoke coverage of the pure-timeline verbs may omit it, but then
# it is NOT the CAP-019 gate.
#
# Requirements: cargo + a Rust toolchain, python3 (stdlib only), and — for a
# meaningful (`--strict`) run — a GPU adapter wgpu can open plus `ffmpeg`
# and `ffprobe` on PATH. Test fixtures come from the committed corpus under
# `crates/photonic-video/tests/fixtures/` (regenerate with
# `python3 tools/gen-test-fixtures.py`) and one vector-doc fixture from
# `crates/photonic-render/tests/golden/`.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

ALL_STORIES=(as1 as2 as3)
declare -A STORY_SCRIPT=(
  [as1]="tools/as1_arrange_cut.py"
  [as2]="tools/as2_proxy_edit.py"
  [as3]="tools/as3_motion_graphics.py"
)
declare -A STORY_TITLE=(
  [as1]="AS-1 Social clip (arrange + cut)"
  [as2]="AS-2 Short film (proxy, grade, node comp, mixer, dual export)"
  [as3]="AS-3 Motion graphics (vector keyframes, composite, alpha export)"
)

STRICT="${PHOTONIC_AS_STRICT:-0}"
BUILD=1
PROFILE=dev
BIN="${PHOTONIC_BIN:-}"
OUT_DIR=""
LOG_DIR="${PHOTONIC_AS_LOG_DIR:-}"
SERVER_TIMEOUT="${PHOTONIC_AS_SERVER_TIMEOUT:-180}"
STORIES=()

die() { printf 'run-acceptance-stories: %s\n' "$*" >&2; exit 2; }

usage() { sed -n '2,/^set -uo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//; $d'; }

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --strict) STRICT=1 ;;
    --no-build) BUILD=0 ;;
    --release) PROFILE=release ;;
    --bin) BIN="${2:-}"; [ -n "$BIN" ] || die "--bin needs a path"; shift ;;
    --out-dir) OUT_DIR="${2:-}"; [ -n "$OUT_DIR" ] || die "--out-dir needs a path"; shift ;;
    --log-dir) LOG_DIR="${2:-}"; [ -n "$LOG_DIR" ] || die "--log-dir needs a path"; shift ;;
    as1|as2|as3) STORIES+=("$1") ;;
    *) die "unknown argument '$1' (try --help)" ;;
  esac
  shift
done

[ ${#STORIES[@]} -gt 0 ] || STORIES=("${ALL_STORIES[@]}")

command -v python3 >/dev/null 2>&1 || die "python3 not found — the story scripts are Python"

if [ -z "$LOG_DIR" ]; then
  LOG_DIR="$(mktemp -d -t photonic-acceptance-XXXXXX)"
fi
mkdir -p "$LOG_DIR" || die "cannot create log dir $LOG_DIR"

# ── Build ────────────────────────────────────────────────────────────────────
if [ -z "$BIN" ]; then
  BIN="${REPO_ROOT}/target/${PROFILE/dev/debug}/photonic"
  if [ "$BUILD" -eq 1 ]; then
    command -v cargo >/dev/null 2>&1 || die "cargo not found (pass --bin/--no-build to use a prebuilt binary)"
    echo "== building photonic-app (${PROFILE}) =="
    build_args=(build -p photonic-app)
    [ "$PROFILE" = release ] && build_args+=(--release)
    ( cd "$REPO_ROOT" && cargo "${build_args[@]}" ) || die "cargo build failed"
  fi
fi
[ -x "$BIN" ] || die "MCP server binary not found or not executable: $BIN"

# ── Server lifecycle ─────────────────────────────────────────────────────────
SERVER_PID=""
cleanup() {
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  SERVER_PID=""
}
trap 'cleanup; exit 130' INT TERM
trap cleanup EXIT

# A free ephemeral port, asked of the kernel rather than guessed, so this can
# run alongside a developer's own instance on the default 7842 and alongside a
# second copy of itself on the same machine.
free_port() {
  python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}

# Poll POST /mcp with a real `initialize` until the server answers, failing
# fast if the server process dies first (otherwise a crash-on-startup shows up
# only as a timeout, minutes later).
wait_for_server() {
  local port="$1" pid="$2" deadline=$((SECONDS + SERVER_TIMEOUT))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if ! kill -0 "$pid" 2>/dev/null; then return 1; fi
    if python3 - "$port" <<'PY' >/dev/null 2>&1
import json, os, sys, urllib.request
url = f"http://127.0.0.1:{sys.argv[1]}/mcp"
body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                              "clientInfo": {"name": "run-acceptance-stories.sh", "version": "1"}}}).encode()
req = urllib.request.Request(url, data=body, headers={
    "Content-Type": "application/json",
    "x-mcp-secret": os.environ["PHOTONIC_MCP_SECRET"],
}, method="POST")
with urllib.request.urlopen(req, timeout=5) as r:
    assert json.loads(r.read())["result"]["serverInfo"]["name"] == "photonic"
PY
    then
      return 0
    fi
    sleep 1
  done
  return 1
}

# ── Run the stories ──────────────────────────────────────────────────────────
declare -A RESULT
overall=0

echo "== acceptance stories: ${STORIES[*]} (strict=${STRICT}) =="
echo "   binary: $BIN"
echo "   logs:   $LOG_DIR"
echo

for story in "${STORIES[@]}"; do
  script="${REPO_ROOT}/${STORY_SCRIPT[$story]}"
  [ -f "$script" ] || die "story script missing: $script"

  port="$(free_port)"
  server_log="${LOG_DIR}/${story}-server.log"
  PHOTONIC_MCP_SECRET="$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')"
  export PHOTONIC_MCP_SECRET
  TMPDIR="${REPO_ROOT}/target/acceptance-tmp/${story}"
  mkdir -p "$TMPDIR"
  export TMPDIR

  echo "── ${story}: ${STORY_TITLE[$story]} (port ${port}) ────────────────"
  # Each story gets a pristine server: the MCP surface mutates one in-process
  # document, so a shared server would let story N+1 see story N's sequences.
  ( cd "$REPO_ROOT" && exec "$BIN" --headless --mcp-port "$port" ) >"$server_log" 2>&1 &
  SERVER_PID=$!

  if ! wait_for_server "$port" "$SERVER_PID"; then
    echo "[FAIL] ${story}: MCP server never became ready on port ${port} (see ${server_log})"
    tail -n 30 "$server_log" || true
    RESULT[$story]="SERVER-FAIL"
    overall=1
    cleanup
    echo
    continue
  fi

  story_args=(--url "http://127.0.0.1:${port}/mcp")
  [ "$STRICT" = 1 ] && story_args+=(--strict)
  if [ "$story" = as2 ] && [ -n "$OUT_DIR" ]; then
    story_args+=(--out-dir "$OUT_DIR")
  fi

  if ( cd "$REPO_ROOT" && python3 "$script" "${story_args[@]}" ); then
    RESULT[$story]=PASS
  else
    RESULT[$story]=FAIL
    overall=1
    echo "   (server log: ${server_log})"
  fi

  cleanup
  echo
done

# ── Summary ──────────────────────────────────────────────────────────────────
echo "== CAP-019 acceptance-story summary =="
for story in "${STORIES[@]}"; do
  printf '  %-4s %-12s %s\n' "$story" "${RESULT[$story]}" "${STORY_TITLE[$story]}"
done

if [ "$overall" -ne 0 ]; then
  echo
  echo "At least one acceptance story failed. Server logs: ${LOG_DIR}"
fi
exit "$overall"
