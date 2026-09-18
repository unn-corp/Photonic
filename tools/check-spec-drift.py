#!/usr/bin/env python3
"""Evaluate inline `<!-- spec-assert: ... -->` claims in the spec docs against
the real code, using the JSON index produced by `tools/spec-extract`
(40-spec-verification.md §3.2).

The checker verifies STRUCTURE only — that a const still equals a value, a dep
is still present/absent, a symbol still exists, a CI step still mentions a
string, a crate feature is/absent, and conditional (`if X then Y`) forms of
those. Behaviour is verified by tests, design by humans (§3's hard rule). This
script deliberately does NOT understand YAML semantics or parse Rust with
regexes; the one Rust parser in the system is `spec-extract`.

Anchored `spec-source` code blocks (§3.1) are NOT handled here — that is task 5.

Usage:
    cargo run -q -p spec-extract -- --out spec-index.json
    python3 tools/check-spec-drift.py --index spec-index.json

    check-spec-drift.py --index <spec-index.json> [--root <repo-root>]
                        [--docs docs/] [--format text|json]

Exit codes:
    0  clean
    1  one or more drift failures
    2  malformed assertion / missing index / unparseable Cargo.toml
"""
import argparse
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# Cargo dependency tables to union when resolving dep-present/dep-absent.
_DEP_TABLE_KINDS = ("dependencies", "dev-dependencies", "build-dependencies")

# A trailing Rust integer-literal type suffix, so `4u32` compares equal to `4`.
_INT_SUFFIX = re.compile(r"^(-?\d+)(?:u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize)$")


class Malformed(Exception):
    """Raised for an unrecognized or wrong-arity assertion (exit code 2)."""


def norm_ws(text: str) -> str:
    return " ".join(text.split())


def norm_value(text: str) -> str:
    """Whitespace-collapse and strip a trailing int-type suffix."""
    t = norm_ws(text)
    m = _INT_SUFFIX.match(t)
    return m.group(1) if m else t


def norm_dep(name: str) -> str:
    """`-`/`_` are equivalent in crate names (`egui-snarl` == `egui_snarl`)."""
    return name.replace("-", "_")


# ── index ────────────────────────────────────────────────────────────────────


class Index:
    """Lookup maps over the spec-extract JSON index."""

    def __init__(self, data: dict):
        self.items = data.get("items", [])
        self.by_file_name: dict[str, dict] = {}
        self.by_module: dict[str, dict] = {}
        self.by_name: dict[str, list[dict]] = {}
        for it in self.items:
            self.by_file_name[f"{it['file']}::{it['name']}"] = it
            self.by_module[f"{it['module_path']}::{it['name']}"] = it
            self.by_name.setdefault(it["name"], []).append(it)

    def resolve(self, ref: str):
        """Resolve a `<file>::<Name>`, `<module::path::Name>`, or bare `<Name>`
        reference to its Item, or None."""
        if ref in self.by_file_name:
            return self.by_file_name[ref]
        if ref in self.by_module:
            return self.by_module[ref]
        # Bare name (the §3.4 example uses `const CURRENT_FORMAT_VERSION == 3`).
        if "::" not in ref:
            hits = self.by_name.get(ref)
            if hits:
                return hits[0]
        return None


# ── Cargo.toml scanning ──────────────────────────────────────────────────────


def cargo_manifests(root: Path) -> list[Path]:
    """Workspace root manifest + every `crates/*/Cargo.toml`."""
    out = []
    top = root / "Cargo.toml"
    if top.is_file():
        out.append(top)
    crates = root / "crates"
    if crates.is_dir():
        out += sorted(crates.glob("*/Cargo.toml"))
    return out


def _dep_keys_from_table(table: dict) -> set[str]:
    """Normalized keys from one `[…dependencies]` table, honouring `package =`
    renames (both the table key and the renamed package count as present)."""
    keys: set[str] = set()
    for key, spec in table.items():
        keys.add(norm_dep(key))
        if isinstance(spec, dict) and "package" in spec:
            keys.add(norm_dep(spec["package"]))
    return keys


