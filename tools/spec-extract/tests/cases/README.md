# Historical-drift regression corpus

Each `sd-NN/` directory reconstructs a spec-vs-code drift that ACTUALLY
happened (the 17 `SD-*` findings in `docs/specs/video-editor/27-spec-audit.md`
§3) as a minimal, self-contained case: the ORIGINAL drifted `spec-assert` in a
tiny `doc.md`, a hand-written miniature `index.json` (spec-extract's schema)
carrying the real value, and the exact expected checker output. The suite in
`../spec_drift.rs` replays them so a checker that stops catching a known drift
fails CI.

`malformed/` is not an SD case; it exercises the exit-2 path for an
unrecognized assertion form (§3.2: a silently-ignored assertion is worse than
no assertion).

Cases present in this commit:

| Case | Form exercised | Proves |
|------|----------------|--------|
| `sd-01` | `symbol-exists` on a missing type | crate/type going missing is caught |
| `sd-03` | `const … == <stale>` | §3.4 three-line failure text, byte-exact |
| `sd-04` | `ci-step-contains` with the substring gone | a removed CI step is caught |
| `sd-05` | `dep-present` on an absent dep | SD-5's "asserted a dep never added" |
| `sd-13` | `dep-absent` on a present dep | SD-13's "asserted absent, later adopted" |
| `malformed` | unknown token | exit code 2, not a silent pass |

Each frozen case carries its own `doc.md`, miniature `index.json`, and — for the
dep/ci forms — a `Cargo.toml` / `.github/workflows/ci.yml` stub, so the checker
runs with `--root` pointed at the case dir and never touches the real repo.

The PASSING real-tree forms (`dep-present proptest`/`criterion`, `feature-absent`,
`const … == 4`, `if X then Y`) are exercised directly against the live workspace
index inside `../spec_drift.rs` rather than as frozen case dirs, because their
ground truth IS the repo (freezing a `Cargo.toml` snapshot would just re-test
the snapshot).

## `anchored/` — §3.1 structural block comparison

`anchored/<case>/` corpora exercise the anchored ```rust `// spec-source:`
mechanism: `exact-match` and `abbrev-ok` (a `...` block listing a real subset)
pass; `field-added`, `abbrev-bad` (a `...` block listing a field that does not
exist), `order-swapped`, and `enum-variant-reordered` each fail with the §3.4
failure shape. `../spec_drift.rs` drives them, passing `--spec-extract` so each
block is parsed by the one Rust parser in the system.

## `acceptance/` — §4.3 acceptance-index enforcement

`acceptance/<case>/{docs,src}/` corpora drive `gen-acceptance-index.py --check`:
`covered-with-test` and `waived-with-reason` pass; `covered-no-test`,
`waived-no-reason`, `unknown-id`, and `duplicate-id` each fail, proving the
status field cannot become decorative.

## Unchecked SD findings (§2: behaviour, not structure)

These six are recorded here so a future reader does not assume they were
forgotten. The drift checker verifies STRUCTURE; each of these is a claim about
runtime BEHAVIOUR or required a call-graph read, which §2's rule assigns to
tests, not to a machine-checkable annotation:

| SD | Why the checker cannot see it |
|----|-------------------------------|
| SD-7  | Export is a runtime stub — "does nothing yet" is a behaviour claim |
| SD-9  | The finding turned out to be false; nothing to assert |
| SD-11 | Drop-frame separator handling is a formatting behaviour |
| SD-12 | `master_level()` returning `None` is runtime state, not signature |
| SD-14 | Stale phase model is prose narrative, no structural anchor |
| SD-16 | Needed a call-graph read; the index is declaration-only |
