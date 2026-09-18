# 42 — Localization: Scope, String Externalization, Locale Formatting, Script-Aware Captions

**Status:** Draft — implementation contract; no code authorization
**Date:** 2026-07-20
**Audience:** product owner, GUI owner, caption/text owners, CI owner

**Depends on:** [01-data-model.md](01-data-model.md) §7/§9, [03-render-color-pipeline.md](03-render-color-pipeline.md), [06-captions-ai.md](06-captions-ai.md) §3.5/§5, [10-mcp-tools.md](10-mcp-tools.md) §1, [13-ux-components.md](13-ux-components.md), [36-error-model.md](36-error-model.md), [37-robustness.md](37-robustness.md) §4.

**Owns:** [27 MC-8](27-spec-audit.md#mc-8--p2--localization) in full — **both halves**. MC-8 assigned captions to [06](06-captions-ai.md) and the shell to "product"; "product" is not a document, so the shell half had no owner.

**Does not own:** accessibility ([41](41-accessibility.md)). Adjacent — both concern who can use the editor — but they share no mechanism, and coupling them would delay both.

---

## 1. The gap

Verified 2026-07-20:

- **No i18n crate anywhere.** Every user-visible string is a Rust literal at its use site — ~2,345 sentence-shaped literals across `photonic-gui` + `photonic-app`, of which ~421 sit directly in a `label`/`button`/`heading` call.
- **The caption wrap budget is a Unicode scalar count.** `captions/grouping.rs:38` sets `max_chars_per_line: 42`; `:73` measures with `text.chars().count()`. [06 §3.5](06-captions-ai.md) documents this as normative.
- **Caption tokenization is `split_whitespace`** (`captions/proportional.rs:19`). Japanese, Chinese and Thai have no inter-word spaces, so an entire cue collapses to **one token** — word-level timing, karaoke and the grouping heuristics all silently degenerate to cue granularity.
- **Sentence detection is ASCII-only** (`grouping.rs:60`). The CJK terminators `。！？`, Arabic `؟`, Devanagari `।` are all missed, so the cue-split rule never fires for those scripts.
- **Typewriter reveal splits grapheme clusters** (`graph/compile.rs:1701-1703` — `chars().count()` then `chars().take(n)`), which can emit a Devanagari matra with no base or truncate an emoji ZWJ sequence.
- **RTL is one parenthetical** — [06 §5.2](06-captions-ai.md): *"(v1 is LTR-only; RTL sweep direction is future work)."* That is the entire treatment of bidirectional text in the spec set.
- **The UI has no non-Latin glyphs at all.** `photonic-app/src/main.rs:503` builds `FontDefinitions::default()` and adds only `egui_phosphor`. A Japanese or Arabic UI locale renders as tofu in every widget.

### 1.1 The asymmetry that structures this document

The two text stacks are in **opposite** states, and the spec set treats them as one problem:

| | UI chrome | Caption / on-video text |
|---|---|---|
| Engine | `epaint` → `ab_glyph` | `glyphon` → `cosmic-text` |
| Shaping | **none** — per-`char` glyph lookup | `rustybuzz`, `Shaping::Advanced` at **every** call site |
| BiDi | **none** | `unicode-bidi` (non-optional dep) |
| Line breaking | hand-rolled | `unicode-linebreak` (UAX #14) |
| Font fallback | flat per-`char` over registered fonts | per-platform, per-script `Fallback` + system scan |

**The render path is already right and the authoring path is already wrong.** A caption containing Arabic is shaped with joining forms, reordered per UAX #9 and wrapped per UAX #14 **today**. The `42` in `grouping.rs` is an upstream approximation of a wrap that cosmic-text subsequently performs correctly — and that mismatch is the entire CJK defect. This is a far cheaper problem than MC-8 implies on the caption side, and a far more expensive one on the UI side.

---

## 2. Principles

1. **Content correctness outranks chrome translation.** A wrong caption ships inside the customer's exported video and cannot be patched. An English menu is an inconvenience.
2. **Refuse visibly rather than render wrongly** — consistent with [36 §2](36-error-model.md#2-principles).
3. **Technical formats are not localized.** Any string parsed back, written to a file, or compared has exactly one form everywhere.
4. **Persisted decisions must not depend on machine state.** Cue boundaries are stored in the document, so they may never depend on font metrics. This single rule decides §5.
5. **Ship the mechanism before the translations.** Externalizing after the UI stabilises means doing it twice.
6. **Never guess a language.** It is a stored, user-visible field with an explicit default.

---

## 3. Scope

| Tier | Content | v1? |
|---|---|---|
| **A** | Script-correct caption **content**: measurement, segmentation, shaping, bidi, fallback, clean refusal | **Yes — blocking.** This is the half that ships inside exports |
| **B** | UI string **externalization mechanism**, `en-US` only, pseudo-locales in CI | **Yes — blocking.** Cheap now, expensive later |
| **C** | Actual UI translations | No — v2; a product decision |
| **D** | **RTL UI** (mirrored layout) | **No — refused**, see §3.2 |

**Why B ships with zero translations.** Externalizing ~2,345 strings is mechanical but large, and every feature added before Tier C adds more. Doing it now costs one macro call at the point the string is already being written, and immediately buys §8's pseudo-localization harness — which finds truncation, concatenation and hardcoded strings **whether or not any translation exists**. That harness justifies the change on its own.

### 3.2 Why RTL UI is refused, not deferred

Three blockers, increasing in severity:

1. **Our own code** uses `egui::Layout::right_to_left` across **19 files** as an idiom meaning "right-align this row". Under a global mirror each would need to become direction-relative. Tractable.
2. **egui has no context-level direction** — it is per-container and opt-in, so mirroring means threading direction through every container by hand.
3. **egui cannot render RTL text at all.** `epaint` has no bidi dependency in any version; the upstream issue has been open since **2021**. Recent releases improved shaping and explicitly did not touch bidi, script detection or Arabic joining.

Blocker 3 is not on our schedule and not responsive to our effort. Mirroring the layout while the text inside still renders in logical order is **worse** than not mirroring. Photonic therefore **refuses `ar`, `he`, `fa`, `ur` as UI locales** with a stated reason.

**This does not restrict caption content** — Arabic and Hebrew captions are Tier A and supported (§7).

---

## 4. String externalization

**Decision: Fluent, via `i18n-embed` + `i18n-embed-fl`, catalogs as `.ftl` embedded at build time, `fl!` at the call site.**

Rationale, in decision order:

1. **`fl!` is checked at compile time** — a typo or deleted key is a build error, not a runtime `???`. Across 84 files this is the difference between a safe refactor and an unsafe one, and it is the strongest single argument in the Rust i18n landscape.
2. **CLDR-correct plurals.** Russian and Polish need `few`/`many`; Arabic needs all six categories. Anything offering only singular/plural is wrong for those languages by construction.
3. **Gender and variant selection in the catalog**, where translators can reach them, with no Rust API surface.
4. **Immediate mode makes locale switching free** — egui re-runs the UI closure every frame, so swapping the bundle translates the whole app on the next frame. A live language switcher becomes trivial rather than a project.
5. **Bidi isolation is automatic** — interpolated arguments are wrapped in FSI/PDI, which is exactly the protection needed when a filename is interpolated into a translated sentence.

**Rejected:** `rust-i18n` — the most actively maintained and ergonomically nicest option, **rejected on plurals**: it offers count-based branch keys, not CLDR plural-category resolution, and correct plurals are the only reason to own a plural mechanism at all. No compile-time key checking either, which at this size is disqualifying alone. · `gettext-rs` — drags a native build dependency into a workspace that otherwise cross-compiles cleanly; the pure-Rust alternative is dead. · A hand-rolled `HashMap` — fails on plurals, gender and compile-time checking; every project starting here migrates later at greater cost.

**The concatenation rule.** Never build a user-visible sentence by concatenation or by `format!` over translated fragments. One message id per sentence, with named arguments. Word order is not preserved across languages. §8's bracket-delimiter pseudo-locale detects violations mechanically: a concatenated sentence renders as one message split across two bracket pairs.

**Locale selection** is detected once at startup, normalized, negotiated against the shipped set, and stored as an explicit preference (`ui_locale: Option<String>`, `None` = follow OS). This mirrors the existing `DocumentUnit` precedent — and **`DocumentUnit` stays independent of `ui_locale`**, because a German user working to an American client's spec needs inches with a German UI.

---

## 5. Locale-aware formatting

### 5.1 The line

**A format is *technical* if anything other than a human ever reads it.** Technical formats are invariant. A format is *human* only if it is displayed, never parsed, never written to a file, never compared.

| Value | Class | Why |
|---|---|---|
| **Timecode `HH:MM:SS:FF`** | **Technical — never localized** | `parse_timecode` reads it back; `at_tc` accepts it over MCP; it round-trips through EDL/OTIO ([34](34-interchange.md)). It is SMPTE, not prose |
| Frame numbers, sample/bit rates, resolutions | Technical | Parsed, or copied into ffmpeg argv |
| **MCP args and `error_code`s** | **Technical** | Agents match on them ([36 §5](36-error-model.md#5-mcp-mapping)) |
| Human-readable durations, dates, file sizes, percentages | **Human — localized** | Displayed, never parsed. Note stored timestamps stay ISO-8601 UTC — that is technical |

**The mistake this table exists to prevent** is applying a locale decimal separator or Eastern Arabic-Indic digits to a timecode field. It looks like thoughtful localization. It breaks `parse_timecode`, breaks every interchange format, and breaks CAP-019 parity. **The timecode ruler, the monitor readout and every timecode field stay ASCII in all locales.** The *label* beside them is translated; the value is not.

**Engine:** ICU4X with `compiled_data`, restricted to the shipped locale set, so a build shipping only `en-US` pays approximately nothing. Measure the binary delta with `cargo bloat` when it lands and record it in [25](25-performance.md).

---

## 6. Caption text is user content, not UI

### 6.1 The rule that decides the architecture

**Cue boundaries are persisted in the document. Line breaks are not** ([06 §3.5](06-captions-ai.md)). Therefore:

- **Render-time wrapping may use measured advance width** — recomputed every frame, stored nowhere.
- **Authoring-time cue segmentation may not** — measured advance depends on the resolved font face, which depends on what is installed. A cue boundary depending on font metrics makes the *document* machine-dependent, breaking reproducibility and producing different `.photon` files from the same input on two machines.

So the fix is **not** "measure everything". It is: fix the unit of the authoring budget, and leave the render-time wrap alone.

### 6.2 Render-time wrapping is already correct — do not regress it

`caption.rs` sets the buffer width and lets cosmic-text wrap, using shaped advances, UAX #14 breaks and UAX #9 reordering. **Correct for CJK, Arabic, Hebrew and Devanagari today.** The only requirement is negative: **`Shaping::Advanced` is mandatory at every call site and must never become a performance toggle** (§7.1).

[06 §5.3](06-captions-ai.md)'s claim that render-time wrap targets `max_chars_per_line` "and font metrics" is **wrong and must be corrected** — the render path wraps on `max_width` alone. The character budget is an authoring-time concept only.

### 6.3 The authoring budget: width-weighted grapheme cells

Replace the scalar count with an integer **half-width cell** budget: iterate **grapheme clusters** (UAX #29), skip zero-advance combining marks, weight each remaining cluster by East Asian Width — `W`/`F` count 2, everything else 1.

Three properties make this right:

1. **It is exactly Netflix's own model** — their Japanese rule counts full-width as 1 and half-width as 0.5; half-width cells are that with the fractions cleared.
2. **It is deterministic integer arithmetic** over frozen Unicode tables — identical on GPU and CPU, identical across machines, safe to persist (§6.1).
3. **It fixes Thai for free** — Netflix excludes composite characters (tone marks, top/bottom vowels), which is precisely the combining-mark skip.

`unicode-segmentation` is already in the tree transitively; `unicode-width` needs promoting to a direct dependency. **`unicode-width` is used here and nowhere else** — it reports terminal cell widths, which are wrong for hit-testing or caret placement in a proportional font.

### 6.4 Per-language budgets

`CaptionTrack` gains `language: Option<String>` — additive and `#[serde(default)]`, so it is load-compatible with v4 on its own. It nonetheless ships inside the **consolidated v4→v5 step** ([01 §9.1](01-data-model.md#91-the-v4--v5-migration--one-step-nine-changes)): nine such changes are pending across seven documents, and separate bumps would imply nine format versions. The transcription providers already carry a language field and drop it on the floor today; auto-caption populates from it.

| Language | Netflix CPL | `max_cells_per_line` | Reading speed |
|---|---|---|---|
| Latin / Cyrillic / Greek | 42 | **84** | 20 cps |
| Arabic, Hebrew | 42 | **84** | 20 cps |
| Thai | 35 (marks excluded) | **70** | 17 cps |
| Chinese | 16 full-width | **32** | 9 cps |
| Korean | 16 full-width | **32** | 12 cps |
| Japanese | **13** full-width | **26** | **4 cps** |

Two notes that matter: the commonly cited "16" for Japanese is the **SDH** limit — regular horizontal Japanese is **13**. And Japanese minimum cue duration is **500 ms**, not [06 §3.5](06-captions-ai.md)'s 0.8 s, so `min_cue_duration` becomes language-derived too. The table is data, not code.

**Reading speed is a second, independent gate.** Cells are geometry; cps is duration, and they are not substitutes — a 42-character English line held 0.8 s satisfies the budget and violates comfortable reading. That 20 cps (English) and 4 cps (Japanese) express the *same* comfort level is exactly why this must be per-language and never a global constant.

### 6.5 Tokenization, terminators, reveal

**Tokenization** in priority order: provider word timings when present (unchanged); otherwise UAX #29 word boundaries; and for **scriptio continua** (Han, Kana, Thai, Lao, Khmer, Myanmar) distribute timing across **grapheme clusters** rather than fabricating words — so karaoke on a Japanese cue sweeps per cluster, which is what Japanese karaoke actually does. Proportional weight changes from `chars().count()` to cell width, so a kanji weighs 2 against a Latin letter's 1.

**Sentence terminators** extend to `。！？．` (CJK), `؟ ۔` (Arabic/Urdu), `। ॥` (Devanagari), `; ·` (Greek), `။ ၏` (Myanmar). Trailing closing punctuation is stripped before the test — which also fixes the existing English bug where `He said "stop."` fails to register.

**Typewriter reveal** moves from `chars()` to grapheme clusters. Identical to current behaviour for ASCII, so **no golden changes for existing fixtures**.

---

## 7. Font fallback and complex scripts

### 7.1 Supported today — because it already works

Arabic joining and lam-alef · Indic reordering · mark positioning · BiDi reordering · CJK line breaking with basic kinsoku · script-aware fallback. All verified in the render path, not aspirational.

**The single hard requirement is negative:** `Shaping::Basic` must never be offered as a performance option. It is a 1:1 codepoint→glyph lookup — Arabic renders as disconnected isolated letters, Devanagari matras stay in logical position. **The trap is that CJK looks fine under `Basic`**, so the regression ships and is only discovered when Arabic or Hindi content arrives. Enforce with a test asserting the shaping mode at every call site.

### 7.2 Three fallback defects, increasing in severity

1. **The locale is not normalized.** cosmic-text's Han-unification arm matches the locale string **exactly** against `"ja"`, `"ko"`, `"zh-HK"`, `"zh-TW"`, falling through to a Simplified-Chinese default — and the system locale is `"ja-JP"`, which matches none of them. **Consequence: a Japanese user on a Japanese machine gets Han characters in Simplified Chinese glyph forms.** These are legible-but-wrong shapes, so it does not look like a bug, it looks like a bad font. Fix: derive the locale from the caption track's `language` (§6.4), not the OS — the caption language and the UI language are independent. *(Inferred from a code read; confirm empirically with a `ja` fixture before the fix lands.)*
2. **Fallback names families that may not be installed.** Fix: a Photonic `Fallback` impl naming, per script, the faces we ship or have verified, ahead of the platform list. **Bundle Noto Sans Arabic and Devanagari (small, unreliable platform coverage); do not bundle full CJK** (10–20 MB, and every target platform ships a usable face). All Noto is SIL OFL.
3. **Substitution is silent, and this is the severe one.** A caption authored with font X and exported on a machine lacking X produces a different image, silently — contradicting canvas-equals-export identity and making caption goldens machine-dependent. **The vector path already solved this**: `text_outline.rs` warns when glyphon resolves a different family, precisely so export does not quietly render in a fallback font. That mechanism exists and was never applied to captions.

**Fix, three parts:** (a) reuse the existing substitution warning in the caption compositor, routed to [36](36-error-model.md) as `Render::FontSubstituted`; (b) **missing-glyph (tofu) detection is an error, not a warning** — a glyph id of 0 means the exported video contains a literal box; (c) **export refuses to complete silently on tofu**, consistent with [37 §1.3](37-robustness.md#13-recovery-protocol)'s rule that an export reporting success while producing garbage is the worst available outcome. The user is told before the encode starts, with the option to proceed.

### 7.3 Refused cleanly in v1

**Thai/Lao/Khmer/Burmese line breaking** — `unicode-linebreak` resolves Complex-Context characters to Ordinary Alphabetic, so these scripts have **no break opportunities at all** and a Thai cue is one unbreakable run that overflows silently. Shaping and marks are fine; only wrapping is broken. v1: render the text, disable automatic wrapping for these scripts, raise `Caption::NoLineBreakOpportunities`. **Overflowing silently is the one outcome not permitted.** The v2 fix is known and costed — ICU4X's `LineSegmenter` carries dictionary/LSTM data for exactly these scripts — but requires feeding cosmic-text pre-segmented runs, which is a render-path change.

**Karaoke FillSweep on RTL** — a left-to-right intra-glyph split sweeps backwards on an RTL run. v1 **degrades to WordPop for RTL runs**, which is a binary colour swap and therefore direction-agnostic — correct rather than merely less wrong — and reports the substitution once per cue. This replaces [06 §5.2](06-captions-ai.md)'s parenthetical dismissal with a defined behaviour.

**Paragraph direction is stored, not inferred.** `Auto` derives direction from the first strong character, which is wrong for a common case: an Arabic caption beginning with a Latin brand name lays out backwards. `CaptionStyle` gains an explicit `TextDirection`, defaulting to `Auto`, set from the track language by auto-caption.

**Vertical Japanese layout** — not expressible in the current text stack. Refused with a clear message.

---

## 8. Testing

**Three pseudo-locales**, generated from `en-US` at build time, using **private-use tags only** (a real product once used `tk-TM` for a pseudo-locale, which later became a shipping locale and broke their builds):

| Locale | Transform | Catches |
|---|---|---|
| `en-XA` | Accent, expand, wrap in `[ ]`, mark placeholders | Truncation (missing `]`), **concatenation** (one sentence in two bracket pairs), **hardcoded strings** (they render unaccented) |
| `en-XB` | RLM/RLO wrapping per word | BiDi handling — and a standing demonstration of §3.2 rather than a bug to fix |
| `en-XC` | **Photonic-specific.** Pad with Greek, Cyrillic, Han, Devanagari and Arabic | **Missing font coverage / tofu**, in the UI *and*, applied to caption fixtures, **in exported video frames** |

`en-XC` is the one that matters most here and does not exist off the shelf. A missing glyph in the UI is embarrassing; a missing glyph in a deliverable is a re-render. ASCII `[!!! … !!!]` padding cannot find it.

**Expansion rule:** +100% over 3 words, +50% for 3 or fewer — deliberately conservative on short strings so the pseudo-localized UI stays usable.

**Script fixtures**, one per failure mode, rendered headless per [11](11-testing-phasing.md): `caption_ja` (cell counting, cluster distribution, JP-not-SC Han forms) · `caption_zh_hans` · `caption_ko` (mixed-width arithmetic) · `caption_ar` (joining, bidi, direction override) · `caption_he` (FillSweep degradation) · `caption_hi` (reordering, cluster reveal) · `caption_th` (**clean refusal, no silent overflow**) · `caption_emoji_zwj`. Each also asserts **no tofu**.

**Property tests:** cell width is monotonic under concatenation, equals `chars().count()` for pure ASCII (proving no regression on existing goldens), is unaffected by inserting combining marks, and is exactly `2n` for `n` CJK ideographs. **Determinism:** cue segmentation is byte-identical across platforms for every fixture.

**Gating:** per [37 §4.2](37-robustness.md#42-recommendation-two-tiers-and-be-honest-about-which-is-which), these are **hard gates** — a localization regression is a correctness regression.

---

## 9. Acceptance

| # | Test |
|---|---|
| 1 | A Japanese cue of 13 full-width characters fits one line; 14 wraps. **The current code allows 42** |
| 2 | An Arabic caption with an embedded Latin brand name renders with correct joining and bidi ordering |
| 3 | A Devanagari cue under Typewriter never renders a combining mark without its base at any tick |
| 4 | A Thai cue exceeding `max_width` raises a warning and does **not** overflow silently |
| 5 | Cue segmentation is byte-identical on Linux, macOS and Windows for every fixture |
| 6 | Removing the CJK system font makes export report `Render::MissingGlyph` and refuse to complete silently — it does not emit boxes |
| 7 | A caption exported on a machine lacking its authored font reports `Render::FontSubstituted` naming both faces |
| 8 | A `ja` track renders Japanese Han forms, not Simplified Chinese forms, on a machine reporting `ja-JP` |
| 9 | Under `en-XA`, no string is truncated and no sentence appears split across two bracket pairs |
| 10 | Under `en-XA`, every string in the [29](29-qa-spec.md) AS walkthroughs renders accented — an unaccented string is a hardcoded literal and fails the build |
| 11 | Under `en-XC`, no UI surface and no exported caption frame contains tofu |
| 12 | Timecode fields are byte-identical ASCII under every shipped and pseudo locale, and round-trip through `parse_timecode` |
| 13 | Selecting `ar` as a UI locale is refused with a stated reason; Arabic **captions** remain fully functional in the same session |
| 14 | Switching UI locale at runtime translates the app on the next frame, no restart |
| 15 | `Shaping::Basic` appears at no call site and is reachable through no setting |

Tests 1, 5 and 12 are the ones that would have caught the three defects this document exists to fix. **Test 5 is the most valuable** because it is a property rather than a number, so it holds on any machine and on scripts nobody wrote a fixture for.

---

## 10. Sequencing

| Order | Item | Rationale |
|---|---|---|
| 1 | Cell-width budget, tokenization, terminators, grapheme reveal (§6.3, §6.5) | Pure core/video change, no render work, **no new dependencies**. Closes the substantive half of MC-8. Ship first |
| 2 | `CaptionTrack.language` + budget table + cps gate (§6.4) | Additive serde; rides the consolidated v5 step ([01 §9.1](01-data-model.md#91-the-v4--v5-migration--one-step-nine-changes)). Needed before item 1's budgets can be anything but the Latin default |
| 3 | Shaping-mode assertion + script fixtures (§7.1, §8) | Locks in what already works before anything can regress it. Cheap |
| 4 | Substitution + tofu reporting wired to [36](36-error-model.md) (§7.2) | Reuses the existing vector-path warning. Highest support value per line, and closes a silent-wrong-export path |
| 5 | Locale normalization + Photonic `Fallback` + Arabic/Devanagari bundling | After 4, because 4 is what makes the fix observable |
| 6 | `TextDirection`, FillSweep RTL degradation, Thai refusal (§7.3) | Completes Tier A |
| 7 | Fluent scaffolding + pseudo-locales + the ~421 direct widget literals | Tier B mechanism. Large but mechanical and parallelisable |
| 8 | ICU4X + human-format conversion + binary-size measurement into [25](25-performance.md) | After 7, so there is a catalog for plural forms |
| 9 | The remaining ~1,900 strings | Ongoing; gated by test 10 |

Item 1 first because it is the only item that changes what ships **inside a customer's exported video**, needs no new dependency, and is independent of everything else.

**Definition of done:** every §9 row passes. *(The [06](06-captions-ai.md) amendments below were applied 2026-07-20.)* [06 §3.5](06-captions-ai.md)/[§5.2](06-captions-ai.md) point here for the budget unit, the RTL position and the Thai refusal; [06 §5.3](06-captions-ai.md)'s claim that render-time wrap consults `max_chars_per_line` is deleted as incorrect (§6.2); and `max_chars_per_line` exists nowhere in the workspace.