def collect_deps(root: Path) -> set[str]:
    deps: set[str] = set()
    for manifest in cargo_manifests(root):
        try:
            data = tomllib.loads(manifest.read_text(encoding="utf-8"))
        except (tomllib.TOMLDecodeError, OSError) as exc:
            raise Malformed(f"unparseable {manifest}: {exc}") from exc
        # [workspace.dependencies]
        ws = data.get("workspace", {})
        if isinstance(ws, dict):
            deps |= _dep_keys_from_table(ws.get("dependencies", {}))
        # [dependencies], [dev-dependencies], [build-dependencies]
        for kind in _DEP_TABLE_KINDS:
            deps |= _dep_keys_from_table(data.get(kind, {}))
        # [target.<cfg>.<kind>]
        for cfg_table in data.get("target", {}).values():
            if not isinstance(cfg_table, dict):
                continue
            for kind in _DEP_TABLE_KINDS:
                deps |= _dep_keys_from_table(cfg_table.get(kind, {}))
    return deps


def crate_features(root: Path, crate: str) -> set[str] | None:
    """`[features]` keys for a crate, or None if the crate manifest is absent.
    A crate with NO `[features]` table returns an empty set (§3.2: missing table
    means 'no features', not an error)."""
    manifest = root / "crates" / crate / "Cargo.toml"
    if not manifest.is_file():
        return None
    try:
        data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    except (tomllib.TOMLDecodeError, OSError) as exc:
        raise Malformed(f"unparseable {manifest}: {exc}") from exc
    return set(data.get("features", {}).keys())


def ci_text(root: Path) -> str:
    """Concatenated raw text of every workflow YAML — a deliberately dumb
    substring corpus (§5 forbids extending this into YAML semantics)."""
    wf = root / ".github" / "workflows"
    chunks = []
    if wf.is_dir():
        for path in sorted(wf.glob("*.yml")) + sorted(wf.glob("*.yaml")):
            try:
                chunks.append(path.read_text(encoding="utf-8"))
            except OSError:
                continue
    return "\n".join(chunks)


# ── evaluation ───────────────────────────────────────────────────────────────


class Result:
    __slots__ = ("ok", "actual", "site")

    def __init__(self, ok: bool, actual: str = "", site: str = ""):
        self.ok = ok
        self.actual = actual
        self.site = site


