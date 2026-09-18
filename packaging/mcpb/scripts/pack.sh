#!/usr/bin/env bash
# Build a platform-specific Photonic MCPB (binary-only).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
OUT="${1:-$ROOT/packaging/mcpb/dist}"
TARGET="${2:-}"
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)"/\1/')"
PLATFORM="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64) ARCH=x64 ;;
  aarch64|arm64) ARCH=arm64 ;;
esac
NAME="photonic-${VERSION}-${PLATFORM}-${ARCH}"
STAGE="$OUT/$NAME"
mkdir -p "$STAGE/server"
echo "Building release photonic..."
if [[ -n "$TARGET" ]]; then
  cargo build -p photonic-app --release --target "$TARGET" --manifest-path "$ROOT/Cargo.toml"
  BIN="$ROOT/target/$TARGET/release/photonic"
else
  cargo build -p photonic-app --release --manifest-path "$ROOT/Cargo.toml"
  BIN="$ROOT/target/release/photonic"
fi
cp "$BIN" "$STAGE/server/photonic"
chmod +x "$STAGE/server/photonic"
# manifest with version substituted
python3 - <<PY
import json, pathlib
tpl = pathlib.Path("$ROOT/packaging/mcpb/manifest.template.json")
data = json.loads(tpl.read_text())
data["version"] = "$VERSION"
pathlib.Path("$STAGE/manifest.json").write_text(json.dumps(data, indent=2) + "\n")
PY
# zip as .mcpb
mkdir -p "$OUT"
( cd "$STAGE" && zip -qr "$OUT/${NAME}.mcpb" manifest.json server )
echo "Wrote $OUT/${NAME}.mcpb"
# optional validate if npx available
if command -v npx >/dev/null 2>&1; then
  npx --yes @anthropic-ai/mcpb validate "$OUT/${NAME}.mcpb" 2>/dev/null || \
    echo "(mcpb validate skipped or failed — install @anthropic-ai/mcpb if needed)"
fi
ls -la "$OUT/${NAME}.mcpb"
