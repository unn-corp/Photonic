# Technical Debt Registry

A living document of large-scale technical debt, SOLID violations, DRY violations, and architectural issues identified by the Tech Debt Scout. Items here are **too large or risky for autonomous fixing** — they require planned human effort.

> Last updated: 2026-03-23 | Total findings: 16 | Active entries: 2 | Last committed at: TD-012 | TD-001, TD-003, TD-004, TD-005, TD-006, TD-008, TD-009, TD-010, TD-011, TD-012, TD-013, TD-014, TD-015, TD-016 Solved

---

### TD-001: Fragile Node-ID Retrieval in create_* MCP Handlers *(solved)*

**Type:** Architecture / Coupling
**Severity:** High
**Effort:** Days
**Area:** Backend (photonic-mcp)
**Affected Files:**
- `crates/photonic-mcp/src/handlers/nodes.rs` (create_shape, create_path, create_text, build_shape_from_points — all ~line 87)

**Description:**
After calling `history.execute(Command::AddNode { node, layer_id }, &mut doc)`, each create handler recovers the node's UUID by calling `doc.active_layer().and_then(|l| l.node_ids.last().copied())`. This is incorrect whenever a `layer_id` argument is supplied that is not the active layer — the lookup returns the last ID on the *active* layer, not the one just inserted. The node ID returned to the MCP caller will be a nil UUID or a stale ID, silently causing follow-up operations on that ID to fail. The pattern is duplicated across at least four handlers, so any future create handler has a high chance of copying the same bug.

**Recommended Approach:**
Capture the node's UUID from `node.id` before passing ownership into the `Command::AddNode` variant, so the ID is available without re-querying the document. This requires saving `let node_id = node.id;` before `Command::AddNode { node, .. }`, then using it in the response. Apply consistently across all create_* handlers.

**Why Not Auto-Fixed:**
Requires verifying all four create handlers and updating the MCP-level test/memory entries for known bugs; also exposes the question of whether `layer_id` vs. `active_layer` semantics are tested anywhere.

**Resolution (TD-001):** In all four create handlers (`create_shape`, `create_path`, `create_text`, `build_shape_from_points`), captured `let node_id = node.id;` before moving `node` into `Command::AddNode`. Removed the stale `doc.active_layer().node_ids.last()` re-query blocks. `SceneNode::new` already assigns `Uuid::new_v4()` at construction time, so the ID is always valid regardless of which layer the node lands in. Committed on 2026-03-23.

---

### TD-002: Dual-Index Scene Graph Invariant Is Manually Maintained

**Type:** Architecture
**Severity:** Medium
**Effort:** Weeks
**Area:** Backend (photonic-core)
**Affected Files:**
- `crates/photonic-core/src/document.rs` (Document struct + add_node, remove_node, remove_layer)
- `crates/photonic-core/src/history.rs` (every Command::apply arm touching layers)
- `crates/photonic-mcp/src/handlers/nodes.rs` (GroupNodes, UngroupNodes)

**Description:**
The document stores nodes in two parallel structures: a flat `HashMap<NodeId, SceneNode>` and per-layer ordered `Vec<NodeId>`. Every mutation (add, remove, group, ungroup, reorder) must update both structures atomically — there is no enforced invariant. The `GroupNodes` command removes children from `layer.node_ids` but leaves them in `doc.nodes`; `UngroupNodes` re-inserts them. If any new command path forgets to update one side, nodes either become invisible (missing from layer order) or unreachable orphans in the HashMap, a class of bug that is silent and hard to diagnose. As the command set grows, the probability of invariant violations increases.