class Evaluator:
    def __init__(self, root: Path, index: Index):
        self.root = root
        self.index = index
        self._deps: set[str] | None = None
        self._ci: str | None = None

    def deps(self) -> set[str]:
        if self._deps is None:
            self._deps = collect_deps(self.root)
        return self._deps

    def ci(self) -> str:
        if self._ci is None:
            self._ci = ci_text(self.root)
        return self._ci

    def eval(self, body: str) -> Result:
        body = body.strip()
        if body.startswith("if "):
            return self._eval_conditional(body)
        tokens = body.split()
        if not tokens:
            raise Malformed("empty assertion")
        head = tokens[0]
        if head == "const":
            return self._eval_const(body)
        if head in ("dep-present", "dep-absent"):
            return self._eval_dep(head, tokens)
        if head == "symbol-exists":
            return self._eval_symbol(tokens)
        if head == "ci-step-contains":
            return self._eval_ci(body, tokens)
        if head in ("feature-present", "feature-absent"):
            return self._eval_feature(head, tokens)
        raise Malformed(f"unknown assertion form: {body}")

    def _eval_const(self, body: str) -> Result:
        rest = body[len("const"):]
        if "==" not in rest:
            raise Malformed(f"const needs `==`: {body}")
        ref, expected = (p.strip() for p in rest.split("==", 1))
        if not ref or not expected:
            raise Malformed(f"const needs a ref and a value: {body}")
        item = self.index.resolve(ref)
        if item is None:
            return Result(False, "symbol not found")
        actual = norm_value(item.get("value") or "")
        site = f"{item['file']}:{item['line']}"
        return Result(norm_value(expected) == actual, actual, site)

    def _eval_dep(self, head: str, tokens: list[str]) -> Result:
        if len(tokens) != 2:
            raise Malformed(f"{head} takes exactly one crate name")
        name = norm_dep(tokens[1])
        present = name in self.deps()
        if head == "dep-present":
            return Result(present, "present" if present else "absent from all Cargo.toml")
        return Result(not present, "present in a Cargo.toml" if present else "absent")

    def _eval_symbol(self, tokens: list[str]) -> Result:
        if len(tokens) != 2:
            raise Malformed("symbol-exists takes exactly one <file>::<item>")
        ref = tokens[1]
        if "::" in ref:
            head, *rest = ref.split("::")
            # `<file>::<Type>::<method>` — methods are not top-level items, so
            # the index cannot resolve it: malformed, never a silent pass (§3.2).
            if head.endswith(".rs") and len(rest) > 1:
                raise Malformed(
                    f"symbol-exists cannot resolve a method path: {ref}"
                )
        item = self.index.resolve(ref)
        return Result(item is not None, "found" if item else "not found in index")

    def _eval_ci(self, body: str, tokens: list[str]) -> Result:
        if len(tokens) < 2:
            raise Malformed("ci-step-contains takes a substring")
        needle = body[len("ci-step-contains"):].strip()
        present = needle in self.ci()
        return Result(present, "found" if present else "not found in .github/workflows/*")

    def _eval_feature(self, head: str, tokens: list[str]) -> Result:
        if len(tokens) != 2 or "/" not in tokens[1]:
            raise Malformed(f"{head} takes <crate>/<feature>")
        crate, feature = tokens[1].split("/", 1)
        feats = crate_features(self.root, crate)
        if feats is None:
            raise Malformed(f"{head}: no crate `{crate}`")
        present = feature in feats
        if head == "feature-present":
            return Result(present, "present" if present else "absent")
        return Result(not present, "present" if present else "absent")

    def _eval_conditional(self, body: str) -> Result:
        rest = body[len("if "):]
        if " then " not in rest:
            raise Malformed(f"if needs a ` then `: {body}")
        antecedent, consequent = rest.split(" then ", 1)
        ante = self.eval(antecedent.strip())
        # FALSE antecedent => whole assertion passes vacuously.
        if not ante.ok:
            return Result(True)
        # TRUE antecedent => result is the consequent's.
        return self.eval(consequent.strip())


# ── driver ───────────────────────────────────────────────────────────────────

_ASSERT = re.compile(r"<!--\s*spec-assert:\s*(.*?)\s*-->")


_FENCE = re.compile(r"^\s*(`{3,}|~{3,})(.*)$")


def _fence_info(line: str):
    """(char, length, info-string) if `line` opens/closes a code fence, else
    None."""
    m = _FENCE.match(line)
    if not m:
        return None
    seq = m.group(1)
    return seq[0], len(seq), m.group(2).strip()


def _fenced_span(lines: list[str], i: int, ch: str, length: int) -> int:
    """Index of the line that closes the fence opened at `i` (same char, length
    >= the opener's, per CommonMark nesting), or len(lines) if unterminated."""
    j = i + 1
    while j < len(lines):
        info = _fence_info(lines[j])
        if info and info[0] == ch and info[1] >= length:
            return j
        j += 1
    return j


def scan_docs(docs_dir: Path):
    """Yield (rel_path, 1-based line, body) for every spec-assert in the tree.

    Whole fenced code blocks (``` … ``` / ~~~ … ~~~, with CommonMark length
    nesting so a ```` ```` block swallows an inner ``` ``` verbatim) are skipped:
    real assertions live in prose as HTML comments, whereas a fenced block is an
    ILLUSTRATIVE example of the syntax (e.g. 40 §3.2's own example table, which
    deliberately shows a drifted `== 3` and a not-yet-real `feature-present`).
    Evaluating those would red the gate on a clean tree. Anchored `spec-source`
    code blocks (§3.1) are a separate mechanism handled by their own scanner.
    """
    for md in sorted(docs_dir.rglob("*.md")):
        try:
            lines = md.read_text(encoding="utf-8").splitlines()
        except OSError:
            continue
        i, n = 0, len(lines)
        while i < n:
            info = _fence_info(lines[i])
            if info:
                i = _fenced_span(lines, i, info[0], info[1]) + 1
                continue
            m = _ASSERT.search(lines[i])
            if m:
                yield md, i + 1, m.group(1).strip()
            i += 1


