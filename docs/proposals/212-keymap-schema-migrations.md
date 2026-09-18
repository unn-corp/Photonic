# 212 — Keymap Schema Migrations

> **Status: Accepted and Implemented (wave-1, 2026-08-03).**  
> `keymap_schema_version` + `migrate_keymap` run on preferences load; see
> ROADMAP §0's OpenCut harvest table.  
> CapCut-class apps version their shortcut tables so **new defaults** reach
> existing users without wiping custom binds. Photonic’s command registry +
> persisted keymap ([69](69-customizable-keyboard-shortcuts-searchab.md)) lacked
> a migration chain. Clean-room under [207](207-opencut-harvest-index.md) §2.

**Owner refs:**  
- [69](69-customizable-keyboard-shortcuts-searchab.md) — MVP shipped  
- `crates/photonic-gui/src/commands.rs` + `preferences.rs` keymap  
- Video commands continuously added on `feat/video-editor-module`  

**Territory:** GUI preferences only. **Effort:** S.  
**Format impact:** **none** on `.photon` documents — user config only.

---

## 1. Problem and user outcome

**Today.**

- Defaults live in `CommandDef` / `REGISTRY`.  
- User overrides live in `AppPreferences.keymap: HashMap<CommandId, KeyBinding>`.  
- **Missing default problem:** if a user already has a `preferences.json` from
  an older build, **new commands** with defaults never appear as bindings
  unless the user resets — because “absent from map” should mean default, but
  **changed defaults** for *existing* command ids cannot be pushed without
  either overwriting customs or leaving everyone on stale defaults.  
- Video mode is adding many commands (`video.*`); social and pro users will
  otherwise never see new shortcuts.

**After 212.** Preferences store a `keymap_schema_version: u32`. On load:

1. Run ordered migrations `vN → vN+1` that **only** insert bindings for keys
   that are still at the **previous default** or are **absent**.  
2. Never clobber a binding the user customized.  
3. New installs jump to `CURRENT_VERSION` with full defaults.

---

## 2. Current state

| Piece | State |
|---|---|
| `KeyBinding` serialize `"ctrl+shift+g"` | Shipped |
| `resolve_binding` user-over-default | Shipped |
| Conflict detection in settings UI | Shipped |
| Schema version field | **Missing** |
| Migration modules | **Missing** |
| Full hardcode → registry migration | Partial (69 remaining work) |

---

## 3. Contract

### 3.1 Storage shape

```json
{
  "keymap_schema_version": 3,
  "keymap": {
    "video.add_bookmark": "b",
    "undo": "ctrl+z"
  }
}
```

- Missing `keymap_schema_version` ⇒ treat as `0` and run all migrations.  
- `keymap` entries are only **overrides**; resolution remains
  `user.get(id).copied().or(registry_default)`.  
- Migrations may write into `keymap` to **pin** a new default for users who
  never customized that id (see §3.3).

### 3.2 Migration function signature

```rust
fn migrate_vN_to_vN1(state: KeymapState) -> KeymapState
// pure, deterministic, no IO
```

Each migration is a named file or module:
`preferences/keymap_migrations/v0_to_v1.rs`, registered in an ordered table.

### 3.3 Customization detection (normative)

A binding for `id` is **customized** if:

```text
keymap.get(id) is Some(b) AND b != default_binding(id) at the *pre-migration* schema
```

When promoting a **new default** for an existing `id`:

- If customized → leave user value.  
- If absent or equal to old default → set to **new** default (or remove key to
  mean “follow registry”, preferred).  

When adding a **brand-new** `id`:

- Do nothing to the map (absence ⇒ new registry default). Migration only
  needed if we must **reserve** a key that used to mean something else.

### 3.4 Conflict policy

If a migration would assign a default that collides with another command’s
resolved binding:

1. Prefer leaving the older command’s binding.  
2. Log / diagnostics entry (non-fatal).  
3. Surface in Keyboard Shortcuts UI as the existing conflict warning.

### 3.5 Versioning policy

- Bump `CURRENT_VERSION` in the **same PR** that changes a shipped default or
  introduces a binding that must be forced onto existing installs.  
- Changelog line under release notes: “Shortcuts: …”.  
- Migrations never deleted once shipped (same discipline as document
  migrations, but for prefs).

---

## 4. Non-goals

- Cloud sync of keymaps.  
- Per-mode (vector vs video) schema forks — single version chain; optional
  later `when: Mode` on CommandDef is orthogonal.  
- Chords / multi-key sequences (69 out of scope).  
- Import/export file format changes beyond version field (69 remaining).

---

## 5. Tests

| ID | Case | Pass |
|---|---|---|
| T1 | v0 empty prefs → CURRENT, all defaults resolve | unit |
| T2 | User customized `undo` → migration does not reset it | unit |
| T3 | Old default for `video.X` upgraded only if still old default | unit |
| T4 | Migration table is contiguous 0..CURRENT | unit |
| T5 | Conflict leaves prior binding, both still in map or resolved | unit |

---

## 6. Provenance

Versioned keybinding migrations are standard in editors that persist shortcuts
(VS Code keybinding updates, browser apps with localStorage maps, OpenCut
classic actions docs). Photonic implements the pattern on **its** `KeyBinding`
and `CommandId` types only.

---

## 7. Delivery

- Land **before** or **with** [210](210-timeline-interaction-velocity-pack.md)
  bookmark defaults and [213](213-social-first-editing-velocity.md) shortcut
  bundle so video defaults actually reach users.  
- Can ship without any video feature change (infra-only PR).