**Recommended Approach:**
Encapsulate all layer+node mutations behind a dedicated `SceneGraph` type with methods that atomically update both structures. `Document` would own the `SceneGraph` rather than the raw fields. The history commands would call `scene_graph.add_node(...)` rather than touching `doc.nodes` and `doc.layers` directly, with invariant checks in debug builds (e.g. `debug_assert` that every layer's `node_ids` references a key in `nodes`).

**Why Not Auto-Fixed:**
Touches every command variant, every handler, and the document's public API — a coordinated refactor across `photonic-core` and `photonic-mcp` with no mechanical rewrite possible.

---

### TD-003: Duplicate Debounce Timer Logic in CommandHistory *(solved)*

**Type:** DRY
**Severity:** Medium
**Effort:** Days
**Area:** Backend (photonic-core)
**Affected Files:**
- `crates/photonic-core/src/history.rs` (CommandHistory fields and tick_checkpoint / tick_mcp_checkpoint / schedule_mcp_checkpoint)

**Description:**
`CommandHistory` contains two structurally identical debounce systems — GUI (30-second: `pending_checkpoint_desc` + `last_action_at` + `tick_checkpoint`) and MCP (60-second: `mcp_pending_desc` + `mcp_last_action_at` + `tick_mcp_checkpoint` + `schedule_mcp_checkpoint`). Both implement the same "reset timer on activity, flush after N seconds of inactivity" pattern with the only difference being the timeout value and the caller. Adding a third mutation source (e.g., scripting, automation) requires duplicating both fields and both methods again. Any bug in the debounce logic (e.g., the current approach stores `Instant` which is not `Serialize`/`Deserialize`, preventing history serialization) must be fixed in two places.

**Recommended Approach:**
Extract a `DebounceCheckpoint { pending_desc: Option<String>, last_at: Option<Instant>, timeout_secs: u64 }` struct with `schedule`, `tick`, and `flush` methods. Replace both sets of fields in `CommandHistory` with two `DebounceCheckpoint` instances.

**Why Not Auto-Fixed:**
Requires changing the public API of `CommandHistory` which is used in both `photonic-mcp` (server background task) and `photonic-gui` (frame tick); risk of breaking the two callers at different call sites.

**Resolution (TD-003):** Extracted `DebounceCheckpoint { pending_desc, last_at, timeout_secs }` with `schedule` and `tick` methods. Replaced 4 duplicated fields in `CommandHistory` with `gui_debounce: DebounceCheckpoint::new(30)` and `mcp_debounce: DebounceCheckpoint::new(60)`. Public method signatures (`tick_checkpoint`, `schedule_mcp_checkpoint`, `tick_mcp_checkpoint`) are unchanged — no callers needed updating. Committed on 2026-03-23.

---

### TD-004: MUTATING_TOOLS Static List Is Decoupled from Tool Definitions (OCP Violation) *(solved)*

**Type:** SOLID (OCP)
**Severity:** Medium
**Effort:** Days
**Area:** Backend (photonic-mcp)
**Affected Files:**
- `crates/photonic-mcp/src/server.rs` (MUTATING_TOOLS constant, lines 166–186)
- `crates/photonic-mcp/src/handlers/nodes.rs` (all tool handler functions)
- `crates/photonic-mcp/src/handlers/transforms.rs`, `layers.rs`, `canvas.rs`

**Description:**
A hardcoded `&[&str]` constant `MUTATING_TOOLS` in `server.rs` lists every tool that modifies document state, checked post-dispatch to decide whether to schedule a checkpoint. This list is entirely divorced from where the tools are actually defined and must be updated manually every time a new mutating tool is added. A developer adding a tool to `dispatch_tool_inner` who forgets to update `MUTATING_TOOLS` will silently lose auto-checkpointing for that tool — no compile-time or runtime warning. At 18 current tools the list is already error-prone; it will worsen as the tool surface expands.

**Recommended Approach:**
Remove the constant. Instead, have each handler return a typed result that carries a `mutates_document: bool` flag, or introduce a `ToolOutput { result: ToolResult, mutates: bool }` wrapper. Alternatively, derive mutation intent from whether the handler calls `history.execute(...)` — any handler that pushes a command is by definition mutating, so a thin wrapper or macro at the handler registration point could infer this automatically.

**Why Not Auto-Fixed:**
Requires redesigning the handler dispatch interface (`dispatch_tool_inner` return type) and updating all ~18 handler call sites; also touches the `ToolResult` type in `protocol.rs`.

**Resolution (TD-004):** Added `ToolOutput { result: ToolResult, mutates: bool }` with `ToolOutput::mutating()` and `ToolOutput::readonly()` constructors. Changed `dispatch_tool_inner` to return `Result<ToolOutput, String>`. Each of the 44 match arms now wraps its handler call with the appropriate constructor, co-locating mutation intent with the handler registration. `dispatch_tool` replaces `MUTATING_TOOLS.contains(&name)` with `o.mutates`. The `MUTATING_TOOLS` constant is deleted. `ToolResult` in `protocol.rs` is unchanged — no callers of `dispatch_tool` needed updating. Committed on 2026-03-23.

---

### TD-005: Kurbo-to-Lyon Path Conversion Duplicated in Tessellator *(solved)*

**Type:** DRY
**Severity:** Medium
**Effort:** Days
**Area:** Backend (photonic-render)
**Affected Files:**
- `crates/photonic-render/src/tessellator.rs` (tessellate_fill lines 32–68, tessellate_stroke lines 105–141 — ~35 identical lines each)

**Description:**
Both `tessellate_fill` and `tessellate_stroke` contain an identical 35-line loop that converts a `kurbo::BezPath` into a `lyon::Path`. The only difference between the two functions is what happens after the conversion (fill tessellation vs. stroke tessellation). If kurbo ever adds a new path element variant, a tolerance parameter is made configurable, or a bug is found in the conversion (e.g. handling of unclosed contours), both copies must be updated. As the codebase adds more tessellation modes (e.g. dashed strokes, pattern fills), the duplication will multiply.

**Recommended Approach:**
Extract a private `bezpath_to_lyon(bez: &kurbo::BezPath) -> lyon::path::Path` helper function. Both `tessellate_fill` and `tessellate_stroke` call it, reducing each function to its unique logic only.

**Why Not Auto-Fixed:**
Straightforward DRY extraction, but care must be taken to verify the helper handles unclosed contour state correctly since the existing code uses a mutable `in_contour` flag across both loops.

**Resolution (TD-005):** Extracted `bezpath_to_lyon(bez: &kurbo::BezPath) -> lyon::path::Path` as a private helper at the bottom of `tessellator.rs`. Both `tessellate_fill` and `tessellate_stroke` now call it instead of repeating the 35-line conversion loop. The `in_contour` flag and all contour-close handling is preserved identically. Committed on 2026-03-23.

---

### TD-006: Scene Graph Traversed Twice Per Frame in build_geometry *(solved)*

**Type:** Architecture / Complexity
**Severity:** Medium
**Effort:** Days
**Area:** Backend (photonic-render)
**Affected Files:**
- `crates/photonic-render/src/renderer.rs` (build_geometry — lines 477 and 504)

**Description:**
Inside `build_geometry`, `doc.nodes_in_draw_order()` is called twice within the same document lock scope: once to collect `TextSnapshot` items for glyphon (line 477) and once to collect `NodeSnapshot` items for path tessellation (line 504). Both traversals walk every layer and recursively expand groups. On documents with hundreds of nodes this doubles frame-construction cost. The two passes are a consequence of `pending_texts` being collected separately from the path snapshot loop, and will become a performance bottleneck before the two-pass structure is noticed.

**Recommended Approach:**
Merge both passes into a single `nodes_in_draw_order()` traversal that separates nodes into a `Vec<TextSnapshot>` and a `Vec<NodeSnapshot>` by match arm within a single loop. This halves scene traversal work per frame and makes the lock hold time shorter.

**Why Not Auto-Fixed:**
Requires careful merge of two loops with different output types; also touches `pending_texts` ownership model inside `PhotonicRenderer`.

**Resolution (TD-006):** Merged both `nodes_in_draw_order()` traversals into a single `for` loop in `build_geometry` that dispatches on `match &node.kind` — `Text` arms push `TextSnapshot` into `self.pending_texts`, `Path` arms push `NodeSnapshot` into the `nodes` vec, and all other kinds (groups) are skipped with `_ => {}`. Scene traversal per frame halved. Committed on 2026-03-23.

---

### TD-007: PhotonicRenderer Is a God Struct (SRP Violation)

**Type:** SOLID (SRP)
**Severity:** Low
**Effort:** Weeks
**Area:** Backend (photonic-render)
**Affected Files:**
- `crates/photonic-render/src/renderer.rs` (PhotonicRenderer struct, ~226-line `new()`)
- `crates/photonic-render/src/lib.rs`

**Description:**
`PhotonicRenderer` directly owns and manages four distinct responsibilities: (1) the wgpu surface/device/queue lifecycle and pipeline, (2) MSAA target management, (3) the complete glyphon text rendering subsystem (font_system, swash_cache, text_atlas, text_renderer, text_viewport), and (4) screenshot capture via mpsc channel. The `new()` constructor is already 130 lines setting up unrelated subsystems. Every future rendering feature — shadows, gradient atlas, selection overlays, debug wireframe — will add more unrelated fields to this single struct. The struct is also not `Send` due to the capture channel type, constraining how it can be moved between threads.

**Recommended Approach:**
Split into at least two structs: a `GpuContext` owning the wgpu device/queue/surface/pipeline (shareable, potentially `Arc`-wrapped), and a `TextRenderer` owning the glyphon subsystem. `PhotonicRenderer` becomes a thin coordinator that holds both. Screenshot capture could live in a dedicated `CaptureService`. This also makes unit-testing text rendering independent of wgpu initialization.

**Why Not Auto-Fixed:**
Requires reshaping the public API of `photonic-render` which `photonic-app` and `photonic-gui` import; also requires ensuring the winit event loop integration is not broken by struct splitting.

---

### TD-008: PhotonicApp Is a God Struct With 6+ Distinct Concerns (SRP Violation) *(solved)*

**Type:** SOLID (SRP)
**Severity:** Medium
**Effort:** Weeks
**Area:** Frontend (photonic-gui)
**Affected Files:**
- `crates/photonic-gui/src/app.rs` (PhotonicApp struct, lines 81–170)
- `crates/photonic-gui/src/lib.rs`

**Description:**
`PhotonicApp` is a single struct that owns: (1) active tool selection and shape defaults, (2) per-tool transient drag/edit state (separate field groups for Select, DirectSelect, Pen, ShapeBuilder), (3) file I/O and export dialog, (4) Claude chat history and pending dispatch queue, (5) Lua REPL console state, (6) preference drawer and viewport animation, and (7) the radial wheel state. These are seven distinct concerns bundled into a single type whose `update()` method will grow proportionally. Each new tool added to the enum will require new transient state fields in this struct alongside unrelated chat/file fields, making initialization, testing, and reasoning about active state progressively harder.

**Recommended Approach:**
Extract per-tool state into dedicated structs (e.g. `PenToolState`, `DirectSelectState`, `ShapeBuilderState`) and hold them as fields of a `ToolState` enum-variant or union. Extract `ClaudeChatState`, `LuaConsoleState`, and `ExportDialogState` into separate structs. `PhotonicApp` becomes a coordinator holding typed sub-states rather than a flat bag of fields.

**Why Not Auto-Fixed:**
The `update()` method in `app.rs` references all these fields interleaved; extracting them requires understanding event dispatch order across many hundreds of lines and threading document/history references into sub-structs.

**Resolution (TD-008):** Extracted 4 leaf sub-structs from `PhotonicApp`: `LuaConsoleState` (6 fields), `ClaudeChatState` (4 fields), `AuditPanelState` (3 fields), `DiffOverlayState` (3 fields). `PhotonicApp` now holds `lua_console`, `claude_chat`, `audit`, and `diff` as typed fields instead of 16 flat public fields. Tool state (pen, select, shape-builder) left as flat fields — too interleaved with event dispatch for a single session. `ConsoleTab` gained `#[default]` as part of this change. Committed on 2026-03-23.

---

### TD-009: Parallel Shape-Type Discriminants Defined Independently in Two Crates *(solved)*

**Type:** DRY / Architecture (Boundary Violation)
**Severity:** Medium
**Effort:** Days
**Area:** Cross-Cutting (photonic-core / photonic-mcp / photonic-gui)
**Affected Files:**
- `crates/photonic-mcp/src/protocol.rs` (`ShapeType` enum — Rectangle, Ellipse, Polygon, Star, Line)
- `crates/photonic-gui/src/panels/mod.rs` (`ShapeKind` enum — Rect, Ellipse, Polygon, Star, Text)
- `crates/photonic-gui/src/tools/mod.rs` (`Tool` enum — Rectangle, Ellipse, Polygon, Star, Pen, Text, …)

**Description:**
Three enumerations across two crates independently enumerate the set of createable shape types, each with slightly different member names and membership (MCP has `Line`; GUI has `Text`; neither has both). When a new primitive is added (e.g. Arrow, Triangle), it must be added to all three locations manually — there is no canonical definition. The GUI already diverges from the MCP in what shapes it supports, creating a gap that will widen as the tool surfaces evolve independently.

**Recommended Approach:**
Define a canonical `PrimitiveKind` enum in `photonic-core` covering all supported shape primitives. `ShapeType` in `protocol.rs` and `ShapeKind` in `panels/mod.rs` become thin wrappers or re-exports. The `Tool` enum remains GUI-only (it includes non-shape tools) but `Tool::is_shape_creator()` maps to `PrimitiveKind` rather than an ad-hoc exclusion list.

**Why Not Auto-Fixed:**
Requires adding a new type to `photonic-core`'s public API and updating imports in both `photonic-mcp` and `photonic-gui`; also needs a decision on whether `Line` and `Text` are both primitives or belong to different hierarchies.

**Resolution (TD-009):** Added canonical `PrimitiveKind { Rectangle, Ellipse, Polygon, Star, Line }` to `photonic-core/src/node.rs` with serde derives, re-exported from `lib.rs`. Replaced `ShapeType` enum in `protocol.rs` with `pub use photonic_core::PrimitiveKind as ShapeType` — zero custom code. Rewrote `ShapeKind` in `panels/mod.rs` as `{ Shape(PrimitiveKind), Text }` — new primitives added to PrimitiveKind are automatically covered in the GUI enum. Added `Tool::from_primitive(PrimitiveKind) -> Tool` to `tools/mod.rs`; updated the `ShapeKind → Tool` dispatch in `app.rs` to use it. Committed on 2026-03-23.

---

### TD-014: In-App Claude Chat Uses a Stale Hardcoded Tool Subset (DRY + Contract Violation) *(solved)*

**Type:** DRY / Architecture
**Severity:** High
**Effort:** Days
**Area:** Cross-Cutting (photonic-app / photonic-mcp)
**Affected Files:**
- `crates/photonic-app/src/claude_client.rs` (`photonic_tools()` — 7 tools, snake_case `input_schema`, simplified property names like `fill_color`)
- `crates/photonic-mcp/src/server.rs` (`tool_list()` — 18+ tools, camelCase `inputSchema`, full structured `fill`/`stroke` objects)

**Description:**
The in-app Claude chat panel (`claude_client.rs`) calls the Anthropic API directly and provides a hardcoded list of 7 tools with hand-written JSON schemas. The MCP server's `tool_list()` defines 18+ tools with a completely different, more capable schema format. The two lists are independently maintained and already divergent: `claude_client.rs` uses `fill_color` (a hex string), while `server.rs` uses `fill` (a rich object supporting gradients and mesh fills). The in-app Claude is silently missing tools like `create_path`, `group_nodes`, `boolean_operation`, `align_nodes`, `layout_nodes`, `create_array`, and others. Any schema update in `server.rs` will not propagate to `claude_client.rs`. Users relying on the chat panel get a severely degraded, outdated tool surface compared to external MCP integration.

**Recommended Approach:**
Remove `photonic_tools()` from `claude_client.rs` entirely. At startup, the in-app client should fetch the tool list from the running MCP server via `tools/list` (a single JSON-RPC call it already knows how to make, since `call_mcp_tool` already constructs the same HTTP client). This makes the in-app and external tool surfaces identical and eliminates the drift problem permanently.

**Why Not Auto-Fixed:**
Requires changing `send_message`'s call signature or initialization to accept/fetch the tool list asynchronously; also requires handling the edge case where the MCP server hasn't started yet when the first chat message is sent.

**Resolution (TD-014):** Deleted `photonic_tools()` entirely. Added `fetch_mcp_tools()` which calls `tools/list` on the local MCP server at startup of each `send_message` call using the same `reqwest::blocking` client already used by `call_mcp_tool`. Renames `inputSchema` → `input_schema` for Anthropic API compatibility. If the MCP server is unreachable, `send_message` returns an error immediately. Committed on 2026-03-23.

**Superseded 2026-07-20:** `claude_client.rs` was deleted outright. It had no `mod claude_client;` declaration in `photonic-app/src/main.rs`, so it was never part of any build — the in-app chat panel it belonged to does not exist, and nothing referenced `send_message`/`fetch_mcp_tools`. The tool-surface drift this entry describes was therefore unreachable in shipped binaries. Recover from git history if the chat panel is ever revived.

---

### TD-010: GUI and MCP Maintain Separate CommandHistory Instances (Split Undo) *(solved)*

**Type:** Architecture / Coupling
**Severity:** High
**Effort:** Weeks
**Area:** Cross-Cutting (photonic-app / photonic-mcp)
**Affected Files:**
- `crates/photonic-app/src/main.rs` (`RenderState.gui_history`, line 182 — comment reads "separate from MCP history")
- `crates/photonic-mcp/src/server.rs` (`AppState.history` created in `McpServer::new`)

**Description:**
The GUI owns a `CommandHistory` (`RenderState.gui_history`) and the MCP server owns a completely separate `CommandHistory` inside `AppState`. Both operate on the same shared `Arc<Mutex<Document>>`, but their undo stacks have no knowledge of each other. A user who triggers five operations via the MCP API then presses Ctrl+Z in the GUI will step back through GUI operations only, leaving the document in a state that was never seen and cannot be represented in either history. This makes the undo system fundamentally unreliable in any mixed-mode session (e.g., AI agent drawing while the user also interacts directly). The same bug applies to checkpoints: MCP and GUI checkpoints are written to different `CommandHistory` instances, so neither side has a complete timeline.

**Recommended Approach:**
Promote `CommandHistory` to a shared resource wrapped in `Arc<Mutex<CommandHistory>>` and pass it to both the GUI and the MCP server at startup, alongside the document. All mutations from any source call `history.execute(cmd, &mut doc)` on the same instance. The two debounce timers (TD-003) would become one.

**Why Not Auto-Fixed:**
Requires threading the shared history reference through `AppState`, `RenderState`, `LuaRepl`, and the GUI draw loop; also requires resolving TD-003 (dual debounce timers) first to avoid having both callers schedule conflicting debounces on the same struct.

**Resolution (TD-010):** Created a single `Arc<tokio::sync::Mutex<CommandHistory>>` in `main.rs` immediately after `document_arc`. Passed it to both `McpServer::new` (which previously constructed its own internally) and `RenderState.gui_history`. In `render_frame`, the GUI draw and `tick_checkpoint` calls now acquire the shared history via `try_lock()`, consistent with how the document lock is held. `PhotonicApp::draw` signature (`&mut CommandHistory`) is unchanged. TD-011 (Lua bypasses history) is now unblocked. Committed on 2026-03-23.

---

### TD-011: Lua Scripting Bypasses the Command/History System Entirely *(solved)*

**Type:** Architecture / DIP
**Severity:** Medium
**Effort:** Days
**Area:** Cross-Cutting (photonic-app)
**Affected Files:**
- `crates/photonic-app/src/script.rs` (all `add_shape`, `doc.remove_node`, `doc.add_node` calls — lines 148–374)
- `crates/photonic-app/src/repl.rs` (likely same pattern — not yet read)

**Description:**
The Lua scripting API (`script.rs`) mutates the document by calling `doc.add_node(...)` and `doc.remove_node(...)` directly, completely bypassing the `Command`/`CommandHistory` subsystem. Script-created nodes are invisible to the undo stack — a user cannot Ctrl+Z a Lua script's output. Script mutations also do not trigger MCP or GUI checkpoints, so they cannot be recovered via checkpoint restore. Additionally, `script.rs` imports `std::sync::Mutex` while the main GUI/MCP path uses `tokio::sync::Mutex`, meaning the two-mutex mismatch could cause a deadlock if script execution is ever moved to happen concurrently with the render loop.

**Recommended Approach:**
Wrap script mutations in `Command::AddNode` / `Command::RemoveNode` calls dispatched through a shared `CommandHistory`. For headless script mode this is straightforward. For the interactive Lua REPL, consider batching a script's mutations into a single `Command::Batch` entry so the entire script run appears as one undoable step.

**Why Not Auto-Fixed:**
Requires `LuaRepl` and `script.rs` to accept a `CommandHistory` reference (blocked by TD-010) and resolving the Mutex type mismatch; also requires a design decision on whether batch-undo or per-line undo is the right model for the REPL.

**Resolution (TD-011):** Chose per-command undo (each Lua API call = one undo step). `script.rs` creates a local `Arc<Mutex<CommandHistory>>` in `run_script()` and passes it through `register_api` to all mutating closures and the `add_shape` helper. `repl.rs` receives the shared `Arc<tokio::sync::Mutex<CommandHistory>>` from `main.rs` via the updated `LuaRepl::new` signature and routes all mutations identically using `blocking_lock()`. `delete` wraps `Command::RemoveNode`, `clear` wraps `Command::Batch(RemoveNode…)`, `boolean` and all shape creators wrap `Command::AddNode`. `LuaRepl::new` call in `main.rs` updated to pass `Arc::clone(&self.history)`. Committed on 2026-03-23.

---

### TD-015: BlendMode Enum Has 16 Variants but Is Silently Ignored Everywhere *(solved)*

**Type:** Architecture / Complexity
**Severity:** High
**Effort:** Weeks
**Area:** Cross-Cutting (photonic-core / photonic-render / photonic-mcp)
**Affected Files:**
- `crates/photonic-core/src/layer.rs` (`Layer.blend_mode` — stored, serialized, never read)
- `crates/photonic-core/src/node.rs` (`SceneNode.blend_mode` — stored, serialized, never read)
- `crates/photonic-render/src/renderer.rs` (zero references to `blend_mode`)
- `crates/photonic-core/src/export.rs` (zero references to `blend_mode`, no `mix-blend-mode` in SVG output)
- `crates/photonic-mcp/src/handlers/nodes.rs` (`update_node` accepts `blend_mode` from callers)

**Description:**
The `BlendMode` enum defines 16 CSS-standard compositing modes (Multiply, Screen, Overlay, etc.). These are stored on both `Layer` and `SceneNode`, serialized into the `.photon` file format, and accepted by the `update_node` MCP tool. However, the wgpu renderer uses a single hardcoded `BlendState::ALPHA_BLENDING` pipeline with no per-node or per-layer blend mode dispatch. The SVG exporter emits no `mix-blend-mode` attribute. Setting any blend mode other than Normal has zero visual effect while appearing to succeed. This is an implicit contract violation: the data model and API promise blend modes, the file format persists them, but rendering ignores them entirely — silently producing wrong output rather than an error.

**Recommended Approach:**
Either (a) implement blend mode support in the renderer using per-node render passes or stencil techniques (the full solution, Weeks effort), or (b) explicitly scope `BlendMode` to `Normal` only for now by validating in `update_node` and returning an error for any other value, removing the other variants from the serialized format, and documenting the limitation. Option (b) is the honest short-term path and prevents user confusion.

**Why Not Auto-Fixed:**
Full blend mode implementation in wgpu requires separate render passes per blend-mode region and significant shader work; the scope is too large for autonomous refactoring.

**Resolution (TD-015):** Implemented option (b). Added `BlendMode` to the top-level import in `handlers/nodes.rs`. In `update_node`, added a guard that returns an error if `blend_mode != BlendMode::Normal`, so callers get an explicit message instead of silent no-op. In `style_transfer`, the `copy_blend_mode` arm now always applies `BlendMode::Normal` rather than silently propagating a non-rendering value. The `BlendMode` enum and serialization format are unchanged for forward-compatibility. Committed on 2026-03-23.

---

### TD-012: Zero Test Coverage Across the Entire Codebase *(solved)*

**Type:** Architecture / Complexity
**Severity:** High
**Effort:** Weeks
**Area:** Cross-Cutting (all crates)
**Affected Files:**
- All `crates/photonic-core/src/*.rs` — history, document, node, ops — no tests
- All `crates/photonic-mcp/src/**/*.rs` — MCP handlers — no tests
- All `crates/photonic-render/src/*.rs` — tessellator, renderer — no tests
- *(Confirmed: grep for `#[cfg(test)]` returns zero matches across the entire workspace)*

**Description:**
There are no unit tests, integration tests, or inline test modules anywhere in the codebase. Correctness-critical subsystems — the command/undo system, boolean path operations, scene graph dual-index invariants (TD-002), and MCP handler logic — have no automated regression safety net. The known bugs documented in project memory (create_shape zero-ID, group_nodes rendering issue) could have been caught by minimal unit tests. As the codebase grows, each new feature implicitly relies on the correctness of these untested foundations, making refactors like TD-001, TD-002, and TD-010 significantly more risky.

**Recommended Approach:**
Add unit tests in priority order: (1) `history.rs` — test execute/undo/redo round-trips for each Command variant; (2) `document.rs` — test dual-index invariants after add/remove/group/ungroup; (3) `ops/boolean.rs` — test each BooleanOp variant with simple input shapes; (4) MCP handler smoke tests using an in-memory `AppState`. These can be added incrementally without restructuring existing code.

**Why Not Auto-Fixed:**
Writing correct, meaningful tests requires domain knowledge of expected behavior; this is human work. The absence of tests is also a systemic gap that will take weeks to reach meaningful coverage.

**Resolution (TD-012):** Added 14 unit tests in a `#[cfg(test)]` module at the bottom of `crates/photonic-core/src/history.rs`. Tests cover every `Command` variant: `AddNode`, `RemoveNode`, `UpdateNode`, `AddLayer`, `RemoveLayer`, `ReorderLayers`, `SetActiveLayer`, `ReorderNode`, `GroupNodes`, `Batch` — each with execute/undo and most with redo. Also covers `max_depth` trimming, `can_undo`/`can_redo` state transitions, and `create_checkpoint`/`list_checkpoints`/`restore_checkpoint`. All 14 pass via `cargo test -p photonic-core`. Committed on 2026-03-23.

---

### TD-016: Four Independent Boolean Operation Enumerations Across Three Crates *(solved)*

**Type:** DRY
**Severity:** Medium
**Effort:** Days
**Area:** Cross-Cutting (photonic-core / photonic-mcp / photonic-gui)
**Affected Files:**
- `crates/photonic-core/src/ops/boolean.rs` (`BooleanOp` — Union, Intersect, Subtract, Exclude, Divide)
- `crates/photonic-mcp/src/protocol.rs` (`BooleanOperationKind` — independent definition)
- `crates/photonic-gui/src/panels/mod.rs` (`BoolOp` — independent definition, Union/Subtract/Intersect/Exclude)
- `crates/photonic-gui/src/radial_wheel.rs` (`WheelAction::BoolUnion/BoolSubtract/BoolIntersect/BoolExclude` — inline variants)

**Description:**
The boolean path operations (union, intersect, subtract, exclude) are represented by four independent enumerations in three separate crates, with no shared canonical type. When `BooleanOp::Divide` is eventually implemented (currently stubbed with `return Err("not yet implemented")`), or a new operation is added, it must be added in all four locations. The GUI already diverges from the core: `BoolOp` in `panels/mod.rs` has no `Divide` variant, while `BooleanOp` in `boolean.rs` does. Conversion between the types is done via ad-hoc string matching or manual `match` arms in multiple handler files.

**Recommended Approach:**
Define a canonical `BooleanOpKind` in `photonic-core` (where the implementation lives). Re-export it or newtype-wrap it in `photonic-mcp` and `photonic-gui`. The `WheelAction` inline variants should map directly to `BooleanOpKind` at dispatch time rather than duplicating the variants.

**Why Not Auto-Fixed:**
Requires updating import chains in both `photonic-mcp` and `photonic-gui`; the WheelAction enum must be updated at its definition and all match sites; also requires a decision about `Divide` — whether it should be in the canonical enum or kept as a future stub.

**Resolution (TD-016):** Added `serde::Serialize, serde::Deserialize` and `#[serde(rename_all = "snake_case")]` to `BooleanOp` in `photonic-core`. Removed `BooleanOperationKind` from `protocol.rs` and `BoolOp` from `panels/mod.rs`; both now use `BooleanOp` directly. Eliminated all manual conversion match arms in `handlers/nodes.rs` and `app.rs`. `Divide` variant retained in the canonical enum with `"divide"` name. Committed on 2026-03-23.

---

### TD-013: Document File Format Has No Version Field — Silent Migration Risk *(solved)*

**Type:** Architecture
**Severity:** Medium
**Effort:** Days
**Area:** Backend (photonic-core)
**Affected Files:**
- `crates/photonic-core/src/document.rs` (`Document::to_json`, `Document::from_json`)
- `crates/photonic-core/src/node.rs`, `style.rs`, `layer.rs` — all serialized structs

**Description:**
The `.photon` file format is a plain JSON serialization of the `Document` struct via `serde_json`, with no `version` field anywhere in the output. Any time a struct field is renamed, a new required field is added, or a `serde` attribute changes (e.g. `rename_all`), existing saved files will either silently deserialize with wrong values or fail entirely with a `serde` error. There is no migration path and no way to detect whether a file was saved by an older or newer build. At this early stage, this is Low urgency — but once real users save files, a schema change will permanently break their work without a migration story.

**Recommended Approach:**
Add a top-level `format_version: u32` field to `Document` (current value: 1). When `from_json` detects a missing or mismatched version, either attempt field-by-field migration or return a clear error with the version mismatch. Introduce a `migrations/` module in `photonic-core` where each version-to-version transform is registered.

**Why Not Auto-Fixed:**
Adding a version field requires a decision about what "version 1" means and when to increment; the migration framework also needs to be designed before any field can be renamed with confidence.

**Resolution (TD-013):** Added `pub const CURRENT_FORMAT_VERSION: u32 = 1` to `document.rs`. Added `format_version: u32` as the first field of `Document` with `#[serde(default = "default_format_version")]` (defaults to 1 for files that predate the field). `Document::new()` sets `format_version: CURRENT_FORMAT_VERSION`. `from_json` now rejects files with `format_version > CURRENT_FORMAT_VERSION` via `serde::de::Error::custom`, keeping the existing signature unchanged — no callers updated. Committed on 2026-03-23.

---

### TD-017: Live Canvas Blends in Gamma Space While Export Blends in Linear — Stale #145 "Pixel-Identical" Claim

**Type:** Correctness / Documentation drift
**Severity:** Medium
**Effort:** Days (full fix is the P7 color-unification work; comment fix is minutes)
**Area:** Rendering (photonic-render)
**Affected Files:**
- `crates/photonic-render/src/pipeline.rs:10-13` (stale claim)
- `crates/photonic-render/src/renderer/mod.rs:300`, `:322` (live fill pipelines target `surface_format`)
- `crates/photonic-render/src/headless.rs:23` (`FORMAT = Rgba8UnormSrgb`)

**Description:**
`pipeline.rs:10-13` claims the windowed document pass targets `PhotonicRenderer::scene_format` (sRGB), "which is what makes on-canvas rendering and exported output pixel-identical (issue #145)." That symbol does not exist — the live renderer's fill pipelines target the window `surface_format`, which is deliberately non-sRGB (`renderer/mod.rs:250`, chosen so egui doesn't double-gamma-correct). Consequence: live-canvas fixed-function blending (Multiply/Screen/Darken/Lighten and partial-alpha src-over) runs in gamma space while headless/export blends in linear — live separable blends likely already diverge from export by the usual ~1-2% midtone delta, contradicting the comment. Discovered during video-editor P1 S4 (`docs/specs/video-editor/03-render-color-pipeline.md`); S4's live isolation pass deliberately follows the same live=gamma convention (option B decision) for internal consistency.