def rel_to(path: Path, base: Path) -> str:
    try:
        return path.resolve().relative_to(base.resolve()).as_posix()
    except ValueError:
        return path.as_posix()


# ── anchored code blocks (§3.1) ──────────────────────────────────────────────
#
# A fenced ```rust block whose FIRST content line is `// spec-source: <ref>` is
# compared structurally against the real item: field/variant NAMES and ORDER,
# const value, or fn signature. We never regex Rust (§3.3's trap) — the block is
# handed to `spec-extract --stdin-fragment`, the one Rust parser in the system.
# A bare `...` line is not valid Rust, so it is stripped before parsing and
# recorded out-of-band: it SUPPRESSES the completeness check but not existence
# and not relative order.

_SPEC_SOURCE = re.compile(r"^\s*//\s*spec-source:\s*(\S+)\s*$")


def scan_anchored(docs_dir: Path):
    """Yield (md_path, line, ref, fragment_src, abbreviated) per anchored block.

    `line` is the 1-based line of the `spec-source` comment; `fragment_src` is
    the block with the comment and any `...` lines removed."""
    for md in sorted(docs_dir.rglob("*.md")):
        try:
            lines = md.read_text(encoding="utf-8").splitlines()
        except OSError:
            continue
        i, n = 0, len(lines)
        while i < n:
            info = _fence_info(lines[i])
            if not info:
                i += 1
                continue
            ch, length, lang = info
            close = _fenced_span(lines, i, ch, length)
            # Only a TOP-LEVEL ```rust fence is an anchored block. An inner rust
            # fence nested in a longer ```` ```` block (e.g. 40 §3.1's own
            # example) is skipped whole — its lang line is content, not a fence.
            if "rust" in lang.lower():
                body = lines[i + 1:close]
                first = next((k for k, ln in enumerate(body) if ln.strip()), None)
                if first is not None:
                    sm = _SPEC_SOURCE.match(body[first])
                    if sm:
                        ref = sm.group(1)
                        src_line = i + 1 + first + 1
                        kept, abbreviated = [], False
                        for ln in body:
                            if _SPEC_SOURCE.match(ln):
                                continue
                            if ln.strip() == "...":
                                abbreviated = True
                                continue
                            kept.append(ln)
                        yield md, src_line, ref, "\n".join(kept), abbreviated
            i = close + 1


def parse_fragment(spec_extract: str, src: str) -> dict:
    """Parse one Rust item via `spec-extract --stdin-fragment` and return its
    single Item record."""
    try:
        proc = subprocess.run(
            [spec_extract, "--stdin-fragment"],
            input=src,
            capture_output=True,
            text=True,
        )
    except OSError as exc:
        raise Malformed(f"cannot run spec-extract ({spec_extract}): {exc}") from exc
    if proc.returncode != 0:
        raise Malformed(f"spec-extract could not parse anchored block: {proc.stderr.strip()}")
    return json.loads(proc.stdout)


def _render_names(expected: list[str], real: list[str]) -> str:
    """Comma-joined reality with `+extra` / `-missing` markers vs. expectation."""
    exp_set, real_set = set(expected), set(real)
    parts = [r if r in exp_set else f"+{r}" for r in real]
    parts += [f"-{e}" for e in expected if e not in real_set]
    return ", ".join(parts)


def _compare_names(expected: list[str], real: list[str], abbreviated: bool):
    """Return (ok, actual_render) for a field/variant name-list comparison."""
    if abbreviated:
        real_set, exp_set = set(real), set(expected)
        exists = all(e in real_set for e in expected)
        # Relative order: the listed names appear in `real` in the same order.
        real_filtered = [r for r in real if r in exp_set]
        order_ok = real_filtered == [e for e in expected if e in real_set]
        ok = exists and order_ok
    else:
        ok = expected == real
    return (True, "") if ok else (False, _render_names(expected, real))


