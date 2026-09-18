# tools/

Standalone, stdlib-only Python scripts. Each file in this directory is
self-contained — no shared local module, no `requirements.txt` — matching
the "one script, one job" convention started by `gen-mcp-docs.py`.

## Docs / fixtures generators

- **`gen-mcp-docs.py`** — regenerates `docs/mcp-api.md` from the live tool
  list (`cargo run -p photonic-mcp --bin dump_tools | python3
  tools/gen-mcp-docs.py > docs/mcp-api.md`). No server needed — reads the
  tool manifest straight from `photonic_mcp::server::tool_list()`.
  Protocol / PathPolicy / Pattern B / MCPB: see
  `docs/specs/mcp-2026-07-28.md`.
- **`mcp_tool_smoke.py`** — live full-catalog **registration** smoke against a
  running headless MCP server (`tools/list` full, then `tools/call` each name
  with `{}`). Pass when no `Unknown tool` responses. See spec §8.
- **`gen-test-fixtures.py`** — regenerates the video-editor test-media
  corpus under `crates/photonic-video/tests/fixtures/`. ffmpeg-dependent,
  run locally when a fixture needs to change; CI consumes the committed
  output rather than regenerating it. See that directory's own `README.md`.

## Structural spec-verification gates

Unlike the generators above (which produce artefacts humans then read or
commit), these are **run by CI** on every PR to fail the build when the spec
docs drift from the code (`40-spec-verification.md` §3).

- **`spec-extract`** (Rust crate under `tools/spec-extract/`) — parses every
  `crates/*/src|tests/**/*.rs` with `syn` and emits a JSON structural index of
  the workspace's public API (struct/enum/const/static/fn/type/trait, with
  field and variant order preserved). It is the ONLY Rust parser in the drift
  system and depends on no `photonic-*` crate, so the cheap `lint` job can run
  it. `cargo run -q -p spec-extract -- --out spec-index.json`.
- **`check-spec-drift.py`** — evaluates inline `<!-- spec-assert: … -->`
  claims in `docs/` against that index (const values, `dep-present`/`-absent`,
  `symbol-exists`, `ci-step-contains`, `feature-present`/`-absent`, and
  `if X then Y` conditionals). Anchored ```rust `// spec-source:` blocks are
  compared structurally by piping each block through `spec-extract
  --stdin-fragment` (§3.1); pass `--spec-extract <bin>` when such blocks exist.
  Exit 0 clean, 1 drift, 2 malformed assertion.
  `python3 tools/check-spec-drift.py --index spec-index.json`.
- **`gen-acceptance-index.py`** — regenerates
  `docs/specs/video-editor/ACCEPTANCE.md` from the per-doc `## <n>. Acceptance`
  tables and cross-references `Covers: ACC-…` annotations in the source tree
  (`40-spec-verification.md` §4). Run by CI with `--check` as a hard gate: a
  `covered` row with no backing `Covers:` test, a `Covers:` naming an unknown
  id, a reason-less `waived` row, or a duplicate/mislocated id fails the build.
  `python3 tools/gen-acceptance-index.py > docs/specs/video-editor/ACCEPTANCE.md`.

## Acceptance-story MCP scripts

- **`as1_arrange_cut.py`** — AS-1 "Social clip" story, arrange + cut slice
  (create sequence, import fixtures, insert/split/move/trim/ripple,
  `render_frame_at` spot-checks, export).
- **`as2_proxy_edit.py`** — AS-2 "Short film" story, the **full** slice:
  import fixtures, `generate_proxies` + toggle `set_proxy_mode`, edit,
  multi-track edit with a cross-dissolve transition (`set_transition`), a
  per-clip node composition (`create_clip_composition` +
  `add_graph_node`/`add_graph_edge`, `get_graph` asserts it compiles), a
  full grade pass (CDL + curves via `set_grade`, `apply_lut`, `get_scopes`),
  a mixer touch (`set_track_audio` + `audio_fx` EQ/compressor), then export
  both an AV1 master and a web H.264 (ffprobe-verified).
- **`as3_motion_graphics.py`** — AS-3 "Motion graphics" story, full slice:
  create a sequence, place + animate a vector-doc clip (`set_keyframe` on
  its transform), composite it over footage in a node composition, mock
  auto-caption + grade, export WebM VP9 with alpha (ffprobe-verified the
  `alpha_mode` stream tag is present, CAP-021).

All three are scripts, not `cargo test`s: they drive a **running** Photonic
MCP server over HTTP JSON-RPC (`POST /mcp`), the same protocol a real MCP
client (e.g. Claude) speaks. `docs/specs/video-editor/11-testing-phasing.md`
§3.4/P8 calls for one such script per acceptance story (AS-1/2/3); AS-2 and
AS-3 above are each other's completion of that gate — see each script's own
module docstring for exactly what it does (and, for AS-1, still doesn't
cover yet — captions/reframe/grade land on AS-1 in a later increment).

### Running them

```sh
# 1. Launch a headless Photonic MCP server (no GUI window) in one terminal:
cargo run -p photonic-app -- --headless
# (add --mcp-port <N> to use a non-default port; default is 7842)

# 2. Run a story script against it from another terminal:
python3 tools/as1_arrange_cut.py
python3 tools/as2_proxy_edit.py
python3 tools/as3_motion_graphics.py

# Point at a non-default port/host:
python3 tools/as1_arrange_cut.py --url http://127.0.0.1:7842/mcp
# or:
PHOTONIC_MCP_URL=http://127.0.0.1:7842/mcp python3 tools/as1_arrange_cut.py
```

A running GUI instance also serves the same `/mcp` endpoint, so pointing
`--url` at one works too — headless is just the lighter way to get a server
up for scripted runs.

### Output and exit codes

Each script prints one `[PASS]` / `[FAIL]` / `[SKIP]` line per step and a
final `N/M passed, S skipped, F failed` summary. Exit code is `0` iff
nothing failed (skips don't count as failures — they're reserved for
environment gaps the script can't control, e.g. no GPU adapter for the
engine-backed tools, no `ffmpeg` on `PATH` for export, or no `ffprobe` on
`PATH` for `as2`/`as3`'s export-verification steps). A nonzero exit means at
least one step genuinely failed, which is what makes these CI-runnable once
a GPU + ffmpeg runner is wired up (currently manual / local-only — see
`11-testing-phasing.md` §3.4).

### Fixtures

All three scripts import from the committed corpus at
`crates/photonic-video/tests/fixtures/`. If a fixture is missing, regenerate
the whole corpus with `python3 tools/gen-test-fixtures.py` (needs `ffmpeg`
on `PATH`). No 4K fixture exists in that corpus (everything is kept tiny to
stay inside its size budget) — `as2_proxy_edit.py` uses the corpus's
largest/longest real asset as a stand-in; see that script's module
docstring for why that's still a meaningful test of the proxy-edit slice.
`as3_motion_graphics.py` additionally reuses one of `photonic-render`'s own
golden vector-doc fixtures (`crates/photonic-render/tests/golden/`) as its
`.photon` vector-document asset — read-only, never modified — since no
plain vector-doc fixture lives in the video-editor corpus (see that
script's module docstring).
