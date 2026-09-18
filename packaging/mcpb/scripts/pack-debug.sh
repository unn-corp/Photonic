#!/usr/bin/env bash
# Fast pack using debug binary (dev/CI smoke only — not for distribution).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
OUT="$ROOT/packaging/mcpb/dist"
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)"/\1/')"
NAME="photonic-${VERSION}-debug-local"
STAGE="$OUT/$NAME"
mkdir -p "$STAGE/server" "$OUT"
cargo build -p photonic-app --manifest-path "$ROOT/Cargo.toml"
cp "$ROOT/target/debug/photonic" "$STAGE/server/photonic"
chmod +x "$STAGE/server/photonic"
python3 - <<PY
import json, pathlib
tpl = pathlib.Path("$ROOT/packaging/mcpb/manifest.template.json")
data = json.loads(tpl.read_text())
data["version"] = "$VERSION"
pathlib.Path("$STAGE/manifest.json").write_text(json.dumps(data, indent=2) + "\n")
PY
( cd "$STAGE" && zip -qr "$OUT/${NAME}.mcpb" manifest.json server )
echo "Wrote $OUT/${NAME}.mcpb"
unzip -l "$OUT/${NAME}.mcpb"
# structural checks
python3 - <<PY
import zipfile, json, sys
z=zipfile.ZipFile("$OUT/${NAME}.mcpb")
names=set(z.namelist())
assert "manifest.json" in names, names
assert any(n.startswith("server/photonic") for n in names), names
assert not any("node_modules" in n for n in names)
m=json.loads(z.read("manifest.json"))
assert m["server"]["type"]=="binary"
assert "--mcp-stdio" in m["server"]["mcp_config"]["args"]
print("structural MCPB checks OK")
PY
