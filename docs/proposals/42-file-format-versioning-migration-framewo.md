# File-Format Versioning, Migration Framework, and Package/Collect (#42)

> Status: **migration framework implemented.** Package/Collect is deferred — it
> depends on linked-asset (ImageNode) support that does not exist yet (see
> *Remaining work*).

## What this PR implements

- `crates/photonic-core/src/migration.rs`: the `FormatMigration` trait, an
  ordered `migrations()` chain (empty at v1), `detect_version`, and
  `run_migrations`/`run_migrations_with` that upgrade a raw `serde_json::Value`
  from its stored version up to the target, bumping `format_version` per step.
- `Document::from_json` now migrates at the JSON-tree level **before** struct
  deserialization, instead of deserializing first and rejecting. Older documents
  are upgraded through the chain; a document from a newer build loads leniently
  (unknown fields dropped) within `migration::COMPAT_WINDOW` versions ahead and
  is refused beyond it. v1 documents and documents predating the
  `format_version` field load unchanged.
- `docs/format-versions.md`: the versioning policy + changelog.
- Unit tests cover chain ordering/version bumping, early stop, failure
  reporting, the v1 no-op, round-trip, missing-version default, lenient
  newer-version load, and far-future rejection.

### How to add the next migration

1. Bump `CURRENT_FORMAT_VERSION` in `document.rs`.
2. Implement `FormatMigration` (N → N+1) and append it to `migration::migrations()`.
3. Add a changelog entry to `docs/format-versions.md` and a golden-file test.

### Remaining work (follow-up)