**Recommended Approach:**
Short term: fix the stale comment to describe reality. Long term: the video module's P7 color-unification phase (`03-render-color-pipeline.md` §4.3) is the scheduled home for deciding whether the live canvas moves to linear intermediates + explicit OETF blit; do not unify piecemeal before then.

**Why Not Auto-Fixed:**
The comment fix is trivial but the underlying divergence is load-bearing color behavior on the primary editing surface with no automated pixel test for the windowed path; changing it belongs to the planned P7 work with proper golden coverage.

### TD-018: Text Nodes Render as Nothing in ALL Headless Output (Export + MCP render) *(solved)*

**Type:** Correctness (user-facing)
**Severity:** High
**Effort:** Days
**Area:** Rendering (photonic-render)
**Affected Files:**
- `crates/photonic-render/src/compositor.rs:185` (`SceneNodeKind::Text(_) => {}` explicit no-op)
- `crates/photonic-render/src/headless.rs` (zero glyphon/text wiring on the GPU headless path)

**Description:**
Discovered during video-editor P1 golden-corpus expansion (e964e75): text renders only in the interactive windowed renderer (glyphon pass). Both headless paths — the CPU compositor and the headless GPU path behind `render_rgba_with_opts` — silently drop every `Text` node. Consequence: PNG/JPEG/WebP/GIF/TIFF export and MCP `screenshot`-class/`render_frame_at`-class output lose all text today. The committed `text_basic` and `text_on_path` golden references are (correctly, per current behavior) blank — they act as change detectors that must be re-blessed when this is fixed. Blocks the video module's AS-1 vector-title export (Tier A rasterization uses `render_rgba_with_opts`) — must land before video P3/P4.

