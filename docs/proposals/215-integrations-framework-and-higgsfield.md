# 215 — Integrations Framework and the Higgsfield Provider

> **Status: Proposed — framework design accepted separately from the decisions it depends on.**
> This is **not** a K-Band item, so [26 §19.1](../specs/video-editor/26-kdenlive-mlt-parity.md#191-bands)'s
> Band-5 mini-spec gate does not apply to it and this document does not claim to discharge one.
> Its exit condition is different and stricter: a **new SPEC decision** narrowing
> [D-11](../specs/video-editor/SPEC.md#decisions), plus **counsel sign-off** under
> [23 §7.1](../specs/video-editor/23-legal-open-source-implementation-routes.md#71-rights-hierarchy) for anything that
> imports generative output. No code authorization until both land
> ([23 §14](../specs/video-editor/23-legal-open-source-implementation-routes.md#14-stopgo-checklist-before-any-code)).

**Owner refs:**
- **Tier 2 only:** [06 §2](../specs/video-editor/06-captions-ai.md) — already owns the
  `TranscriptionProvider`/`TtsProvider` contract under **D-04**. This document does not
  re-specify that contract; it demonstrates conformance to it (§5.2).
- **Tiers 1/3/4:** *no existing owner.* This document is the owner, which is itself a fact
  reviewers should weigh (§12.2).
- Layering rules: [00 §2](../specs/video-editor/00-overview.md)

**Territory:** a new `photonic-integrations` crate (§3.3) + `photonic-gui` + `photonic-mcp`
**Effort:** **L–XL** — the largest single item in the numbered proposal series: a new crate, a
provider registry, two transports, a GUI surface, a core helper extraction, and a legal-gated
capability map. Tier 2 alone is M.
**Security posture:** this item sends **user content off the machine** for the first time. §9 is
the load-bearing section, not an appendix.
**Format impact:** none to the document format if artifacts enter through the shared helpers (§6).

---

## 0. What accepting this document authorizes

195 and 201 do not face this problem: their owning decisions (26 §11 K-C1 / K-C4) were locked
before the mini-spec was written, so "accept the mini-spec" and "authorize the work" were the same
act. **Here they are not.** Stating this plainly, because a reviewer will otherwise reasonably
assume acceptance means "build it":

| Accepting 215 means | Accepting 215 does **not** mean |
|---|---|
| The framework shape is agreed: one Integrations surface, a provider registry, Higgsfield as provider #1 | Any Higgsfield capability may ship |
| **Tier 2** (transcription/TTS) may proceed **if** §10.1's D-04 argument is accepted on its merits | Tiers 1/3 may proceed — both need the new SPEC decision **and** counsel sign-off |
| The two SPEC amendments in §10.2–10.3 are tabled for decision | The amendments are adopted — they are drafted here, decided elsewhere |
| The `D-` numbering collision (§10.2) is escalated | A number has been chosen |

**Recorded decision (draft):** **Accept the framework; adopt Tier 2 only; hold Tiers 1 and 3
pending the SPEC decision and counsel; exclude Tier 4 from v1 outright.** Rationale in §5.5.

---

## 1. Problem and user outcome

Photonic has three separate, unrelated seams where an external AI service could plug in — the
caption provider traits, the raster placement path, and the media pool — and no user-visible
place where any of them is configured, no shared credential story, no shared job surface, and no
way to add a second vendor without a second bespoke panel. Meanwhile
[06 §2.4](../specs/video-editor/06-captions-ai.md) already promises a *pluggable* provider model
and explicitly anticipates more than one implementation ("speced here so the trait boundary is
proven to support more than one implementation from day one"), but ships exactly one non-mock
adapter and no UI to select it.

After this item, a user can:

1. Open **Integrations**, see Higgsfield listed with its connection state and remaining credits,
   and connect it — without editing a config file, and without the app having contacted anything
   before they asked.
2. Select an audio or video clip and auto-caption it through Higgsfield instead of the hosted
   default, choosing between providers in one place rather than by environment variable.
3. Generate a voiceover with a Higgsfield voice picked from a real enumerated list, not a
   hardcoded one — [06 §2.2](../specs/video-editor/06-captions-ai.md#22-provider-contract-requirements)
   requires `voices()` discovery and this is the first provider that satisfies it against a live
   catalogue.
4. See, **before** anything is spent, what a generation will cost in credits, and confirm or
   decline it.
5. Close the app mid-generation, reopen it, and be told *"2 Higgsfield jobs were in flight"* with
   their results recoverable — rather than silently losing work that has already been paid for
   (§3.4; this is the failure mode no existing job substrate in the repo can handle, §2.3).
6. Drive all of the above from an agent over MCP, with GUI/MCP parity per capability (§8).

The value is **not** the Higgsfield models. It is that provider selection, credentials, cost
preflight, job surfacing and provenance stop being invented per-vendor.

---

## 2. Current state in code

### 2.1 What exists and is directly usable

| # | Thing | Where | Shape |
|---|---|---|---|
| 1 | Provider traits — `TranscriptionProvider`, `TtsProvider`, plus `CancelToken`, `ProgressSink`, `ProviderProgress`, `ProviderError`, `VoiceDescriptor` | `crates/photonic-video/src/captions/provider.rs` | Blocking trait methods on a worker thread, progress over `crossbeam_channel`, cancel via `Arc<AtomicBool>`. Explicitly *not* async (02 §1) |
| 2 | The provider **contract** any Tier 2 adapter must satisfy | [06 §2.2](../specs/video-editor/06-captions-ai.md#22-provider-contract-requirements) | 10-row requirement table — see §5.2 |
| 3 | Config-model rule: provider selection, endpoints, and **auth token references (env var or OS keychain)** live in an app-level registry, **not** the Document | [06 §2.3](../specs/video-editor/06-captions-ai.md#23-config-model-pointer-only) | Normative today. §3.4's registry is the concrete instance of it |
| 4 | Reference adapter | `crates/photonic-video/src/captions/hosted.rs` | `ureq`; a config **enum** (`TranscriptionEndpointShape`, `TtsEndpointShape`) inside one struct, *not* a multi-impl trait; `map_ureq_error` → `ProviderError`; `apply_auth` injects a single configurable header |
| 5 | Mock adapter | `crates/photonic-video/src/captions/mock.rs` | The pattern §11's network-free fixtures follow |
| 6 | **End-to-end precedent for a network provider job** | `crates/photonic-mcp/src/handlers/video.rs:7247` `generate_voiceover` | `thread::spawn` → register with `JobRegistry` → call the trait synchronously inside the thread → update `JobStatus`. This is the substrate §4.1 reuses |
| 7 | GUI-side variant of the same | `crates/photonic-gui/src/panels/video/caption_editor.rs` | Calls the provider trait on a background thread and polls `crossbeam_channel` itself, bypassing `JobRegistry` (which lives in `photonic-mcp` and the GUI cannot see) |
| 8 | **Crate-shape precedent** | `crates/photonic-matte` | Standalone; `photonic-core` its only photonic dependency; does its **own outbound `ureq`** (`src/lib.rs:84`, one-time ONNX model download) with a `PHOTONIC_RMBG_MODEL_URL` override and a `PHOTONIC_RMBG_MODEL_PATH` offline path; consumed by both `photonic-gui` and `photonic-mcp` |
| 9 | Provenance hook | `crates/photonic-core/src/node.rs:239` | `prompt_history: Vec<String>` — "chronological log of AI prompts that created or modified this node. Stored in the document; **stripped from all export formats**." Written via `crates/photonic-mcp/src/handlers/utility.rs:1972` |
| 10 | Provenance-tagging convention | [21 §537-545](../specs/video-editor/21-dji-core-workflows.md) | `GradeOpProvenance`: an optional field, `serde` default `None`, additive |
| 11 | Subprocess hygiene | `media/child_registry.rs:140,258` (`reap_orphans`, `arm_parent_death_signal`), reference loop `media/proxy.rs:284-337` | Staging path → priority → parent-death arming → cooperative cancel → `stderr_tail` on failure → rename on success |
| 12 | Binary resolution | `media/ffmpeg_locate.rs:59` `locate()` | `$PHOTONIC_FFMPEG_DIR` then `PATH`. §4.3 mirrors it |
| 13 | Atomic artifact writes | `media/atomic_write.rs` | `staging_path`, `write_atomic`, `sweep_stale_staging` |
| 14 | Path sandbox | `photonic-core/src/path_policy.rs`, `photonic-mcp/src/path_guard.rs` `check_path` | Writes must stay under configured roots |
| 15 | Left-rail extension point | `crates/photonic-gui/src/panels/mod.rs:1246` `DrawerGroup` | `:1330` `ALL: [_; 6]`, `:1344` `VIDEO_ALL: [_; 10]`, `:1358` `all_for_mode`, `:1366` `icon`, `:1390` `title`; `#[serde(other)] Unknown` + normalize-on-load make adding a variant downgrade-safe |

### 2.2 Two landing zones that are **not** reusable as-is

This is the single most likely source of a wrong estimate, so it is stated precisely.

**(a) `place_image` needs a helper that does not exist.**
`crates/photonic-mcp/src/handlers/raster.rs:216` is
`pub async fn place_image(state: &AppState, args: PlaceImageArgs) -> ToolResult`. It inlines the
whole construction — `RasterImage::from_encoded` → `RasterNode::new` → `source_uri` →
`SceneNode::new` → `Transform::translate` (`:216-245`). A GUI panel cannot call it: wrong crate,
async, and threaded through `AppState`/`ToolResult`. **A new `photonic-core` helper is required**,
approximately:

```rust
pub fn raster_node_from_image(
    image: RasterImage,
    name: &str,
    transform: Transform,
    source_uri: Option<String>,
) -> SceneNode;
```

with `place_image` refactored to call it, so the MCP tool and the Integrations panel cannot drift.

**(b) `import_media` needs orchestration extracted, which is a different and larger job.**
`crates/photonic-mcp/src/handlers/video.rs:4000` already has its low-level constructor in core
(`photonic_core::timeline::MediaAsset::from_file`), so this is not a missing-constructor problem.
What is inlined at the handler is the *orchestration*: path validation, bin resolution/creation,
and batch command building. Extracting that is a larger refactor than (a) and should be **scoped
and estimated separately** rather than folded into (a)'s line item.

### 2.3 What does not exist anywhere in the repo

**Job persistence.** [195 §2](195-k-c1-clip-jobs-framework.md) records it: "`RenderQueue` and
`JobRegistry` are both process-lifetime only." `JobRegistry` GCs at `JOB_RETENTION = 600s`
(`video_jobs.rs:210-330`). Every job in Photonic today is local work that dies with the process,
and losing it costs only CPU time.

**A Higgsfield job is not that.** It runs server-side, survives a local crash, and **credits commit
at submit** (§9.3). A process-lifetime-only job model applied to it silently orphans work the user
has already paid for. §3.4 is the response, and it is the one genuinely new mechanism in this
document.

---

## 3. The Integrations framework

### 3.1 Scope decision: a framework, not a Higgsfield panel

The tab is specified as provider-agnostic from the start, because the alternative — a Higgsfield
panel plus a later refactor — has already been run once in this codebase for jobs, and
[195 §2](195-k-c1-clip-jobs-framework.md) is the write-up of the result (five unrelated
mechanisms, three of them hand-rolled at the call site). Adding provider #2 must cost a
registration, not a panel.

### 3.2 Surface placement — argued, not asserted

Two defensible homes:

| | Left rail (`DrawerGroup`) | Right rail (`RightDrawerGroup`) |
|---|---|---|
| For | Where *document-producing* tools live (MediaPool, Effects, Titles). Integrations produce document content | `RightDrawerGroup::Chat` is "the AI (Claude) chat assistant" — the existing home for assistive external-service surfaces. Integrations is closer in kind to Chat than to the Modify palette |
| Against | Integrations is configuration + queue as much as authoring; the left rail is the authoring rail | The right rail is narrower and hosts inspectors; a job queue with progress rows fits poorly |

**Recommendation: left rail**, on the grounds that the panel's *primary* verb is "produce an asset
into this document," and its configuration role is secondary and infrequent. Credential setup
belongs in Preferences regardless (§3.4), which removes the main argument for the Chat-adjacent
placement.

**If left rail:** add `DrawerGroup::Integrations`, extend `ALL` 6→7 and `VIDEO_ALL` 10→11
(`panels/mod.rs:1330,1344`), and add `icon()`/`title()` arms. The variant is downgrade-safe with no
extra work: `#[serde(other)] Unknown` plus `AppPreferences::load` normalization already handle a
newer build's token (round-trip test at `preferences.rs:637`).

This is the one placement question this document deliberately leaves open for the reviewer rather
than settling unilaterally, because it is a UX call with no technical forcing function.

### 3.3 Crate placement — resolved

**A new `photonic-integrations` crate**, modeled directly on `photonic-matte`.

| Candidate | Verdict |
|---|---|
| `photonic-core` | **Impossible.** Zero photonic dependencies and no HTTP client, by design |
| `photonic-video` | Compiles (both `photonic-gui` and `photonic-mcp` depend on it) but puts **Vector-mode image generation in the video crate**. Wrong ownership, and the name would lie |
| **`photonic-integrations`** | **Chosen.** `photonic-matte` is the shipped proof of exactly this shape: standalone leaf crate, `photonic-core` as its only photonic dep, its own `ureq` egress, consumed by both `photonic-gui` and `photonic-mcp`. No new wiring pattern is being invented |

The Tier 2 adapter is the one wrinkle: it implements traits that live in `photonic-video`. Two
options — have `photonic-integrations` depend on `photonic-video` for the trait (breaking the
core-only shape), or move the two provider traits down into `photonic-core` (a mechanical move; the
traits depend only on `photonic_core::timeline::Tick` and `crossbeam_channel`). **Recommend the
move**, and note it as a follow-up against 06 (§15.2) since 06 §2.1 documents the traits' location.

### 3.4 The provider registry

Concrete instance of [06 §2.3](../specs/video-editor/06-captions-ai.md#23-config-model-pointer-only)'s
"app-level provider registry," which is normative today but has no implementation.

```rust
pub trait IntegrationProvider {
    fn id(&self) -> &'static str;                     // "higgsfield"
    fn display_name(&self) -> &str;
    fn credential_state(&self) -> CredentialState;    // Absent | Present | Invalid { reason }
    fn capabilities(&self) -> &[Capability];
    fn params(&self, cap: CapabilityId) -> &[ParamSpec];   // reuse 30 §2's ParamSpec/ParamKind
    fn preflight(&self, req: &Request) -> Result<CostEstimate, ProviderError>;
    fn submit(&self, req: &Request) -> Result<RemoteJobId, ProviderError>;
    fn poll(&self, id: &RemoteJobId) -> Result<RemoteJobState, ProviderError>;
    fn fetch(&self, id: &RemoteJobId, into: &Path) -> Result<Artifact, ProviderError>;

    // The reattach pair — see §2.3. Not optional.
    fn list_remote_jobs(&self) -> Result<Vec<RemoteJobSummary>, ProviderError>;
    fn resume(&self, id: &RemoteJobId) -> Result<RemoteJobState, ProviderError>;
}
```

Notes that are load-bearing rather than incidental:

- **`ParamSpec`/`ParamKind` are reused, not redefined** — they already exist for effects
  ([30 §2](../specs/video-editor/30-effect-catalogue.md)) and 201 §3.3 established the precedent of
  a *second* catalogue sharing them. The Integrations panel renders params manifest-driven, exactly
  as the clip inspector does.
- **`preflight` is not advisory.** No `submit` may be issued without a `CostEstimate` shown and
  confirmed (§9.4).
- **Credentials never appear in this trait.** `credential_state` is a status, not an accessor.
  Auth is injected by the registry, per 06 §2.2's "never a trait parameter."
- **Reattach record.** A minimal sidecar — `{ provider_id, remote_job_id, capability, submitted_at,
  target_hint }` — written under the app config dir with `0o600` (the permission path 195 §5 uses),
  swept on successful fetch. It is *not* in the Document: a project file must never carry a
  vendor job id (06 §2.3's rule, and it would leak across users on a shared project).

### 3.5 Panel shape

Provider list → connection state and credit balance → capability picker → manifest-driven params →
**cost preflight and confirm** → job rows with progress → artifact hand-off button. Failed jobs
show a coalesced error with the provider's message available on demand, matching the badge+toast
pattern 195 §1 specifies for job failures.

---

## 4. The Higgsfield provider — job model and transport

### 4.1 Job model — the provider-trait substrate, not a new one

**Decision: reuse the substrate `generate_voiceover` already proves** — one blocking call on a
worker thread, progress over `crossbeam_channel`, cooperative `AtomicBool` cancel; wrapped in
`JobRegistry` when driven from MCP (`handlers/video.rs:7247` verbatim shape), and driven directly
by the panel when driven from the GUI (`caption_editor.rs` verbatim shape).

This generalizes to Tiers 1 and 3 unchanged: a submit-then-poll Higgsfield call is still "one
blocking call on a worker thread that streams progress and checks a cancel flag" — the thread loops
`generate get` internally. It also yields a **better** stop story than Tier 2's TTS has today, which
checks cancel once before the call; a poll loop checks between every poll.

Two alternatives rejected, on their own documents' evidence:

- **K-C1 ([195](195-k-c1-clip-jobs-framework.md)) — rejected as a dependency.** It is proposed and
  unbuilt (`grep -rn 'ClipJob\|JobTemplate' crates/` is clean), so depending on it stalls Tier 2
  behind an unrelated unbuilt proposal. Worse, its `JobOutcome` is deliberately closed to
  asset-level derivations (`NewAsset`/`AttachProxy`/`Analysis`/`None`) with **no variant for
  "produced a document node from a prompt"** — even a built K-C1 would need extending for Tier 1.
  If K-C1 later ships, these jobs should migrate into it; that is a follow-up (§15.4), not a
  precondition.
- **`RenderQueue` — rejected outright.** `ensure_worker(&self, gpu: GpuContext, tools: FfmpegTools)`
  (`export/job_queue.rs:82`) requires a GPU context and ffmpeg tools that a network call has no use
  for. 195 §3.1 rejects it for the same reason.

### 4.2 Transport — one provider, an internal transport enum

Following `hosted.rs`'s actual precedent (a config **enum** inside one struct, §2.1 row 4) rather
than a two-implementation trait:

```rust
pub struct HiggsfieldProvider { transport: Transport, /* … */ }
pub enum Transport {
    Cli  { bin: PathBuf },     // shells `higgsfield … --json`
    Http { base: Url },        // ureq; unspecified until a contract is published
}
```

Reasoning: `HttpBackend` is **unimplemented and unspecified** — Higgsfield's HTTP API is not
published as a stable contract (only third-party resellers document it). A trait forces parity with
something that does not exist and invites a stub that satisfies the compiler and nothing else. The
enum keeps one provider type, one credential path, one job model, and makes adding the HTTP arm a
local change. If a stable public contract is published, promoting the enum to a trait is mechanical.

### 4.3 `Cli` transport specifics

- **Not the default.** [`packaging/mcpb/README.md:3`](../../packaging/mcpb/README.md) commits the
  bundle to being **binary-only**; `@higgsfield/cli` is an npm/Node package. Defaulting to `Cli`
  makes that commitment false. `Cli` ships as an **explicit opt-in** with an unambiguous "Node/CLI
  not found" state in the panel — never a silent failure, never an implicit install. §12.1 records
  the consequence: until the `Http` arm exists, Higgsfield is a **dev/desktop-with-Node capability,
  not an MCPB-bundle capability**, and that is a real scope limit, not a footnote.
- **Submit-then-poll, never `--wait`.** `--wait` blocks; the panel must stay responsive and the
  cancel flag must be checked between polls. Use `generate create` → `generate get`.
- **Binary resolution:** `$PHOTONIC_HIGGSFIELD_BIN` → `PATH`, mirroring `ffmpeg_locate::locate()`
  (`media/ffmpeg_locate.rs:59`).
- **argv vectors only, never a shell string** — 195 §7's rule, and the prompt is user text.
- **Register the child and arm parent-death** (`media/child_registry.rs:140,258`). This is the exact
  omission [28 §1](../specs/video-editor/28-security-model.md) records as finding #4 against
  `transcode_media`; do not reproduce it.
- **`Http` arm, when it exists, uses `ureq`** — matching `hosted.rs` and `photonic-matte`. Not
  `reqwest`, which exists in `photonic-app` only for `mcp_proxy`; a third HTTP client in the
  workspace is not justified.

---

## 5. Capability map and tiering

Model names and parameter lists verified against `higgsfield model get <job_type>` on 2026-08-09
against CLI 1.1.18. §11 requires re-verification before acceptance — a vendor catalogue is not a
frozen fixture.

### 5.1 The four tiers

| Tier | Representative models | Lands via | Gate |
|---|---|---|---|
| **1 — generative** | `nano_banana_pro` (`prompt*`, `image_references`, `aspect_ratio`, `resolution`), `gpt_image_2`, `flux_2`, `seedream_v5_pro` · `seedance_2_0` (`prompt*`, `start_image`, `end_image`, `duration`, `resolution`, `generate_audio`), `veo3_1`, `kling3_0`, `wan3_0` | §2.2(a) core helper → raster node · §2.2(b) import path → media pool | **D-11 + counsel** |
| **2 — services** | `speech2text` (`audio_references*`) · `text2speech_v2` (`prompt*`, `variant*`, `voice_id*`, `voice_type*`) + `higgsfield voices list` | Third adapter beside `hosted.rs`/`mock.rs` | **D-04** — argued in §10.1 |
| **3 — enhancement** | `video_upscale`, `topaz_video`, `video_deflicker`, `image_background_remover`, `video_background_remover`, `outpaint`, `color_grading_lut` | Clip/layer ops; LUT → existing `apply_lut` | **D-11 + counsel** |
| **4 — 3D** | `image_to_3d`, `tripo_h3_1_image_to_3d`, `hunyuan3d_v3_image_to_3d`, `meshy_v6_text_to_3d` (12 total) | — | **Out of v1** |

**Tier 4 is excluded on capability grounds, not legal ones.** Photonic has no 3D pipeline, no mesh
type, no viewer. The only honest integration would be "write a `.glb` to disk," which is a file
downloader wearing a feature's clothes. Excluded outright rather than stubbed.

### 5.2 Tier 2 against [06 §2.2](../specs/video-editor/06-captions-ai.md#22-provider-contract-requirements)

This is the checklist Tier 2 must pass. **Four rows cannot be verified without spending credits**,
and this document spends none (§11.5) — so they are recorded as open, not assumed:

| 06 §2.2 requirement | Level | Higgsfield status |
|---|---|---|
| Word-level timestamps ≤50 ms | Required | **UNVERIFIED** — `speech2text`'s response shape is not documented and `model get` returns only its input params. Must be confirmed by one paid job before Tier 2 is scheduled |
| Cue-only source → proportional split | Accepted, degraded | Already implemented adapter-side (`captions/proportional.rs`, `distribute_words_proportionally`). If the row above fails, this is the fallback and the result is marked `degraded: true` |
| Confidence score | Optional | **UNVERIFIED**; omitted if absent, no UI consequence |
| Language hint | Optional | **UNVERIFIED** — no `language` param appears in `speech2text`'s schema, so likely auto-detect only |
| Chunking for long audio | Required (adapter) | **UNVERIFIED** — per-request audio limit unpublished. Adapter must chunk and re-stitch by segment offset regardless; the limit is a constant to discover, not a design risk |
| Streaming partials | Optional | **No** — submit-then-poll has no partial channel. `ProviderProgress::Partial` simply never emitted |
| Auth mechanism | Required | **Satisfied** — injected by the registry (§3.4), never a trait parameter |
| TTS voice discovery `voices()` | Required | **Satisfied** — `higgsfield voices list --json` enumerates id + voice type (`preset` for built-in, `element` for cloned), which maps onto `VoiceDescriptor` |
| TTS word-level alignment | Optional | **No** — a generated clip gets no auto-captions; user can auto-caption the resulting audio via CAP-009, exactly as 06 §2.2 provides for |
| Timeout `2 × source duration + 30s` | Required (adapter) | **Adopted** as the Tier 2 default; see §9.5 for Tiers 1/3, which have no "source duration" |

`clip_transcriber` is **not** proposed as the transcription model. Its schema
(`audio_references*`, `clips_num` default 10) indicates clip *selection*, not transcription, and
using it as a transcriber on a name match would be a guess. `speech2text` is the candidate.

### 5.3 Tier 3 is a cloud alternative, never a replacement

Three of Tier 3's models duplicate work Photonic has already shipped natively:

| Higgsfield | Photonic native |
|---|---|
| `video_deflicker` | Native deflicker (`graph/deflicker.rs`, `graph/rolling_bands.rs`) |
| `image_background_remover` / `video_background_remover` | **`photonic-matte`** — local, offline after first model fetch |
| `color_grading_lut` | Existing `apply_lut` consumes the produced `.cube`; native grading unaffected |

The native path stays the default in every case. Tier 3 exists for the cases where the local result
is not good enough, and the panel must say so — not present the cloud option as the primary.

### 5.4 Non-goals

No Higgsfield-specific document types. No Higgsfield branding in the Document. No automatic
generation on any trigger the user did not press. No background pre-fetching. No model catalogue
bundled into the binary — the catalogue is queried at runtime and cached, so a vendor change does
not require a Photonic release.

### 5.5 Why the recommended disposition splits by tier

Tier 2 rides an already-locked decision, satisfies (or degrades cleanly against) a contract that
already exists, and touches no new document data. Tiers 1 and 3 create *new document content* from
a generative service, which is precisely what
[23 §7.1](../specs/video-editor/23-legal-open-source-implementation-routes.md#71-rights-hierarchy) reserves for counsel.
Shipping Tier 2 first also de-risks Tiers 1/3: the tab, the registry, the credential path, the cost
preflight, the job model and the reattach record are all proven by a capability that does not need
the legal gate.

---

## 6. Migration and format-version impact

**None to the document format**, provided artifacts enter through §2.2's helpers — a generated PNG
becomes an ordinary `SceneNodeKind::Raster`, a generated MP4 becomes an ordinary `MediaAsset`.
Nothing in the Document records that Higgsfield produced them except the provenance in §7.

| Surface | Change | Migration |
|---|---|---|
| Document (`.photon` / timeline project) | Provenance field only (§7.2) — additive, `serde` default | None; old files load, new files load in old builds minus the field |
| `AppPreferences` | New integrations block: enabled providers, transport selection, credential *references* (never values) | `#[serde(default)]`; older builds ignore it |
| `DrawerGroup` | One new variant | Downgrade-safe via `#[serde(other)] Unknown` + load-time normalization (`preferences.rs:637` round-trip test extends to cover it) |
| Reattach sidecar (§3.4) | New file under the app config dir | Not part of any document; absence is the normal state |

---

## 7. Undo units and provenance

### 7.1 Undo

**The network job is not an undo unit.** Only the resulting mutation is, and each is an existing
unit with an existing inverse:

| User verb | Undo unit | Inverse |
|---|---|---|
| Generate image → place | The existing add-raster-node command | Remove node |
| Generate video/audio → import | The existing media-import command | Remove asset |
| Generate LUT → apply | The existing `apply_lut` grade-op command | Remove grade op |
| Auto-caption / voiceover via Higgsfield | The existing `CaptionCmd` / `TtsCmd` | Unchanged from 06 §3.6 |

A cancelled, failed, or still-running generation produces **zero** history entries. This is not an
incidental property: it is what makes "stop waiting" safe (§9.3), since nothing has entered the
document yet.

### 7.2 Provenance — one resolved mismatch

`prompt_history` (`photonic-core/src/node.rs:239`) is a `Vec<String>` and grows by one entry per
generation. A singular `GradeOpProvenance`-style field would hold only the most recent job's
metadata, so after a second regeneration the two records silently disagree about which job produced
what.

**Decision: a parallel `Vec`, index-aligned with `prompt_history`.**

```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub generation_provenance: Vec<GenerationProvenance>,   // { provider, model, remote_job_id, at }
```

Alignment is asserted in the acceptance suite (§11 test 9). The alternative — "most recent
generation only" — is cheaper but produces a record that is wrong rather than incomplete, which is
the worse failure for a field that exists for legal reasons.

**Open question for counsel, not for engineering:** `prompt_history` is *stripped from all export
formats*. Whether generative provenance must survive export (C2PA-style) is a §10 question. This
document does **not** propose changing the stripping behaviour unilaterally — that would leak
prompt text into shared files, which is a privacy regression.

---

## 8. MCP surface

Registered in the Pattern B catalog (`photonic-mcp/src/catalog.rs`, schemas via `schema_gen.rs`,
docs regenerated into `docs/mcp-api.md`), per
[mcp-2026-07-28 §6](../specs/mcp-2026-07-28.md#6-pattern-b--large-tool-catalog).

| Tool | Purpose | PathPolicy |
|---|---|---|
| `list_integrations` | Providers, connection state, capabilities | — |
| `list_integration_capabilities` | Params for one capability (manifest-driven, mirrors `list_generator_kinds`' shape in 201 §6) | — |
| `estimate_integration_cost` | `preflight` — no spend | — |
| `run_integration_job` | Submit + poll to completion; returns artifact path or lands it directly | **write** (`check_path`) |
| `list_integration_jobs` | Local job registry + `list_remote_jobs()` reattach view | — |

**GUI/MCP parity:** full for every capability. **One deliberate exception:** *credential entry is
GUI-and-config-only and has no MCP tool.* An agent must never be able to write a credential, and
[28 §1](../specs/video-editor/28-security-model.md) is explicit that agent arguments derive from
content the agent read and are not the user. Recorded here as the sole parity gap.

**Cost confirmation over MCP** is the awkward case: there is no user present to confirm. Resolution
— `run_integration_job` requires an explicit `max_credits` argument and fails closed if the
preflight exceeds it. An agent cannot spend an unbounded amount by omission.

---

## 9. Security posture

### 9.1 What is actually new

**This is not the product's first outbound network egress.** `photonic-matte/src/lib.rs:84` already
performs `ureq::get` to fetch an ONNX model, with a `PHOTONIC_RMBG_MODEL_URL` override and a
`PHOTONIC_RMBG_MODEL_PATH` offline path.
[28 §159](../specs/video-editor/28-security-model.md)'s "there is no remote surface, and §4.3 exists
to keep it that way" is about **inbound** listeners, and remains true.

**What is new is the first egress that carries user content.** Prompt text, and for image-to-image
or image-to-video, **actual frames from the user's document**, leave the machine and are processed
by a third party. That is the novel risk and the thing a reviewer should focus on. `photonic-matte`
is the precedent to follow in *form* — env-var override, explicit offline path, one-time and
visible — but it fetches a public artifact **in**; this sends private content **out**.

### 9.2 Controls

1. **Off by default.** Never enabled by opening a project. Never enabled by an MCP call.
2. **Explicit connect.** The provider is inert until the user connects it in the panel.
3. **Disclosure at point of use** — the confirm dialog states what leaves the machine (prompt text;
   and, when an image input is used, which frame/layer), not a settings-page footnote.
4. **argv vectors only**, never a shell string (§4.3).
5. **All downloads through `check_path` + `write_atomic`** into the app cache dir; stale staging
   swept with `sweep_stale_staging`.
6. **No document content is uploaded implicitly** — only inputs the user explicitly selected as
   parameters for that job.
7. **Reattach record is `0o600`** and contains no credential and no prompt text.

### 9.3 Two honest limits

**There is no cancel.** CLI 1.1.18 exposes `cost, create, get, list, wait, workflow` — no cancel
verb — and credits commit at submit. The UI must therefore say **"Stop waiting (credits already
spent)"**, not "Cancel". Presenting a Cancel button that does not cancel would be a lie about the
user's money. The cooperative cancel flag stops the *local* poll loop and download only.

**Credentials are plaintext on disk regardless.** The `Cli` transport delegates to
`~/.config/higgsfield/credentials.json`, which Photonic does not control. Photonic can promise not
to create a *second* copy, and does. It cannot promise "no plaintext credentials on disk" while the
`Cli` transport exists. Only the `Http` transport plus an OS keychain
([06 §2.3](../specs/video-editor/06-captions-ai.md#23-config-model-pointer-only) contemplates
exactly this) could make that claim, and that is another reason the `Http` arm matters beyond
packaging.

### 9.4 Spend controls

No `submit` without a `preflight` result displayed and confirmed. Credit balance shown in the
panel (`higgsfield workspace list`). MCP path bounded by mandatory `max_credits` (§8). A failed job
that still consumed credits is reported as such rather than silently retried — **no automatic
retry on any job that may have spent credits** (§9.5).

### 9.5 Timeouts, retries, rate limits

- **Tier 2 timeout:** 06 §2.2's `2 × source audio duration + 30s`.
- **Tiers 1/3 timeout:** no "source duration" exists, so adapt
  [195 §7.3 rule 4](195-k-c1-clip-jobs-framework.md)'s bound — **default 600 s, hard max 3600 s**,
  configurable per capability. Generations observed in the 3–8 minute range sit inside the default;
  video models may need a raised per-capability value.
- **Retry:** transport-level retry (connect failure, 5xx **before** submit) is permitted with
  bounded backoff. **Retry after a successful submit is forbidden** — it double-spends. Recovery
  after submit is `resume()`, never re-submit.
- **Rate limits:** `ProviderError::RateLimited` already exists (`hosted.rs` maps HTTP 429 to it).
  Surfaced as a job-row state with the provider's retry-after honoured; never auto-escalated.
- **Batch partial failure:** each item is an independent job. A batch reports per-item outcomes;
  successes land, failures are listed with their errors. No all-or-nothing rollback, because
  successes have already been paid for.

---

## 10. Legal gate and SPEC amendments

### 10.1 The D-04 argument for Tier 2 — made, not assumed

**The counter-argument, stated first.** [D-11](../specs/video-editor/SPEC.md#decisions) (locked
2026-07-12) puts "account-backed licensing services" out of scope. Higgsfield is, on its face, an
account-backed service. A reviewer is entitled to read "route Higgsfield through D-04's pluggable
provider interface" as an attempt to get an account-backed service in through a side door while
D-11 is pointed at the front one. That reading should be answered, not ignored.

**The answer.** D-11's clause sits in a sentence about *content*: "Online stock catalogs, stock
footage, account-backed licensing services, automatic content recommendation, and media without
release-grade rights evidence remain out of scope." Every item in that list is a way of obtaining
**pre-existing third-party creative work under a licence**. The amendment it belongs to (23 §S1)
is about a bundled starter audio pack and its rights manifest. D-11 governs *whose content ends up
in the user's project and under what licence*.

Tier 2 obtains no content. `speech2text` returns a transcript **of the user's own audio**;
`text2speech_v2` returns a reading **of the user's own script**. No third-party creative work is
licensed, catalogued, recommended, or delivered. The service sold is compute.

That is exactly the transaction [D-04](../specs/video-editor/SPEC.md#decisions) already authorizes
— "Captions/TTS via pluggable provider interface" — and which
[06 §2.2](../specs/video-editor/06-captions-ai.md#22-provider-contract-requirements) explicitly
opens to any adapter "hosted or otherwise." The existing default backend is itself a remote service
reached with a bearer token; the difference between it and Higgsfield is who operates it and who
bills for it, neither of which is what D-11 regulates.

**The boundary this argument does not cross.** It applies *only* to transforming user-supplied
input. It does **not** extend to Tiers 1 and 3, which synthesize new imagery — there the output is
plausibly third-party creative work, its copyright status is unsettled, and
[23 §7.1](../specs/video-editor/23-legal-open-source-implementation-routes.md#71-rights-hierarchy) applies with full
force. **Tiers 1 and 3 are not argued for here and remain gated.** If a reviewer rejects the
distinction, the fallback is clean: Tier 2 waits for the same amendment as Tiers 1/3, and nothing
else in this document changes.

### 10.2 Proposed SPEC decision — number contested

> **D-??:** Generative and AI-service integrations operated under the **user's own account and
> credentials**, producing output into the user's own project, are in scope, subject to the asset
> rights gate in 23 §7 for anything Photonic itself bundles or redistributes. D-11's exclusion of
> "account-backed licensing services" is clarified to govern **content Photonic bundles,
> catalogues, recommends, or redistributes** — not services a user elects to call with their own
> credentials. Photonic bundles no generative output, ships no vendor catalogue in the binary, and
> redistributes nothing.

**⚠ The number is not proposed, because the `D-` space is contested.** Two independent sequences
exist: **SPEC Decisions `D-01…D-12`** (zero-padded,
[SPEC.md §Decisions](../specs/video-editor/SPEC.md#decisions)) and the **DJI inventory `D-1…D-15`**
([00 §85](../specs/video-editor/00-overview.md)), where D-13 is HDR/10-bit
(23 §S3, [26:561](../specs/video-editor/26-kdenlive-mlt-parity.md)), D-14 still panoramas
(23 §S5) and D-15 shot detection ([26 §397](../specs/video-editor/26-kdenlive-mlt-parity.md)).
Taking "the next number" in the SPEC space collides visually with three live DJI items. **This
needs an editorial call before the amendment is tabled**; resolving it silently would create a
citation ambiguity that outlives this document. Recorded as open question §12.3.

### 10.3 Proposed 23 §4 amendment — **S15**

S1–S12 are assigned and S13–S14 are drafted-not-accepted
([23 §4.7](../specs/video-editor/23-legal-open-source-implementation-routes.md)), so **S15** is the
next free number.

> **S15 — user-account generative integrations.** Replace, within the D-11 non-goal, the clause
> "account-backed licensing services" with: *"account-backed **content-licensing** services.
> Generative or AI-processing services invoked with the user's own credentials, whose output enters
> only that user's own project, are in scope subject to the gates below."*

**Gates, mandatory if accepted:**

1. **Terms review.** Counsel confirms Higgsfield's ToS permit commercial use of output by the
   account holder, and that Photonic invoking the API on the user's behalf is permitted.
2. **Training/provenance posture.** Recorded: whether user inputs are retained or used for
   training, and whether that is disclosed to the user at point of use.
3. **Copyrightability risk.** Recorded, not resolved — the user is told output copyrightability is
   unsettled in some jurisdictions; Photonic makes no representation.
4. **No redistribution.** Photonic bundles, ships and redistributes **no** generated output. Any
   future proposal to do so re-engages 23 §7.2's `AssetRightsManifest` in full.
5. **Provenance recorded** on every generated artifact (§7.2).
6. **Vendor-neutral.** The amendment is written to the *shape* of the integration, not to
   Higgsfield, so provider #2 does not need a new amendment.

**Counsel sign-off on gates 1–3 is the exit condition for Tiers 1 and 3.** This document states the
questions; it does not answer them, and nothing in it should be read as legal advice.

---

## 11. Acceptance fixtures and tests

**No test in this suite touches the network or spends a credit.** Every provider interaction is
against a mock transport replaying recorded `--json` payloads, following `captions/mock.rs`.

| # | Assertion | Where | Ties to |
|---|---|---|---|
| 1 | Registry with zero configured providers renders the panel empty and performs no I/O | `photonic-integrations/tests/registry.rs` | §9.2 rule 1 |
| 2 | A provider with `CredentialState::Absent` cannot `submit`; the panel shows connect, not run | same | §9.2 rule 2 |
| 3 | `submit` without a prior confirmed `preflight` is rejected at the registry, not the UI | same | §9.4 |
| 4 | `run_integration_job` with `max_credits` below the estimate fails closed and spends nothing | `photonic-mcp/tests/integrations.rs` | §8 |
| 5 | Stop-waiting mid-poll ends the local loop, leaves zero history entries, and reports credits spent | `photonic-integrations/tests/jobs.rs` | §7.1, §9.3 |
| 6 | Timeout at the configured bound reports `Failed`, kills the child, and the worker survives | same | §9.5 |
| 7 | Transport error **before** submit retries with backoff; error **after** submit never re-submits | same | §9.5 |
| 8 | Reattach: a sidecar record from a prior process surfaces via `list_remote_jobs()` on start | same | §3.4, §2.3 |
| 9 | After two generations on one node, `prompt_history.len() == generation_provenance.len()` and indices align | `photonic-core/tests/provenance.rs` | §7.2 |
| 10 | Provenance round-trips additively; a file written with it loads in a build without the field | same | §6 |
| 11 | `DrawerGroup::Integrations` prefs round-trip; an unknown newer token normalizes rather than discarding the file | extends `preferences.rs:637` | §3.2, §6 |
| 12 | Refactored `place_image` produces a byte-identical node to the pre-refactor path for the existing corpus | `photonic-mcp/src/handlers/raster.rs` tests | §2.2(a) |
| 13 | Panel and MCP produce identical results for one capability end-to-end (parity) | `photonic-mcp/tests/integrations.rs` | §8 |
| 14 | No MCP tool can write a credential — asserted over the generated schema | `schema_gen` test | §8 exception |
| 15 | Tier 2 adapter degrades to proportional word distribution when the mock returns cue-only output, and marks `degraded: true` | `photonic-video/tests/captions.rs` | §5.2 |
| 16 | `voices()` maps the recorded `voices list --json` payload onto `VoiceDescriptor` with no hardcoded list | same | §5.2 |
| 17 | Batch partial failure: successes land, failures listed, no rollback of paid successes | `photonic-integrations/tests/jobs.rs` | §9.5 |
| 18 | Offline: with no network and no CLI, the panel reports unavailable and the rest of the app is unaffected | `photonic-integrations/tests/offline.rs` | §9.2, ROADMAP §10.8 |

**Pre-acceptance verification (manual, one-time):** re-run `higgsfield model get` for every model in
§5.1 and confirm the param lists; and resolve §5.2's four UNVERIFIED rows, which requires **one paid
`speech2text` job**. That single job is the only credit spend this document sanctions, it must
happen **before** Tier 2 is scheduled, and its result may invalidate Tier 2's timestamp row.

---

## 12. Risks, open questions, deliberate exclusions

### 12.1 Risks

1. **The `Cli` transport limits where this ships.** Until the `Http` arm exists, Higgsfield is
   unavailable in the binary-only MCPB bundle (§4.3). If the primary target is MCPB users, this
   item delivers much less than it appears to. **Mitigation:** state the limit in the panel and in
   release notes; do not let it be discovered.
2. **Undocumented vendor API.** The `Http` arm cannot be specified until Higgsfield publishes a
   contract. **Mitigation:** the transport enum (§4.2) confines the unknown to one arm.
3. **Vendor catalogue churn.** 78 models today; model ids may vanish. **Mitigation:** catalogue
   queried at runtime, never compiled in (§5.4); unknown ids degrade to "unavailable."
4. **Tier 2's timestamp requirement may fail** (§5.2 row 1). **Mitigation:** the proportional
   fallback exists and is already implemented; the result is `degraded`, not broken.
5. **Cost surprise.** Preflight is an estimate. **Mitigation:** show balance before and after;
   `max_credits` on the MCP path.
6. **Scope creep into a vendor-features panel.** 78 models is an inviting surface.
   **Mitigation:** §5.4's non-goals, and tiering that makes each addition a gated decision.

### 12.2 Open questions needing a call (each with a recommendation)

1. **Left rail or right rail?** (§3.2) — *Recommend left rail.*
2. **Move the provider traits to `photonic-core`?** (§3.3) — *Recommend yes*; mechanical, and it
   keeps `photonic-integrations` a clean leaf crate.
3. **Which `D-` sequence gets the new decision?** (§10.2) — *No recommendation; editorial call.*
4. **Does generative provenance need to survive export?** (§7.2) — *Recommend no change now*;
   exporting `prompt_history` would leak prompt text into shared files. Revisit if counsel requires
   C2PA-style attestation.
5. **Should this document own Tiers 1/3/4 at all,** given it has no owning spec section for them
   (§0)? — *Recommend that acceptance of Tiers 1/3 create an owning spec section rather than leave
   a proposal as the permanent home.*

### 12.3 Deliberately excluded

Tier 4 (§5.1). Any Higgsfield-specific document type. Credential entry over MCP (§8). Automatic
retry after submit (§9.5). Bundling any generated output (§10.3 gate 4). Provider #2 — the
framework is built to accept one, but none is proposed.

---

## 13. Vendor dependency and terms provenance

This is **not** a clean-room section. 195 §12 and 201 §10 exist to prove no copyleft Kdenlive/MLT
source was consulted while reimplementing a reference NLE's behaviour
([26 §2](../specs/video-editor/26-kdenlive-mlt-parity.md)'s fence). **215 reimplements nothing** —
it calls a vendor's published interface. The applicable provenance question is different:

| Question | Record |
|---|---|
| What was consulted? | `higgsfield --help` and subcommand help; `higgsfield model list --json`; `higgsfield model get <job_type>` for the models in §5.1; `higgsfield voices --help`; `higgsfield workspace list --json`. All are the vendor's own CLI output on an authenticated account |
| What was **not** consulted? | The CLI's source. No decompilation, no traffic capture, no reverse-engineering of the HTTP API — which is precisely why §4.2 leaves the `Http` arm unspecified rather than inferring it |
| Was any output sampled? | **No.** No `generate create` was run; zero credits spent. Every model claim in §5 derives from declared input schemas, and §5.2's four unverifiable rows are marked as such rather than guessed |
| Vendor terms | **Not yet reviewed** — §10.3 gate 1 is open. No claim about permitted use appears anywhere in this document |
| New dependency | The `higgsfield` CLI (npm, `@higgsfield/cli`) as an **optional, user-installed** runtime tool — not vendored, not bundled, not auto-installed (§4.3). No new Rust dependency beyond `ureq`, already in the workspace |

---

## 14. Definition of done → ROADMAP §10, made answerable

| # | [ROADMAP §10](../specs/video-editor/ROADMAP.md#10-definition-of-done) point | Answered by |
|---|---|---|
| 1 | Core op/engine service with unit tests | `photonic-integrations` registry + provider (§3.4, §4), the extracted `raster_node_from_image` helper (§2.2a); §11 tests 1–8, 12 |
| 2 | GUI route, or a recorded exception | The Integrations panel (§3.5); §11 tests 11, 13. **No exception requested** |
| 3 | MCP tool/schema/generated docs | §8 — five tools, `docs/mcp-api.md` regenerated; §11 tests 4, 13, 14. **One recorded parity exception: credential entry is GUI-only** |
| 4 | One user verb = one undo unit | §7.1 — zero new command variants; every landing reuses an existing unit with its existing inverse |
| 5 | Additive serde/migration round-trip | §6 — no format-version bump; §11 tests 10, 11 |
| 6 | IR/eval/golden/sync coverage for new pixel/audio paths | **Not applicable and deliberately so** — no new pixel or audio *path* is created. Generated media enters as an ordinary raster node or media asset and is evaluated by the existing paths, which is the reason §2.2's helper extraction is a requirement rather than a convenience |
| 7 | Hard gates green; trend metrics not regressed | No graph-compile, eval, or export path is touched. The only new runtime cost is opt-in network I/O on a worker thread, off the render path entirely |
| 8 | Offline, privacy, licensing, content, product gates | **The open one.** Offline: §11 test 18. Privacy: §9.1–9.3. Licensing/content: §10 — **not yet passed; counsel sign-off is the exit condition for Tiers 1/3** |
| 9 | No protected-surface regression | Only `place_image` is refactored; §11 test 12 asserts byte-identical output. No existing command, IR op, or manifest row is modified |
| 10 | Goal-backward L1–L4, incl. GUI/MCP parity | §1's six outcomes are the L4 script; §11 test 13 is the parity arm, test 8 the reattach arm |

**Point 8 is why this item cannot be marked done on engineering evidence alone**, and the reason
§0's disposition splits by tier.

---

## 15. Follow-ups

Changes this document deliberately did **not** make (each needs its own change):

1. **[28-security-model.md §159](../specs/video-editor/28-security-model.md)** — "there is no remote
   surface, and §4.3 exists to keep it that way" is true of *inbound* surfaces and reads as a claim
   about all network activity. It was already imprecise before this proposal:
   `photonic-matte/src/lib.rs:84` performs outbound `ureq::get` today. 28 should state the
   inbound/outbound distinction explicitly and gain a section on **egress carrying user content**
   (§9.1), which is genuinely new.
2. **[06-captions-ai.md §2.4](../specs/video-editor/06-captions-ai.md)** — two corrections. It
   locates the hosted adapters at `photonic-video/src/ai/hosted.rs`; they are at
   `photonic-video/src/captions/hosted.rs`. And if §3.3's trait move is accepted, 06 §2.1's stated
   location for the traits changes too. 06 should also record that a third adapter now sits beside
   `hosted.rs`/`mock.rs`.
3. **[00-overview.md](../specs/video-editor/00-overview.md) doc/crate map** — a row for
   `photonic-integrations`, and for this document if Tiers 1/3 are accepted (§12.2 q5).
4. **[195 K-C1](195-k-c1-clip-jobs-framework.md)** — if K-C1 is later built, its `JobOutcome` needs
   a variant for "document node produced from a prompt" before integration jobs can migrate into
   it (§4.1). Filed here so the constraint is not rediscovered.
5. **[SPEC.md Open Questions](../specs/video-editor/SPEC.md)** — the `D-` numbering collision
   (§10.2) should be recorded there rather than living only in this proposal.
6. **`docs/mcp-api.md`** — regenerated by the existing CI step when §8's tools land; no manual edit.

---

## Provenance

Written 2026-08-09 against `feat/video-editor-module` at `35a0d6a`. All code citations verified
against the working tree at that commit. Vendor claims verified against `higgsfield` CLI 1.1.18 on
an authenticated account with 90 credits, **without running any generation** — see §13.