- **Package / Collect**: gather linked assets/fonts into a folder and relink to
  relative paths. Blocked on linked-asset support (`ImageNode::path`, issue #27),
  so it is intentionally not implemented here.

---

> Original design scaffold follows.

## Summary

The `.photon` JSON format has `format_version: u32` (constant `CURRENT_FORMAT_VERSION = 1`, `document.rs:66`) and a guard in `Document::from_str` (line 895–902) that rejects future versions. However, there is no forward migration: if a document saved by a future build adds a new field (e.g. `ImageNode` from M3, artboard constraints, appearance stacks), the current loader either silently drops it (serde `#[serde(default)]`) or fails. As the model grows, this becomes a compatibility liability. There is also no Package/Collect: documents that reference linked assets or fonts cannot be moved without manual file gathering.

## Scope (in / out)

**In:**
- A versioned migration chain: a trait + registry of per-version migration functions that upgrades a JSON value from version N to N+1, executed in sequence on open.
- Policy: `CURRENT_FORMAT_VERSION` bumped on every field addition or structural change; each bump gets a corresponding migration function.
- A schema changelog (committed to `docs/format-versions.md`).
- On open: warn (non-fatal) if the version is newer than `CURRENT_FORMAT_VERSION` (unknown version, load with defaults); refuse if the gap is beyond a configurable compatibility window.
- **Package Document**: collect all linked asset paths (embedded images, linked images, referenced fonts) into a target folder; update links in the saved `.photon` to relative paths.

**Out:**
- Binary / CBOR format — out of scope; JSON remains the canonical format.
- Lossless downgrade (saving as an older version) — too complex; document as unsupported.
- Cloud sync or asset hosting — out of scope.

## Proposed Approach

### Migration Framework

1. **Trait** in `crates/photonic-core/src/document.rs` (or a new `crates/photonic-core/src/migration.rs`):

```rust
/// Upgrade a raw JSON Value from one format version to the next.
pub trait FormatMigration {
    fn from_version(&self) -> u32;
    fn to_version(&self) -> u32;
    fn migrate(&self, value: &mut serde_json::Value) -> Result<(), MigrationError>;
}
```

2. **Registry**: A `static` or `LazyLock` `Vec<Box<dyn FormatMigration>>` sorted by `from_version`. On open, `Document::from_str` calls `run_migrations(value, detected_version, CURRENT_FORMAT_VERSION)` which applies the chain in order.

3. **`run_migrations` function**:
   - Parse the raw bytes to `serde_json::Value` first (no struct deserialization yet).
   - Read `value["format_version"]` (default 1 if absent).
   - If `file_version > CURRENT_FORMAT_VERSION`: emit a non-fatal warning, proceed with serde defaults.
   - Else: for each migration with `from_version` in `[file_version, CURRENT_FORMAT_VERSION)`, call `migrate(&mut value)`.
   - Only then deserialize with `serde_json::from_value::<Document>(value)`.

4. **First real migration (v1 → v2)**: When the next structural change lands (e.g. `ImageNode` in M3), bump `CURRENT_FORMAT_VERSION` to 2 and write a `MigrateV1ToV2` struct that adds the new fields with defaults, or renames moved fields.

5. **Tests**: A golden-file test for each migration: store a minimal V_N JSON string as a test fixture; assert it deserializes cleanly to the current `Document` after migration.

### Package / Collect

6. **New function** `crates/photonic-core/src/export.rs` or a new `package.rs`:

```rust
pub struct PackageOptions {
    pub target_dir: PathBuf,
    pub copy_fonts: bool,
    pub copy_linked_images: bool,
    pub relink_to_relative: bool,
}

pub fn package_document(
    doc: &mut Document,
    source_path: &Path,
    opts: &PackageOptions,
) -> Result<PackageManifest, PackageError>
```

- Walk all `SceneNode`s for `ImageNode::path` (linked image paths) once M3 exists.
- Walk `Document::fonts` (if a font list is added) for embedded/referenced font paths.
- Copy each asset to `target_dir/Links/` (images) or `target_dir/Fonts/` (fonts).
- If `relink_to_relative`, rewrite the path strings in the document to relative paths.
- Write the updated document JSON to `target_dir/<docname>.photon`.
- Return a `PackageManifest` listing all copied files.

7. **MCP tool**: `package_document(output_dir, copy_fonts, copy_linked_images)`.

### Version Policy (documented in `docs/format-versions.md`)

- Additive-only changes (new optional field with `#[serde(default)]`): bump patch commentary only, no new version. Real structural changes (renamed field, removed field, type change, new required field): bump `CURRENT_FORMAT_VERSION`.
- Compatibility window: support loading files up to 5 versions old; refuse with a clear error beyond that.

## Affected Modules

- `crates/photonic-core/src/document.rs` — `CURRENT_FORMAT_VERSION` constant, `from_str` migration hook, `MigrationError` type
- `crates/photonic-core/src/migration.rs` — new file: `FormatMigration` trait, registry, `run_migrations`
- `crates/photonic-core/src/export.rs` (or new `package.rs`) — `package_document` function
- `crates/photonic-core/src/lib.rs` — re-export migration and package items
- `crates/photonic-mcp/src/server.rs` — `package_document` tool handler
- `crates/photonic-mcp/src/protocol.rs` — `PackageDocumentArgs` struct
- `docs/format-versions.md` — new file (schema changelog)
- `tests/fixtures/` — per-version golden JSON files

## Risks & Open Questions

- **`serde_json::Value` round-trip fidelity**: Migrating via raw JSON values means any field not reflected in the value (e.g. computed from other fields) must be reconstructed post-migration. Keep migrations pure data transforms.
- **Version discovery without deserialization**: Reading `format_version` from raw bytes requires a two-parse strategy (Value → migration → struct). This is already how `from_str` works today; the migration step inserts between the two.
- **Linked assets tracking**: `Document` has no current `ImageNode` or font reference list. Package/Collect cannot be fully implemented until M3 adds those node types. Ship the migration framework now; land the package feature as a follow-up once M3 is done.
- **Font licensing**: Copying fonts for packaging raises licensing questions; mark the feature as "copy only if permitted by font license" and add a UI warning.

## Acceptance Criteria

- [ ] A V1 document opened by a V2+ build runs the migration chain without data loss; the migration is covered by a golden-file test.
- [ ] A document newer than `CURRENT_FORMAT_VERSION` opens with a visible warning but does not crash.
- [ ] `CONTRIBUTING.md` / `docs/format-versions.md` documents the policy for when to bump the version.
- [ ] `package_document` copies linked assets and rewrites paths; the output folder is self-contained.
- [ ] MCP `package_document` tool is functional.

## Effort Estimate

**M** — The migration framework itself is moderate (trait + registry + two-parse strategy). Package/Collect is straightforward once M3 `ImageNode` exists but is gated on that. Tests and documentation add meaningful time.
