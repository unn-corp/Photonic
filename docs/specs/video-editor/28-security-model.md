# 28 — Security Model: Trust Boundaries, Path Containment, Subprocess and Parser Hardening

**Status:** Draft — implementation contract; no code authorization
**Date:** 2026-07-20
**Audience:** MCP owner, engine maintainers, release/packaging owner, legal reviewer

**Depends on:** [10-mcp-tools.md](10-mcp-tools.md) (tool surface and error codes), [02-engine.md](02-engine.md) (sidecar model), [05-import-export.md](05-import-export.md) (ffmpeg boundary), [01-data-model.md](01-data-model.md) (asset path resolution), [27-spec-audit.md](27-spec-audit.md) (MC-1, the finding this closes).

**Owns:** the security model for the video module and the MCP surface — trust boundaries, path containment, subprocess isolation, parser hardening, and the refusal vocabulary.

---

## 1. Why this document exists

**No document in the spec set contained the word "security".** [27 MC-1](27-spec-audit.md#mc-1--p0--security) is the only P0 in that audit with no owning document, which is very likely why it was never written: there was no plausible home for it.

Verified exposure, on the current code:

| # | Finding | Evidence |
|---|---|---|
| 1 | **The MCP server has no authentication of any kind** | `server.rs:163-168` — the router is `/mcp` + `CorsLayer::permissive()`, no auth layer |
| 2 | **`CorsLayer::permissive()`** lets any web page the user visits issue cross-origin requests and read the responses | `server.rs:166` |
| 3 | **No path validation anywhere in the video handlers** — no `canonicalize`, no root containment, no allowlist | grep clean across `photonic-mcp/src/` |
| 4 | Unvalidated filesystem paths on `import_media { paths }`, `relink_media { new_path }`, `export_sequence { out_path }`, `apply_lut { lut_path }`, caption import/export, `transcode_media` | `protocol/args/video.rs:769,777,969,986,1278` |
| 5 | Every imported file is parsed by an **unsandboxed `ffmpeg` subprocess** with attacker-controlled content | `decode/sidecar.rs` |
| 6 | Asset paths resolve "relative first, then absolute, then relink-by-hash", so a `.photon` from an untrusted source **reads paths outside the project** | [01 §9](01-data-model.md) |

Combined, 1–4 are **arbitrary local file read and write**, driven from a web page, with no confirmation step and no error code to refuse with.

**The one mitigating fact:** the server binds `127.0.0.1` (`server.rs:154`), so there is no remote network attacker. That narrows the threat model to *local processes and the user's own browser* — which is a real reduction and is why this is P0-fix-before-ship rather than P0-stop-everything. **It is not a control**, and it must not be treated as one.

**Already right, and worth keeping:** argument construction is safe — `export/encoder.rs` builds a `Vec<String>` argv and never a shell string, so there is no command-injection surface. The `.cube` parser bounds `LUT_3D_SIZE` to `2..=256` and rejects mismatched row counts (`lut.rs:22,37-42`), closing the obvious allocation bomb.

---

## 2. Trust boundaries

| Boundary | Trust | Consequence |
|---|---|---|
| **The user, via the GUI** | Trusted | May open and write anywhere the OS permits. No containment; that is what a desktop app is |
| **An MCP client** | **Semi-trusted** — the *agent* is delegated by the user, but its inputs may be attacker-influenced (a filename in a transcript, a path in a document it read) | §3 containment applies; destructive operations are refusable |
| **A web page in the user's browser** | **Untrusted** | Must not reach the server at all — §4 |
| **Media files, LUTs, subtitles, project files** | **Untrusted content** | §5 parser and subprocess hardening |
| **A `.photon` from elsewhere** | **Untrusted structure** | §6 asset-path resolution |

The important distinction is the second row. It is tempting to treat MCP as "the user by another route" — but an agent's arguments derive from content the agent read, and that content is not the user. Photonic already accepts this reasoning elsewhere: [23](23-legal-open-source-implementation-routes.md) treats agent-supplied provenance as needing independent verification.

---

## 3. Path containment

### 3.1 The rule

Every filesystem path crossing the MCP boundary is resolved and checked against an **allowed-roots** set before use:

```rust
pub struct PathPolicy { pub roots: Vec<PathBuf>, pub allow_read_outside: bool }

pub enum PathVerdict { Allowed(PathBuf), Denied { reason: DenyReason } }
pub enum DenyReason { OutsideRoots, TraversalRejected, NotCanonicalizable, SymlinkEscape, DeviceOrFifo }
```

Resolution order, normative:

1. Reject paths containing a NUL byte or, on Windows, a reserved device name.
2. **Canonicalize** — resolving symlinks. For a path that does not yet exist (an export target), canonicalize the deepest existing ancestor and re-append the remainder.
3. Assert the canonical path is **inside** an allowed root. Prefix comparison must be **component-wise**, never string-prefix: `/home/u/proj-evil` must not match root `/home/u/proj`.
4. Reject non-regular files for read (FIFOs, devices, sockets) — a FIFO passed to the decode sidecar hangs a worker indefinitely.

Canonicalizing **after** symlink resolution is the load-bearing detail: a symlink inside the project pointing at `~/.ssh` is the obvious bypass of a naive check.

### 3.2 Default roots

- The **open project's directory** and its sidecar cache.
- The **app config/data directory** (presets, manifests, title templates).
- A user-configured list of additional roots, empty by default.

`allow_read_outside` defaults to **true for read** and **false for write**. Rationale: importing media from anywhere is the normal case and blocking it would make the product useless, while writing outside the project is rare, and the asymmetry costs the attacker much more than it costs the user. Both are user-configurable; neither is silently overridable by a tool argument.

### 3.3 Refusal, not clamping

A denied path returns a **new MCP error code `PathNotPermitted`**, carrying the offending path and the reason. It is never silently rewritten into an allowed location — a silent rewrite means an export lands somewhere the caller did not ask for, which is its own defect.

[10 §8](10-mcp-tools.md)'s error catalogue gains this code; [36 §3](36-error-model.md) owns how it surfaces in the GUI.

### 3.4 Destructive operations confirm

Writing to an **existing** file outside the project root requires either an explicit `overwrite: true` argument or a GUI confirmation. An agent overwriting a user's file because a path was mis-derived is the failure this prevents, and it costs one boolean.

---

## 4. The MCP transport

Three changes, in priority order:

1. **Remove `CorsLayer::permissive()`.** This is the highest-severity, lowest-cost item in the document. The MCP server has no legitimate browser client; permissive CORS exists only to let any page read its responses. Replace with no CORS layer at all, or an explicit empty allowlist.
2. **Require a bearer token.** Generate per session, write it to the app data directory with owner-only permissions, and require it on every request. This is the standard local-service pattern and it defeats *both* the browser vector and other local processes. Reject unauthenticated requests with a 401 and **no detail**.
3. **Keep the loopback bind**, and never make the interface configurable to a non-loopback address without a separate, explicit decision. A "listen on 0.0.0.0" convenience flag would turn every finding above into a remote vulnerability.

**Rate-limiting is not proposed.** Against a local attacker it accomplishes nothing, and it would complicate legitimate agent batch operations.

---

## 5. Untrusted content

### 5.1 The decode subprocess

`ffmpeg` parses attacker-controlled bytes and is the largest attack surface in the product. It is also, usefully, already **out-of-process** — a crash contains itself, and `decode/scheduler.rs` already restarts it with a cap of 3 and backoff.

Required hardening, in order of value:

| Control | Rationale |
|---|---|
| **Wall-clock timeout per probe/decode start** | A malformed file that makes ffmpeg spin currently wedges a worker until EOF that never comes. `MAX_RESTARTS` bounds crashes, not hangs |
| **Memory ceiling** on the child (`RLIMIT_AS` / job object / `ulimit`) | Bounds an allocation bomb to one process |
| **No shell, ever** — argv vectors only | Already true (`export/encoder.rs`); make it an asserted invariant, not a habit |
| **Explicit `-f` / format restriction where the format is known** | Removes demuxer probing as an attack path for the proxy/thumbnail paths where the format was already established |
| OS sandbox (seccomp / AppContainer / sandbox-exec) | Strongest, most platform work. **Recommended as a follow-up**, not a v1 blocker |

### 5.2 Bring-your-own-FFmpeg

[05 §3.7](05-import-export.md) describes a user-configurable ffmpeg path as "a warning-gated preference". That is **arbitrary code execution configured from a settings file** — if malware can write the config, it gains execution under Photonic's identity.

**Recommendation:** the configured binary is resolved and its path recorded; on change, the user confirms **in the GUI**, and the confirmation is not settable by any MCP tool. Optionally record a digest and warn when it changes. Do not attempt signature verification — it is platform-specific and defeats the legitimate use case of a locally-built binary.

*(Note: I could not find this preference in the current code — the only hit is a comment in `export/encoder.rs`. It may be unimplemented, in which case this section is a constraint on building it rather than a fix.)*

### 5.3 Parsers

Photonic hand-writes parsers for `.cube` LUTs, SRT/VTT/ASS subtitles, and — under [34](34-interchange.md) — MLT XML, OTIO JSON and EDL. All consume untrusted files.

Normative rules for every one:

- **Bound every count and dimension read from the file** before it drives an allocation. The `.cube` parser already does this and is the model.
- **Bound total input size** and nesting depth. XML in particular needs an explicit entity-expansion and depth limit — "billion laughs" is a parser-independent hazard and MLT XML is the one format here where a document can reference other documents.
- **Never trust a declared length**; validate it against bytes actually present.
- **No panics on malformed input** — a parse failure is an `Err`, surfaced per [36](36-error-model.md). A panic in a GUI thread is a crash; a panic in an MCP handler is a denial of service.
- **Fuzz the interchange parsers.** They are pure byte-in/structure-out functions, which is the ideal fuzz target, and [34](34-interchange.md)'s formats are the ones most likely to be received from elsewhere.

---

## 6. Untrusted project files

[01 §9](01-data-model.md) resolves asset paths "relative first, then absolute, then relink-by-hash". A `.photon` received from someone else therefore reads absolute paths chosen by its author.

**Recommendation:**

- On opening a project whose **absolute** asset paths resolve outside the project directory, and which was not created locally, **report them and require confirmation** before resolving. Do not block — this is legitimate and common for a project on a media volume — but do not do it invisibly.
- **Never** resolve an absolute path to a location outside the allowed roots for a project opened via MCP.
- Treat every string in a project file as data: names, notes and marker text reach the UI and any log, so they must not be interpreted as markup or format strings.

---

## 7. What is deliberately not in scope

- **Sandboxing the whole application.** Photonic is a desktop editor; the user is trusted.
- **Encryption at rest**, DRM, or watermarking.
- **Multi-user authorization** — collaborative editing is a SPEC non-goal.
- **Network hardening beyond loopback** — there is no remote surface, and §4.3 exists to keep it that way.

---

## 8. Acceptance

| # | Test |
|---|---|
| 1 | A request without a valid token is rejected with 401 and no detail |
| 2 | A cross-origin request from a page origin is rejected (no permissive CORS) |
| 3 | `../` traversal, absolute escape, and **symlink escape** from inside a root are each denied with `PathNotPermitted` |
| 4 | Component-wise containment: root `/a/proj` denies `/a/proj-evil` |
| 5 | Write outside roots is denied by default; read outside roots is allowed by default; both follow config |
| 6 | Overwriting an existing file outside the project requires explicit opt-in |
| 7 | A FIFO or device passed as a media path is refused, not opened |
| 8 | A malformed media file that hangs ffmpeg is killed by timeout and reported; the worker survives |
| 9 | A `.cube` declaring an out-of-range size, and a subtitle/XML/OTIO/EDL file with a declared length exceeding its content, are each refused without panic |
| 10 | XML with deep nesting or entity expansion is refused by explicit limit |
| 11 | Fuzz corpora for the interchange parsers run in CI without panic |
| 12 | A project with absolute out-of-root asset paths prompts on GUI open, and refuses on MCP open |

---

## 9. Sequencing

| Order | Item | Cost |
|---|---|---|
| 1 | **Remove permissive CORS** | Minutes. Highest severity-to-cost ratio in the document |
| 2 | Bearer token on the MCP transport | Small |
| 3 | `PathPolicy` + `PathNotPermitted` + apply at every path-taking handler | Medium — the bulk of the work |
| 4 | Subprocess timeout + memory ceiling | Small |
| 5 | Parser bounds audit + no-panic guarantee | Medium |
| 6 | Untrusted-project prompting (§6) | Small |
| 7 | Fuzz harness for interchange parsers | Medium; lands with [34](34-interchange.md) |
| 8 | OS sandbox for the decode subprocess | Large; follow-up |

Items 1–2 should not wait for the rest. They are the difference between "any web page you visit can read and write your filesystem through Photonic" and "a local process must first steal a token", and together they are perhaps a day's work.
