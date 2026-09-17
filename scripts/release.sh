#!/usr/bin/env bash
#
# release.sh — cut a new Photonic release.
#
# This is the single command that ships a version to users. It:
#   1. validates the version + a clean tree,
#   2. bumps the workspace version (single source of truth),
#   3. rolls CHANGELOG.md's [Unreleased] section into a dated version heading
#      and opens a fresh empty [Unreleased],
#   4. (optionally) builds the release binary as a smoke test,
#   5. commits "release: vX.Y.Z", tags vX.Y.Z, and pushes branch + tag.
#
# Pushing the tag triggers .github/workflows/release.yml, which builds + signs
# the per-platform archives and publishes them to GitHub Releases. From there
# the in-app updater (and the launch banner) carry users forward, and the
# "What's New" popup shows them the changelog section this script just wrote.
#
# Usage:
#   scripts/release.sh 0.2.0          # cut v0.2.0
#   scripts/release.sh v0.2.0         # leading 'v' is fine
#   scripts/release.sh 0.2.0 --no-build   # skip the release build smoke test
#   scripts/release.sh 0.2.0 --dry-run    # do everything except commit/tag/push
#
# Before running: put the user-facing changes for this release under the
# "## [Unreleased]" heading in CHANGELOG.md (that text becomes the release
# notes and the in-app "What's New" body).

set -euo pipefail

# ── Locate the repo root (this script lives in scripts/) ────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT"

CARGO_TOML="$ROOT/Cargo.toml"
CHANGELOG="$ROOT/CHANGELOG.md"

die() { echo "release.sh: error: $*" >&2; exit 1; }

# ── Parse args ──────────────────────────────────────────────────────────────
VERSION=""
DO_BUILD=1
DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --no-build) DO_BUILD=0 ;;
    --dry-run)  DRY_RUN=1 ;;
    -h|--help)  grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*)         die "unknown flag: $arg" ;;
    *)          [ -z "$VERSION" ] || die "version given twice"; VERSION="$arg" ;;
  esac
done

[ -n "$VERSION" ] || die "usage: scripts/release.sh <version> [--no-build] [--dry-run]"
VERSION="${VERSION#v}"   # strip a leading 'v' if present
echo "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' \
  || die "version must be semver MAJOR.MINOR.PATCH (got '$VERSION')"

TAG="v$VERSION"
TODAY="$(date +%F)"

# ── Sanity checks ───────────────────────────────────────────────────────────
[ -f "$CARGO_TOML" ]  || die "no Cargo.toml at repo root"
[ -f "$CHANGELOG" ]   || die "no CHANGELOG.md at repo root"
command -v git >/dev/null || die "git not found"

RELEASE_BRANCH="main"
CURRENT_BRANCH="$(git branch --show-current)"
[ "$CURRENT_BRANCH" = "$RELEASE_BRANCH" ] \
  || die "releases must be cut from protected '$RELEASE_BRANCH' (currently on '${CURRENT_BRANCH:-detached HEAD}')"

if [ -n "$(git status --porcelain)" ]; then
  die "working tree is dirty — commit or stash first"
fi

if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  die "tag $TAG already exists"
fi

CURRENT="$(grep -E '^version = "' "$CARGO_TOML" | head -1 | sed -E 's/^version = "([^"]+)".*/\1/')"
echo "→ current version: $CURRENT"
echo "→ new version:     $VERSION  (tag $TAG, $TODAY)"
[ "$CURRENT" != "$VERSION" ] || die "version is already $VERSION"

# Require something under [Unreleased] so releases always carry notes.
UNREL_BODY="$(awk '
  /^## \[Unreleased\]/ {grab=1; next}
  /^## \[/ {grab=0}
  grab {print}
' "$CHANGELOG" | grep -E '^\s*[-*] ' || true)"
[ -n "$UNREL_BODY" ] \
  || die "CHANGELOG.md [Unreleased] has no bullet points — add release notes there first"

# ── 1. Bump the workspace version (only the start-of-line one) ───────────────
echo "→ bumping Cargo.toml [workspace.package] version"
# The workspace version is the only `version = "…"` at column 0; dependency
# versions are inline (`kurbo = { version = … }`), so anchor to ^.
sed -i -E "0,/^version = \"[^\"]+\"/s//version = \"$VERSION\"/" "$CARGO_TOML"
grep -qE "^version = \"$VERSION\"" "$CARGO_TOML" || die "Cargo.toml bump failed"

# ── 2. Roll the changelog ────────────────────────────────────────────────────
echo "→ rolling CHANGELOG.md: [Unreleased] → [$VERSION] - $TODAY"
awk -v ver="$VERSION" -v date="$TODAY" '
  /^## \[Unreleased\]/ && !done {
    print "## [Unreleased]"
    print ""
    print "## [" ver "] - " date
    done = 1
    next
  }
  { print }
' "$CHANGELOG" > "$CHANGELOG.tmp" && mv "$CHANGELOG.tmp" "$CHANGELOG"

# ── 3. Build smoke test ──────────────────────────────────────────────────────
if [ "$DO_BUILD" -eq 1 ]; then
  echo "→ building release binary (smoke test; --no-build to skip)"
  cargo build --release -p photonic-app
else
  echo "→ skipping build (--no-build)"
fi

# ── 4. Commit, tag, push ─────────────────────────────────────────────────────
if [ "$DRY_RUN" -eq 1 ]; then
  echo "→ --dry-run: leaving changes uncommitted. Diff:"
  git --no-pager diff -- "$CARGO_TOML" "$CHANGELOG"
  echo
  echo "Dry run complete. Re-run without --dry-run to commit, tag $TAG, and push."
  exit 0
fi

echo "→ committing + tagging $TAG"
git add "$CARGO_TOML" "$CHANGELOG" Cargo.lock 2>/dev/null || git add "$CARGO_TOML" "$CHANGELOG"
git commit -m "release: $TAG"
git tag -a "$TAG" -m "Photonic $TAG"

BRANCH="$(git rev-parse --abbrev-ref HEAD)"
echo "→ pushing $BRANCH and tag $TAG"
git push origin "$BRANCH"
git push origin "$TAG"

echo
echo "✓ Released $TAG."
echo "  CI (release.yml) is now building + signing the platform archives."
echo "  Watch: https://github.com/unn-corp/Photonic/actions"
echo "  Release: https://github.com/unn-corp/Photonic/releases/tag/$TAG"
