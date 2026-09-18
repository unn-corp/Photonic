#!/usr/bin/env python3
"""Generate (and validate) the video-editor acceptance index from the per-doc
Acceptance tables (40-spec-verification.md §4).

Every doc under `docs/specs/video-editor/` that has a `## <n>. … Acceptance`
section carries a 4-column table `| # | Test | Id | Status |`. This tool reads
those rows, cross-references `Covers: ACC-…` annotations in the source tree, and
either emits a single `ACCEPTANCE.md` roll-up (stdout) or, with `--check`,
validates the invariants and emits nothing.

HARD FAILURES (§4.3 — otherwise the status field is decorative):
  1. a `covered` row with no `Covers:` naming its id;
  2. a `Covers:` naming an id that appears in no doc;
  3. a `waived` row with no reason text after the colon;
  4. a duplicate id, an id whose doc/section digits disagree with its location,
     a non-contiguous `#` sequence, or an unknown status word.
A row that is `open` but DOES have a matching `Covers:` is a WARNING on stderr —
the annotation landing before the status flip is normal ordering.

Usage:
    python3 tools/gen-acceptance-index.py > docs/specs/video-editor/ACCEPTANCE.md
    python3 tools/gen-acceptance-index.py --check
"""
import argparse
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# `## 10. Acceptance` or `## 10. CPU/GPU Acceptance`, capturing the section num.
_ACCEPT_HEADING = re.compile(r"^##\s+(\d+)\.\s+(.*)$")
_NEXT_HEADING = re.compile(r"^#{1,2}\s")
_ID = re.compile(r"^ACC-(\d{2})-(\d{2})-(\d{2})$")
_COVERS = re.compile(
    r"Covers:\s*(ACC-\d{2}-\d{2}-\d{2}(?:\s*,\s*ACC-\d{2}-\d{2}-\d{2})*)"
)
_FN = re.compile(r"(?:fn|def)\s+(\w+)")

_STATUS_WORDS = ("open", "covered", "waived")


class Violation(Exception):
    """A hard acceptance-index failure (§4.3)."""


def strip_ticks(cell: str) -> str:
    return cell.strip().strip("`").strip()


def parse_row(cells: list[str]):
    """Return (num:int, test:str, id:str, status:str, reason:str|None) or None
    if the row is the header/separator."""
    if len(cells) != 4:
        return None
    num_s, test, id_s, status_s = (c.strip() for c in cells)
    if num_s in ("#", "", "---") or set(num_s) <= {"-", ":"}:
        return None
    if not num_s.isdigit():
        raise Violation(f"non-numeric row number {num_s!r}")
    status_s = strip_ticks(status_s)
    reason = None
    if status_s.startswith("waived"):
        rest = status_s[len("waived"):].lstrip()
        if rest.startswith(":"):
            reason = rest[1:].strip()
        status = "waived"
        if not reason:
            raise Violation(f"waived row {strip_ticks(id_s)} has no reason text")
    else:
        status = status_s
        if status not in _STATUS_WORDS:
            raise Violation(f"unknown status word {status_s!r}")
    return int(num_s), strip_ticks(test), strip_ticks(id_s), status, reason


def doc_number(path: Path) -> str:
    m = re.match(r"(\d\d)-", path.name)
    return m.group(1) if m else "??"


def parse_doc(path: Path):
    """Yield (doc, section, num, test, id, status, reason) for one doc's
    acceptance table."""
    doc = doc_number(path)
    lines = path.read_text(encoding="utf-8").splitlines()
    i, n = 0, len(lines)
    while i < n:
        m = _ACCEPT_HEADING.match(lines[i])
        if not (m and "Acceptance" in m.group(2)):
            i += 1
            continue
        section = f"{int(m.group(1)):02d}"
        i += 1
        expect_num = 0
        while i < n and not _NEXT_HEADING.match(lines[i]):
            line = lines[i].strip()
            if line.startswith("|"):
                cells = [c for c in line.strip("|").split("|")]
                row = parse_row(cells)
                if row is not None:
                    num, test, id_s, status, reason = row
                    expect_num += 1
                    if num != expect_num:
                        raise Violation(
                            f"doc {doc} §{int(section)}: non-contiguous row "
                            f"# {num} (expected {expect_num})"
                        )
                    im = _ID.match(id_s)
                    if not im:
                        raise Violation(f"doc {doc}: malformed id {id_s!r}")
                    if (im.group(1), im.group(2), int(im.group(3))) != (
                        doc,
                        section,
                        num,
                    ):
                        raise Violation(
                            f"id {id_s} disagrees with its location "
                            f"(doc {doc} §{int(section)} row {num})"
                        )
                    yield doc, section, num, test, id_s, status, reason
            i += 1
    return


def collect_docs(docs_dir: Path):
    rows = []
    for md in sorted(docs_dir.glob("[0-9][0-9]-*.md")):
        rows.extend(parse_doc(md))
    return rows