def compare_anchored(frag: dict, real: dict, abbreviated: bool):
    """Structural compare of a parsed block against its index item.
    Returns (ok, actual_render)."""
    kind = frag.get("kind")
    if kind == "Struct":
        return _compare_names(frag.get("fields", []), real.get("fields", []), abbreviated)
    if kind == "Enum":
        exp = [v["name"] for v in frag.get("variants", [])]
        got = [v["name"] for v in real.get("variants", [])]
        return _compare_names(exp, got, abbreviated)
    if kind in ("Const", "Static"):
        ev, av = norm_value(frag.get("value") or ""), norm_value(real.get("value") or "")
        return (True, "") if ev == av else (False, av)
    if kind == "Fn":
        es, as_ = norm_ws(frag.get("signature") or ""), norm_ws(real.get("signature") or "")
        return (True, "") if es == as_ else (False, as_)
    # TypeAlias / Trait: the block asserts existence only, already resolved.
    return (True, "")


def main() -> int:
    ap = argparse.ArgumentParser(description="Structural spec-vs-code drift checker.")
    ap.add_argument("--index", required=True, help="spec-extract JSON index")
    ap.add_argument("--root", default=str(REPO_ROOT), help="repo root")
    ap.add_argument("--docs", default=None, help="docs dir (default <root>/docs)")
    ap.add_argument("--format", choices=("text", "json"), default="text")
    ap.add_argument(
        "--spec-extract",
        default=None,
        help="path to the spec-extract binary, used to parse anchored "
        "`spec-source` code blocks (§3.1). Required only if such blocks exist.",
    )
    args = ap.parse_args()

    root = Path(args.root)
    docs_dir = Path(args.docs) if args.docs else root / "docs"

    try:
        index = Index(json.loads(Path(args.index).read_text(encoding="utf-8")))
    except (OSError, json.JSONDecodeError) as exc:
        print(f"check-spec-drift: cannot read index {args.index}: {exc}", file=sys.stderr)
        return 2

    if not docs_dir.is_dir():
        # No docs to check is clean, not an error.
        return 0

    ev = Evaluator(root, index)
    failures = []
    for md, line, body in scan_docs(docs_dir):
        doc = rel_to(md, root)
        try:
            result = ev.eval(body)
        except Malformed as exc:
            print(f"{doc}:{line}: {exc}", file=sys.stderr)
            return 2
        if not result.ok:
            failures.append(
                {
                    "doc": doc,
                    "line": line,
                    "label": "spec-assert",
                    "assertion": body,
                    "actual": result.actual,
                    "actual_site": result.site,
                }
            )

    # Anchored `spec-source` code blocks (§3.1).
    for md, line, ref, src, abbreviated in scan_anchored(docs_dir):
        doc = rel_to(md, root)
        real = index.resolve(ref)
        if real is None:
            failures.append(
                {
                    "doc": doc,
                    "line": line,
                    "label": "spec-source",
                    "assertion": ref,
                    "actual": "symbol not found",
                    "actual_site": "",
                }
            )
            continue
        if not args.spec_extract:
            print(
                f"{doc}:{line}: anchored spec-source block needs --spec-extract",
                file=sys.stderr,
            )
            return 2
        try:
            frag = parse_fragment(args.spec_extract, src)
        except Malformed as exc:
            print(f"{doc}:{line}: {exc}", file=sys.stderr)
            return 2
        ok, actual = compare_anchored(frag, real, abbreviated)
        if not ok:
            failures.append(
                {
                    "doc": doc,
                    "line": line,
                    "label": "spec-source",
                    "assertion": ref,
                    "actual": actual,
                    "actual_site": f"{real['file']}:{real['line']}",
                }
            )

    if not failures:
        return 0

    if args.format == "json":
        print(json.dumps(failures, indent=2, sort_keys=True))
        return 1

    blocks = []
    for f in failures:
        site = f"  ({f['actual_site']})" if f["actual_site"] else ""
        blocks.append(
            f"{f['doc']}:{f['line']}\n"
            f"  {f.get('label', 'spec-assert')}: {f['assertion']}\n"
            f"  actual:      {f['actual']}{site}"
        )
    sys.stdout.write("\n".join(blocks).rstrip() + "\n")
    return 1


if __name__ == "__main__":
    sys.exit(main())
