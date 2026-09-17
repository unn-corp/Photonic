# Onboarding: Document Templates, Starter Palettes, and First-Run Experience (#47) — Design Proposal

> Status: design scaffold (not an implementation).

## Summary

A `WelcomeState` struct and its `draw()` method already exist in
`photonic-gui/src/welcome.rs:36`. It tracks `recent: Vec<RecentEntry>`, persists to disk
(`recent_docs.json`), and is wired into `photonic-app/src/main.rs:287`
(`PhotonicApp::new_with_welcome()`). However, the welcome screen offers no preset artboard
sizes, no curated templates, and no starter palettes — users must size the canvas manually
and build colour schemes from scratch. The MCP `apply_document_template` tool
(`photonic-mcp/src/server.rs:1393`) exists but is unreachable from the GUI.

## Scope

**In**
- New Document dialog (preset artboard sizes: print A4/Letter, web 1920×1080,
  social 1080×1080, iOS/Android device frames), colour mode selector (RGB/CMYK).
- Curated starter palettes surfaced in the welcome screen and the GUI colour panel.
- A small library of sample `.photon` template documents shipped as embedded assets.
- Wiring the existing `apply_document_template` MCP handler to a GUI action path.

**Out**
- User-defined or cloud-synced template libraries.
- Template marketplace / community sharing.
- CMYK soft-proofing (colour mode label only, not full CMYK workflow).

## Proposed Approach

1. **Preset size data** (`photonic-gui/src/welcome.rs`)  
   Add a `PresetSize` enum (or const slice of `(name, width, height, unit)` tuples)
   covering common sizes. `WelcomeState::new_document_dialog()` renders a grid of
   preset buttons; selecting one populates the existing `DragValue` width/height fields
   (already in `welcome.rs:179-187`).

2. **Starter palettes** (`photonic-gui/src/welcome.rs` or a new `palettes.rs`)  
   Define 6–8 curated palettes as `const` arrays of hex strings (Neutral, Warm,
   Cool, Brand-Blue, etc.). Display them as swatch strips on the welcome screen.
   Selecting a palette sets the document's global swatches (needs a
   `Command::SetSwatches` or document-level swatch list in
   `photonic-core`).

3. **Template assets**  
   Store 4–6 `.photon` JSON files under `crates/photonic-gui/assets/templates/`.
   Embed via `include_bytes!` or a build-script asset directory (check whether the
   project already has an assets embedding pattern). On selection, load with the same
   deserialization path as file-open.

4. **GUI wiring** (`photonic-gui/src/welcome.rs`, `photonic-app/src/main.rs`)  
   Extend `WelcomeAction` (the return type of `WelcomeState::draw()`) with variants:
   `NewFromPreset(ArtboardPreset)`, `NewFromTemplate(TemplateName)`,
   `ApplyPalette(StarterPalette)`. Handle these in the main event loop
   (`photonic-app/src/main.rs` welcome dispatch block around line 286–310).

5. **MCP bridge**  
   The existing `apply_document_template` MCP handler (`server.rs:1393`) can stay as-is;
   the GUI takes its own direct code path via the embedded assets. No MCP round-trip
   needed for the welcome screen.

## Affected Modules

- `crates/photonic-gui/src/welcome.rs` — primary change surface
- `crates/photonic-gui/src/lib.rs` — re-export any new `palettes` module
- `crates/photonic-gui/assets/templates/` — new directory (embedded `.photon` files)
- `crates/photonic-app/src/main.rs` — welcome action dispatch (lines ~286–310)
- `crates/photonic-core/` — optional `Command::SetSwatches` if swatch state is tracked
  in the document model

## Risks & Open Questions

- Where does the document-level swatch list live? If it's not in the `Document` struct
  today, starter palettes can only be applied per-selection, not document-wide. Needs
  clarification before palette wiring.
- Asset embedding strategy: `include_bytes!` bloats the binary; a runtime asset path
  might be better but complicates packaging. Decide before implementation.
- Template files must stay in sync with the serialization format. Any format version
  bump could silently break them. A schema-version assertion on load is advisable.
- The welcome screen currently has a fixed layout; adding a template gallery may require
  a scrollable panel (egui `ScrollArea`) to avoid overflow on small screens.

## Acceptance Criteria

- [ ] The welcome screen shows preset artboard sizes; selecting one pre-fills the
      new-document dimensions.
- [ ] At least 4 starter palettes are presented as swatch strips; selecting one applies
      colors to the new document.
- [ ] At least 4 template documents are available; opening one loads a pre-built canvas.
- [ ] Recent files list continues to function correctly.
- [ ] No MCP connection required for any of the above.

## Effort Estimate

**M** — data-heavy UI work but confined to `welcome.rs` and the main dispatch; no new
rendering or core-model changes unless swatch state is added.