def collect_covers(src_roots: list[Path]):
    """Map id -> sorted list of covering symbol names, scanning .rs and (under
    tools/) .py sources."""
    covers: dict[str, set[str]] = {}
    for root in src_roots:
        if not root.exists():
            continue
        files = list(root.rglob("*.rs"))
        if root.name == "tools" or root.parts[-1:] == ("tools",):
            files += list(root.rglob("*.py"))
        for f in sorted(files):
            try:
                text = f.read_text(encoding="utf-8")
            except OSError:
                continue
            for m in _COVERS.finditer(text):
                ids = [x.strip() for x in m.group(1).split(",")]
                after = text[m.end():]
                fn = _FN.search(after)
                who = fn.group(1) if fn else f.name
                for acc in ids:
                    covers.setdefault(acc, set()).add(who)
    return covers


def validate(rows, covers) -> list[str]:
    """Return a list of hard-failure messages (empty == clean). Warnings go to
    stderr as a side effect."""
    failures: list[str] = []
    seen: dict[str, str] = {}
    doc_ids = {r[4] for r in rows}

    for _doc, _section, _num, _test, id_s, status, _reason in rows:
        if id_s in seen:
            failures.append(f"duplicate id {id_s}")
        seen[id_s] = status
        if status == "covered" and id_s not in covers:
            failures.append(f"{id_s} is `covered` but no test names it in a `Covers:`")
        if status == "open" and id_s in covers:
            print(
                f"warning: {id_s} is `open` but already covered by "
                f"{', '.join(sorted(covers[id_s]))}",
                file=sys.stderr,
            )

    for acc in sorted(covers):
        if acc not in doc_ids:
            failures.append(f"`Covers: {acc}` names an id that appears in no doc")

    return failures


DOC_TITLES: dict[str, str] = {}


def doc_title(docs_dir: Path, doc: str) -> str:
    if doc not in DOC_TITLES:
        DOC_TITLES[doc] = doc
        for md in docs_dir.glob(f"{doc}-*.md"):
            first = md.read_text(encoding="utf-8").splitlines()
            for line in first:
                if line.startswith("# "):
                    DOC_TITLES[doc] = line[2:].strip()
                    break
            break
    return DOC_TITLES[doc]


def render(docs_dir: Path, rows, covers) -> str:
    by_doc: dict[str, list] = {}
    for r in rows:
        by_doc.setdefault(r[0], []).append(r)

    status_counts: dict[str, int] = {}
    for r in rows:
        status_counts[r[5]] = status_counts.get(r[5], 0) + 1

    lines = [
        "# Video-Editor Acceptance Index",
        "",
        "<!-- GENERATED FILE — do not edit by hand. -->",
        "<!-- Regenerate with: "
        "python3 tools/gen-acceptance-index.py > docs/specs/video-editor/ACCEPTANCE.md -->",
        "",
        f"**{len(rows)}** acceptance criteria across "
        f"{len(by_doc)} documents.",
        "",
        "| Status | Count |",
        "| --- | --- |",
    ]
    for status in sorted(status_counts):
        lines.append(f"| `{status}` | {status_counts[status]} |")
    lines.append("")

    for doc in sorted(by_doc):
        lines.append(f"## {doc} — {doc_title(docs_dir, doc)}")
        lines.append("")
        lines.append("| Id | Test | Status | Covered by |")
        lines.append("| --- | --- | --- | --- |")
        for _doc, _section, _num, test, id_s, status, reason in sorted(
            by_doc[doc], key=lambda r: r[4]
        ):
            status_cell = f"waived: {reason}" if status == "waived" else status
            covered_by = ", ".join(f"`{c}`" for c in sorted(covers.get(id_s, [])))
            test_cell = test.replace("|", "\\|")
            lines.append(
                f"| `{id_s}` | {test_cell} | `{status_cell}` | {covered_by} |"
            )
        lines.append("")

    return "\n".join(lines).rstrip() + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description="Video-editor acceptance index.")
    ap.add_argument("--docs", default=str(REPO_ROOT / "docs/specs/video-editor"))
    ap.add_argument("--src", nargs="+", default=["crates", "tools"])
    ap.add_argument("--check", action="store_true", help="validate only, emit nothing")
    args = ap.parse_args()

    docs_dir = Path(args.docs)
    src_roots = [
        Path(s) if Path(s).is_absolute() else (docs_dir_root(docs_dir) / s)
        for s in args.src
    ]

    try:
        rows = collect_docs(docs_dir)
        covers = collect_covers(src_roots)
        failures = validate(rows, covers)
    except Violation as exc:
        print(f"gen-acceptance-index: {exc}", file=sys.stderr)
        return 1

    if failures:
        for f in failures:
            print(f"gen-acceptance-index: {f}", file=sys.stderr)
        return 1

    if args.check:
        return 0

    sys.stdout.write(render(docs_dir, rows, covers))
    return 0


def docs_dir_root(docs_dir: Path) -> Path:
    """Best-effort repo root for resolving relative --src (walk up past
    docs/specs/video-editor); falls back to REPO_ROOT."""
    p = docs_dir.resolve()
    for parent in [p, *p.parents]:
        if (parent / "Cargo.toml").is_file() or (parent / "crates").is_dir():
            return parent
    return REPO_ROOT


if __name__ == "__main__":
    sys.exit(main())