**Recommended Approach:**
Add a text rasterization path usable off the GUI thread: either drive glyphon against the headless device (preferred — same pixels as canvas), or a CPU glyph rasterizer (swash/ab_glyph) into the CPU compositor. Wire into both `composite_document` and the headless GPU pass; re-bless the two text golden cases; extend the corpus with a styled-text case.

**Why Not Auto-Fixed:**
Cross-cutting rendering work with font-stack implications; needs its own story + review, scheduled as a pre-P3 story in the video execution plan.

### TD-019: Windowed-Only Rendering Features — Headless Export Silently Drops Them

**Type:** Correctness (user-facing)
**Severity:** Medium
**Effort:** Weeks (sum of parts)
**Area:** Rendering (photonic-render)
**Affected Files:** `crates/photonic-render/src/headless.rs`, `compositor.rs` vs `renderer/mod.rs`

**Description:**
Same class as TD-018, found in the same sweep: variable-width strokes (`width_profile_id`), text-on-path (`path_spine_id`), fixed-field glows (`outer_glow`/`inner_glow`/`gaussian_glow`), and arrowheads (`arrowhead_start`/`arrowhead_end`) are wired only in the interactive renderer — zero references in headless paths, so exports drop them. Additionally `dash_array` has no rendering implementation anywhere (windowed included — dashed strokes draw solid), and `GroupNode::clip_children`/`clip_node_id` is data-model-only. Golden cases `variable_width_stroke`, `stroke_arrowheads`, `effect_glow_stack`, `text_on_path` are committed as forward-guards with doc comments citing this gap.

**Recommended Approach:**
Fix per-feature alongside TD-018's headless text work where they share plumbing; `dash_array` is a tessellator feature (lyon supports dashing via path iteration) independent of headless.

**Why Not Auto-Fixed:**
Each is a distinct feature implementation, not a wiring one-liner; needs prioritization against video-module phases.

**Resolution (TD-018):** Fixed in 8b2806e — `headless_text.rs` drives the canvas's glyphon pipeline against the headless device; CPU-compositor output is wrapped with the identical glyphon pass so both export paths produce the same text pixels. `text_basic` re-blessed (renders), new `text_styled` golden case added; `text_on_path` remains a TD-019 forward-guard. Committed 2026-07-07.
