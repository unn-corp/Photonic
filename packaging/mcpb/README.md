# Photonic MCPB packaging

Build a **binary-only** [MCP Bundle](https://github.com/modelcontextprotocol/mcpb) (`.mcpb`) for local desktop hosts.

See **[docs/specs/mcp-2026-07-28.md](../../docs/specs/mcp-2026-07-28.md)** for protocol and security.

```bash
# Debug smoke (structural zip + manifest checks)
./scripts/pack-debug.sh

# Release binary pack
./scripts/pack.sh
# optional: npx @anthropic-ai/mcpb validate dist/*.mcpb
```

Output is under `dist/` (gitignored). The host launches `server/photonic --mcp-stdio`.
