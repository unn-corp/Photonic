# UNNDEV UI Standards Core

Version: 1.0  
Research date: 30 September 2026  
Role: public-safe distribution edition of the shared UI standard  
Audience: agents, designers, implementers and reviewers in adopting repositories  
Applicability: effective within a repository's recorded adoption scope; not a claim of existing compliance

This edition preserves the substantive common contracts, stable rule IDs, accessibility map and public research of the comprehensive 1.0 standard. It intentionally excludes cross-project inventories, private source citations, product-specific design profiles and confidential decisions. The research master remains background evidence; this distribution is the common-rule text that adopting repositories consume. Local profiles specialize visual identity and platform behavior without becoming competing organization-wide standards.

A repository's adoption record identifies this filename and version, its upstream source or verified content hash, adoption scope and local profile/exception paths. Do not invent an upstream URL before the canonical file is installed. Installation is not evidence that existing screens pass the requirements, and this document does not independently authorize implementation, publication or changes beyond the requested adoption.

## Navigation

Read the quick start and authority rules together with the adopting repository's local profile. Implement with stable rule IDs and component contracts; review with the acceptance gates, accessibility map and change-report template.

- [Read this first](#read-this-first)
- [Research basis and scope](#research-basis-and-scope)
- [Agent quick start](#agent-quick-start)
- [Authority and one canonical home](#authority-and-one-canonical-home)
- [Shared component and visual system](#shared-component-and-visual-system)
- [Accessibility baseline](#accessibility-baseline)
- [Interaction and asynchronous state](#interaction-and-asynchronous-state)
- [Layout and content behavior](#layout-and-content-behavior)
- [Component contracts](#component-contracts)
- [Navigation safety and recovery](#navigation-safety-and-recovery)
- [Data and complex workspace contracts](#data-and-complex-workspace-contracts)
- [Content localization and inclusive presentation](#content-localization-and-inclusive-presentation)
- [Tokens and implementation governance](#tokens-and-implementation-governance)
- [Performance and reliability budgets](#performance-and-reliability-budgets)
- [Verification and release contract](#verification-and-release-contract)
- [Adoption and maintenance workflow](#adoption-and-maintenance-workflow)
- [What to take from the reel](#what-to-take-from-the-reel)
- [Acceptance gates](#acceptance-gates)
- [Accessibility applicability and test map](#accessibility-applicability-and-test-map)
- [Source register](#source-register)
- [Revision history](#revision-history)

## Read this first

Build interfaces from the project's existing components and semantic tokens. Preserve its visual identity, make actions and state truthful, support the relevant input and accessibility paths, and verify the result in context. Do not copy a visual recipe into every product.

This is the shared-rule entry point for adopting repositories. Public standards retain their own normative status; design-system references are evidence for the internal rules, not a claim that every cited source mandates every UNNDEV test case.

- **Shared rules** apply within the recorded adoption scope. MUST, SHOULD and MAY express their force once adopted.
- **Local profiles** retain the repository's approved tokens, component ownership, platform constraints and feature contracts.
- **Observed implementation** is evidence of current behavior, not automatic proof of approval or compliance.

MUST means required within an adopted rule's scope. SHOULD allows a documented, justified exception. MAY means optional. A standards rule is not permission to redesign, publish, deploy or change unrelated project instructions.

## Research basis and scope

Research checked on **30 September 2026**. “Modern” here means current, supported and testable, not whichever visual trend is newest. The standard combines shared engineering principles with current primary guidance; a mature pattern is still useful when its publication date is old.

- **Normative web baseline:** WCAG 2.2 Recommendation, including all applicable Level A and AA criteria and its conformance requirements. Individual tests in this document are implementation checks, not a substitute for that standard.
- **Semantic implementation:** native HTML and the stable WAI-ARIA specification take priority over inventing a custom widget. APG is informative implementation guidance; adopting a role means implementing its behavior, not merely adding an attribute.
- **Design-system evidence:** GOV.UK contributes clear transactional forms and recovery patterns; Fluent contributes contextual enterprise surfaces; Apple contributes native interaction expectations; Android/Material contributes touch and progress behavior. Their visual tokens and every platform-specific behavior are not imported wholesale.
- **Interoperability:** DTCG 2025.10 is a stable Community Group specification for token interchange, not a W3C Recommendation, a prescribed palette or a reason to replace functioning token systems. [N1]
- **Performance evidence:** Google's Web Vitals guidance is a useful web field-performance baseline. It is not a native/game frame-rate standard or proof that a long-running editor is responsive. [N2]
- **Product judgment:** the detailed contracts below are UNNDEV policy for repositories that adopt this edition, informed by the public sources listed here. Sources support patterns; UNNDEV-specific scope, prioritization and acceptance gates are internal policy choices whose applicability is recorded at adoption.

### Current accessibility standards status

- **Web release target: WCAG 2.2 Level AA**, meaning applicable Level A and AA success criteria, plus the conformance requirements. The current published Recommendation is dated **12 December 2024**; WCAG 2.2 was first recommended on 5 October 2023. Pin the dated Recommendation in the standard and periodically review errata. [Normative Recommendation](https://www.w3.org/TR/2024/REC-WCAG22-20241212/), [current version](https://www.w3.org/TR/WCAG22/), [W3C overview](https://www.w3.org/WAI/standards-guidelines/wcag/)
- **WCAG 3 is not the release baseline.** The current published version is the **10 September 2026 Working Draft**, incomplete and subject to substantial change. Its conformance model is under development; experimental WCAG 3 work must be labeled separately. [Dated draft](https://www.w3.org/TR/2026/WD-wcag-3.0-20260910/), [introduction updated 25 September 2026](https://www.w3.org/WAI/standards-guidelines/wcag/wcag3-intro/)
- **WAI-ARIA 1.2** is a normative Recommendation dated **6 June 2023**. **ARIA in HTML**, defining permitted author use of ARIA on HTML elements, is a Recommendation updated **11 August 2026**. ARIA semantics do not implement keyboard interaction, focus management, or visual behavior. [WAI-ARIA 1.2](https://www.w3.org/TR/wai-aria-1.2/), [ARIA in HTML](https://www.w3.org/TR/2026/REC-html-aria-20260811/)
- **ARIA Authoring Practices Guide (APG)** is informative, has no conformance model, and its examples are educational rather than production-ready components. Adopt appropriate APG keyboard conventions as explicit UNNDEV component requirements, while distinguishing that internal choice from WCAG's normative requirements. Test actual browser/assistive-technology interoperability. [APG introduction](https://www.w3.org/WAI/ARIA/apg/about/introduction/), [APG limitations](https://www.w3.org/WAI/ARIA/apg/practices/read-me-first/)
- **Understanding, Techniques, and ACT Test Rules are informative implementation/testing aids.** A passing automated rule is only evidence about that rule's limited scope. It is not a complete criterion pass or AA certification. [ACT limitations](https://www.w3.org/WAI/WCAG22/Understanding/understanding-act-rules.html)


### Native applicability boundaries

**Native web controls:** “Browser default” is not a blanket accessibility exemption. The specific target-size, hover-content, non-text-contrast and dragging exceptions have bounded conditions, typically that the relevant presentation/function is UA-determined and author-unmodified. Styling or scripting can invalidate the claimed exception. Test actual native controls on supported browsers and mobile AT; semantic HTML reduces work but does not remove verification obligations.

**Non-web native applications/documents:** use a separate platform applicability profile. WCAG2ICT is a **Group Note dated 11 December 2025**, informative guidance for adapting WCAG 2.2 to non-web software/documents, not an independent normative certification standard. Translate web assumptions to actual platform accessibility APIs, input systems, density-independent measurements, text scaling, and focus mechanisms; use criterion-by-criterion reasoning. Closed functionality may require built-in alternatives because external AT cannot be installed/attached. WCAG alone does not establish accessibility of all such systems or hardware. [WCAG2ICT](https://www.w3.org/TR/2025/NOTE-wcag2ict-22-20251211/), [closed functionality](https://www.w3.org/TR/wcag2ict-22/#closed-functionality)


### Applicability tags

**Applicability rule:** every new UI change MUST declare its applicable surfaces before implementation:

- **ALL:** shared outcomes across products; implement with the native platform's mechanisms
- **WEB:** browser UI, including an Electron web view; use CSS pixels and HTML/ARIA semantics
- **NATIVE:** operating-system app UI; use the framework's accessibility, text sizing, focus and platform units
- **TOUCH:** coarse-pointer use, on web or native; include assistive touch operation
- **EDITOR:** dense authoring, table/grid, canvas, timeline or IDE interactions
- **GAME:** runtime gameplay UI; use the governing input, observation and authority contracts

A surface may carry several tags. A web-based editor is WEB+EDITOR; a native game runtime is GAME plus its supported native input/accessibility environment; a browser catalog is WEB. Lack of a requested mobile product does not make a responsive web page exempt from keyboard or web conformance requirements. Native conformance claims require a stated native evaluation method; do not claim WCAG web certification solely from equivalent design intent.

### Rule interpretation

The `CMP`, `NAV`, `SAFE`, `DATA`, `WORK`, `CONTENT`, `PERF`, `TOKEN`, `VERIFY` and `GOV` IDs below have the same meaning as in the comprehensive 1.0 standard. Their normative force is limited to the repository's recorded adoption scope. The source register and research sections state whether external guidance is normative, informative or platform-specific. Existing rule IDs remain stable; revisions change wording with a revision note rather than recycling an ID for a different requirement.


## Agent quick start

1. Identify the product, surface, target branch and applicable local instructions. Read the adopting repository's local profile and governing sources.
2. State the user task, the relevant states and input methods, and the existing component/token owners before designing anything new.
3. Reuse a component and its named variant. Keep project colors, typography, density, radii and motion. Escalate contradictions instead of silently choosing a new visual system.
4. Implement truthful idle, focus, pressed, unavailable, pending, success, failure and recovery behavior as applicable. Never equate an animation with a completed operation.
5. Check accessibility, compact layouts, theme variants and adverse states. Record passed, failed and untested checks separately.
6. Give the reviewer the change, source revision, affected surfaces, evidence and unresolved exceptions. Do not claim rollout or compliance from this document alone.

## Authority and one canonical home

**Distribution governance.** Keep one canonical versioned shared-rule file named `UNNDEV-UI-STANDARDS-CORE.md`. Project design documents remain local profiles, not competing organization-wide masters. Once a repository home is approved, other repositories should link to the master and record the version they adopt; each distributed copy must identify the canonical distribution identity, version and verified content hash, and must not be independently edited. Include an upstream URL only when it is appropriate and accessible to that repository's readers; never disclose a private repository URL to a broader audience.

Until adoption, follow existing project instructions. After adoption, resolve UI decisions in this order:

1. Applicable accessibility obligations and security/privacy constraints.
2. The adopted shared baseline in this document, including its explicit exception/change process.
3. Owner-approved project/surface decisions, profiles and feature contracts, which specialize the baseline without quietly weakening it.
4. Component and token implementation, as evidence of current behavior.
5. Historical plans, research and external inspiration.

Where sources disagree, report both and obtain a decision for the conflicting part. Current code does not automatically supersede an approved contract; an old document does not automatically authorize undoing the current UI. Later explicitly superseding decisions win over earlier versions within the same project scope. Preserve provisional values as provisional.

**Exception record:** rule ID; project and surface; exact deviation; user need; alternatives considered; accessibility effect; approving owner/decision link; verification; review trigger. Missing accessibility coverage is a tracked gap, not a brand exception or a pass.

## Shared component and visual system

The following are shared rules for adopting repositories, grounded in component ownership and semantic-token principles; each repository supplies its local profile.

- **SYS-01 MUST:** use semantic roles such as background, surface, text, muted text, action, focus, border, success, warning and error. Resolve their values through the local theme. Do not scatter new literal colors, radii or durations across feature files.
- **SYS-02 MUST:** reuse local primitives before creating equivalents. Generic appearance belongs in a named component variant; feature-specific composition belongs with the feature. Layout belongs in the layout owner. Do not introduce a second UI library for a single control without a reviewed need.
- **SYS-03 SHOULD:** use the established spacing and type scale. Align labels, fields and actions consistently; avoid arbitrary one-off offsets and decoration. Dense workspaces may remain dense when hit areas and readability are preserved.
- **SYS-04 MUST:** distinguish primary, secondary and destructive actions through meaning and hierarchy. Do not make every control visually primary. Keep an escape or reverse path where the operation supports one.
- **SYS-05 MUST:** preserve aspect ratios and stable identities for icons, previews and media. Missing or unapproved assets get a clear fallback, not a misleading replacement. Use approved assets with recorded rights.
- **SYS-06 SHOULD:** use sentence-case, concise action labels. Prefer a verb and meaningful object when needed, such as “Export image” or “Save changes.” Established compact icon controls remain valid when they have accessible names and discoverable explanations.

### Control anatomy

Choose each control deliberately: hit area, visual bounds, padding, label and icon, shape, contrast, focus and state feedback. Define dimensions through component tokens; let translated text, zoom and long labels fit without clipping. Align icons optically, keep a consistent gap from text, and avoid shifting neighboring controls when a label becomes a spinner or result.

**Fallback defaults for a new web surface with no existing profile:** 44px minimum main-control height and 44×44px coarse-pointer hit area, an 8px icon gap, and the local body/label type scale. These are product defaults, not WCAG minimums. A 48px prominent call-to-action is optional. Existing project profiles take precedence over these fallback dimensions; small controls still need appropriate usable target areas and spacing.

## Accessibility baseline

**Shared target for adopting web surfaces:** WCAG 2.2 Level AA for web UI. These ten working rules are a practical subset; the complete 55-criterion A+AA applicability and test map later in this document must also be reviewed. Neither is a completed conformance audit. Native desktop, mobile and game UI should apply equivalent outcomes through their platform accessibility and input systems; CSS pixels are not engine units, device pixels, points or Android dp.

- **A11Y-01 MUST:** normal text reaches 4.5:1 contrast; large text reaches 3:1 under WCAG's large-text definition. A 17px semibold label does not qualify for the large-text exemption. Measure actual foreground/background pairs, including themes, opacity and interactive states. Inactive controls and other WCAG exceptions are specific exceptions, not permission to make instructions unreadable. [W1]
- **A11Y-02 MUST:** visual information required to identify a control or its state reaches 3:1 against adjacent colors. A decorative border is not automatically subject to this rule; a border needed to find an input is. Do not require every shadow, hover embellishment or decorative edge to reach 3:1. Name decorative exemptions explicitly. [W2]
- **A11Y-03 MUST:** provide keyboard operation for functionality except genuinely path-dependent input, predictable focus order, a visible focus indicator and a way out of modal states. Native buttons/links are preferred on the web; buttons activate with the standard Enter/Space behavior. Hover is never the only way to discover a required action. [W3, W4]
- **A11Y-04 MUST:** focused controls are not entirely hidden by author-created content, including sticky headers and footers. **SHOULD:** keep the full focus indicator and control visible. Do not describe WCAG's stricter AAA Focus Appearance geometry as an AA requirement. [W5]
- **A11Y-05 MUST:** web pointer targets meet the 24×24 CSS-pixel AA requirement or one of its defined spacing, equivalent, inline, user-agent or essential exceptions. **SHOULD:** use the larger project touch target. Android guidance calls for at least 48dp touch targets, even beyond visual bounds. Do not convert the reel's 48px visual height into a universal platform rule. [W6, W7]
- **A11Y-06 MUST:** expose a meaningful accessible name, role and state; the accessible name includes the visible label. Decorative icons are ignored by assistive technology. Icon-only controls need a meaningful name. Associate field labels, help and errors with their inputs; placeholders do not replace labels. [W8]
- **A11Y-07 MUST:** status information is available to assistive technology without forcing focus to a toast. Use an appropriate status/live-region mechanism on the web, with restrained announcements. Success, errors, selection and freshness never rely solely on color, motion or a brief icon flash. [W9]
- **A11Y-08 MUST:** preserve content and operation during text enlargement and narrow reflow. Test web text resize at 200% and reflow at a 320 CSS-pixel viewport equivalent; intrinsically two-dimensional content has scoped exceptions, not a blanket exemption for the surrounding controls. [W10, W11]
- **A11Y-09 MUST:** provide a single-pointer alternative without dragging for drag-based web actions, except the defined essential/user-agent cases. A keyboard-only alternative does not by itself satisfy that requirement. Preserve keyboard and supported controller alternatives for game commands; do not infer controller acceptance from mouse testing. [W12]
- **A11Y-10 MUST:** respect reduced-motion preferences and retain state meaning with non-motion feedback. Remove unnecessary travel, parallax and looping effects. This product rule is stronger than simply claiming AA; Animation from Interactions is a WCAG AAA criterion. [W13]

## Interaction and asynchronous state

These are shared behavior and state-truth rules for adopting repositories.

- **STATE-01 MUST:** distinguish activation, request submission and confirmed completion. A pressed treatment means the input registered. “Saved,” a success checkmark or “Done” requires a real successful result.
- **STATE-02 MUST:** prevent accidental duplicate consequential requests. If an action is temporarily unavailable, explain why in a reachable location. Do not remove focus unpredictably when entering pending state.
- **STATE-03 MUST:** preserve useful user input on failure. Show the cause and an appropriate next step: retry, correct an input, reconnect, cancel or inspect details. Do not loop retries invisibly.
- **STATE-04 MUST:** handle delayed responses, rejection, cancellation, disconnect/reconnect and stale data. Label cached/last-known data when its freshness matters. Cancel must state whether it cancels the operation or only closes the view.
- **STATE-05 MUST:** retain context across updates where appropriate: selection, scroll, draft and expanded state. Never let a late response replace a newer user's selection or claim success for the wrong item.
- **STATE-06 SHOULD:** show local input acknowledgment within roughly 100ms and use short, interruptible transitions, typically 150–250ms where the project has no token. This is an internal responsiveness target, not a backend SLA or a universal animation law. Use the existing approved project motion tokens where applicable.
- **STATE-07 MUST:** a 300ms animation target must never become a promise that an upload, save, export or server command finishes in 300ms. Keep an honest pending state until the operation resolves. Show determinate progress only when it is known; otherwise use a labeled indeterminate state and useful recovery behavior.
- **STATE-08 SHOULD:** avoid unnecessary continuously repainting animation. Animate only where it explains change; stop unnecessary work when hidden or settled. Measure representative dense and long-lived screens, not just an empty happy path.

### Required state checklist per interactive component

Default; hover where available; keyboard focus; pressed; selected/toggled where applicable; disabled/unavailable and reason; pending; success; error/retry; canceled; stale/disconnected where applicable. Mark genuinely inapplicable states rather than implementing meaningless variants. Hover and focus may coexist; selected and pressed are different states.

## Layout and content behavior

These are shared rules for adopting repositories.

- **LAY-01 MUST:** preserve required actions, labels and recovery controls at supported widths, text scales and safe areas. Use intentional reflow, wrapping, overflow or a labeled alternate view instead of clipping or shrinking text until it becomes unreadable.
- **LAY-02 MUST:** keep context and hierarchy stable across loading and selection changes. Use one coherent shell per surface. Information panels must not unexpectedly pause a live game or take over unrelated navigation.
- **LAY-03 SHOULD:** show the important state and next action first. Move advanced explanation into a reachable disclosure without hiding essential blockers, consequences or required controls.
- **LAY-04 MUST:** choose the right container. A modal is for an interrupting decision; a drawer/inspector retains context; a world-attached callout relates directly to game geometry. Preserve each project's established interaction contract.
- **LAY-05 MUST:** test long content, empty content, loading, errors, unavailable information and realistic populated data. Unknown is not zero; an uncollected value is not a successful empty result.

## Component contracts

A reusable component MUST publish its purpose, semantics, supported variants, inputs/outputs, controlled state, disabled/read-only behavior, keyboard/focus behavior, async behavior, responsive limits and verification cases. A screenshot alone is not its contract. The following contracts apply when that component exists; do not build components a product does not need.

### CMP-01 Buttons and links

**Applies: ALL; HTML details WEB. MUST:** use a link for navigation and a button for an action. Preserve ordinary link behaviors such as open-in-new-tab where meaningful. Set button type deliberately inside forms. Expose toggle state without changing the meaning of its label unpredictably. A disabled appearance must match actual behavior; `aria-disabled` alone does not suppress activation.

**SHOULD:** name the actual consequence, keep one visually primary action per local decision, and avoid disabling a submit button merely to conceal what information is missing. If an action cannot run, give an accessible reason near the action or relevant fields. A visual spinner must not replace the control's meaningful name. [N3, W4]

**Verify:** keyboard and pointer activate once; link modifiers work; accidental Enter does not submit an unrelated action; pending/disabled states suppress duplicate work and retain discoverable context; accessible names match visible copy.

### CMP-02 Text fields and validation

**Applies: ALL. MUST:** provide a persistent label, expected format when needed, explicit required/optional meaning, and useful error text associated with the field. Group related fields semantically. Preserve valid answers after failure and distinguish validation, authorization, network and server errors. Client-side checks do not replace server validation.

**SHOULD:** validate at a useful moment, not aggressively while someone is still composing a value. On submitted forms with multiple errors, present a focused/navigable error summary and field-level errors; link each summary entry to the problem. Do not erase entered values to redisplay an error. GOV.UK's page-level summary pattern is strong evidence for transactional forms, not a mandate to move focus on every inline keystroke. [N4, N5, N6]

**Verify:** blank, malformed, boundary-length, server-rejected and corrected inputs; autofill/paste; input-method composition; screen-reader label/help/error; focus after failed submit; retention of unrelated valid values.

### CMP-03 Choice controls

**Applies: ALL. MUST:** use checkboxes for independent choices, radio groups for one-of-many choices and switches for clearly labeled on/off settings. Define whether changes apply immediately or after Save; do not surprise users with a consequential action when merely changing focus. Group names and selected/mixed/checked states must be programmatically available. A switch's label describes the setting and stays stable as its state changes. [N7, N8, N9]

**SHOULD:** expose a small set of mutually exclusive choices directly when that improves comparison. Do not invent a segmented control that looks like tabs but submits changes invisibly.

**Verify:** all choices are reachable with the conventional keyboard pattern; mixed selection is distinct from off; screen readers announce the group and state; cancel/revert behaves as labeled; disabled choices explain relevant blockers.

### CMP-04 Selects and comboboxes

**Applies: ALL; ARIA details WEB. MUST:** distinguish selection from free-text entry. A custom combobox owns accessible naming, expanded state, active option, selection, escape/dismissal and keyboard behavior. Preserve platform text-editing commands. An async result list must not select a stale result from an older query. [N10]

**SHOULD:** prefer a native select or existing proven primitive when it meets the task. Use search for genuinely large option sets, with clear no-results and unavailable states. Do not treat an empty search as a system failure or “loading” as a selectable result.

**Verify:** keyboard selection/cancel, free text when supported, duplicate labels with distinguishable identity, slow/out-of-order results, composition input, no matches, clearing and restoring a value, touch assistive technology and focus after selection.

### CMP-05 Dialogs and drawers

**Applies: ALL. MUST:** explicitly declare modality. A modal has a name, deliberate initial focus, a reachable exit/cancel path, contained interaction and restoration to the trigger or a logical successor. Background content must behave as inert, not merely look dimmed. A non-modal inspector preserves access to its context. Avoid nested blocking surfaces; if a product genuinely needs them, specify the focus stack and recovery. [N11, N12]

Initial focus depends on content: a heading or static introduction may be appropriate for complex reading; a safe action for irreversible confirmation; the first useful field for a short task. Do not universally focus the first destructive action. Escape closes ordinary modal tasks unless a documented safety/task constraint requires an explicit choice. Never dismiss and discard a dirty draft silently. Do not adopt one design system's alert-dismissal convention as a universal requirement.

**Verify:** open/close by supported inputs; reverse tab; screen-reader name; background isolation; focus return after deletion of the trigger; zoom/virtual keyboard; scrollable long content; dirty/pending close; route change; no click-through to the underlying view. Drawer resizing must not hide required controls. [N13]

### CMP-06 Menus and contextual actions

**Applies: ALL. MUST:** use action-menu semantics only for actual action menus; ordinary website navigation remains navigation. Opening transfers focus according to the chosen menu pattern; arrows, activation and Escape work consistently; dismissal restores useful focus. A context menu cannot be the only route to an essential action. [N14]

**SHOULD:** group related actions, separate destructive actions and show shortcuts where useful. Do not execute an action when users merely open a submenu. Overflow must retain important authorized actions and current context.

**Verify:** keyboard and pointer entry, outside dismissal, nested-menu return, unavailable actions, long labels, viewport edges and context changes while open. The action still targets the item whose menu is open, not a newer selection.

### CMP-07 Tabs disclosures and tooltips

**Applies: ALL. MUST:** tabs switch related panels and expose selection/relationships; disclosures expand content and expose expanded state. Use manual tab activation when automatic activation would cause noticeable loading delays. Important instructions and errors remain available without hover. [N15, N16]

**SHOULD:** use tooltips only for supplemental brief explanations. Keep focus on the trigger and make hover/focus content dismissible, hoverable and persistent as applicable under WCAG. Interactive popup content needs an appropriate popover/dialog pattern instead of a focusable “tooltip.” The APG tooltip page is explicitly work in progress without task-force consensus; WCAG and tested component behavior remain the baseline. [N17]

**Verify:** arrows and activation in tabs; panel headings and hidden-state behavior; disclosure Enter/Space; Escape and pointer travel for hover content; touch access to equivalent explanations; state preservation when switching panels.

### CMP-08 Sliders and numeric editing

**Applies: ALL, especially EDITOR. MUST:** expose label, value, units, range and step; keep pointer, keyboard and text-entry values consistent. Validate out-of-range and non-finite values without silently corrupting data. Distinguish live preview from committed change. Provide a single-pointer non-drag alternative where required. [N18, N19]

**SHOULD:** pair precision-sensitive sliders with direct numeric entry; use locale-aware input/formatting without changing the stored meaning. Do not silently round a precise value merely because its display is shortened. Touch assistive slider support must be tested, not inferred from desktop arrows.

**Verify:** min/max, negative/fractional values where valid, keyboard steps, locale decimal input, cancel/revert, focus loss, drag release outside bounds, stale authority rejection and accessible value text such as time or percentage.

### CMP-09 Loading empty and error states

**Applies: ALL. MUST:** distinguish initial loading, background refresh, no data yet, no search matches, denied access, read-only state, offline/stale content and system failure. Show the affected scope and a useful next action. Preserve loaded content during refresh unless displaying it would be unsafe or misleading. Do not fabricate percentages or add an artificial wait to make animation visible.

Use determinate progress for measurable work and indeterminate status when completion amount is unknown. Progress scope must match the work: one item loading must not disable unrelated content. Skeletons are placeholders, not fake content or additional screen-reader items. [N20]

**Verify:** zero/one/many items; cached refresh failure; lost permission; disconnected startup; partial results; timeout; missing asset; interruption/retry; reduced motion. Any stronger local shared-skeleton contract remains in force.

### CMP-10 Toasts banners and notifications

**Applies: ALL. MUST:** choose severity and persistence by consequence. A transient toast must not be the sole place to recover from a consequential failure or discover an action whose time window is essential. Preserve important outcomes in the relevant screen or history. Status announcements should not repeatedly interrupt reading or steal focus. [N21, N22]

**SHOULD:** use inline feedback near a field/operation, a persistent banner for broader state and a toast for brief nonblocking confirmation. Do not stack duplicate success messages for every background refresh. An Undo affordance must have a real reversal contract; announcing “undone” before confirmation repeats the original state-truth error.

**Verify:** screen-reader announcement once; long translated text; multiple queued messages; dismissal; focus/hover timeout behavior; reopening durable details; urgent vs routine announcements; error still recoverable after a toast vanishes.

## Navigation safety and recovery

### NAV-01 Location and history

**Applies: ALL; browser history WEB. MUST:** make current product, organization/workspace, page and selected object understandable. Keep navigation order and names consistent. Distinguish a back action from closing a local overlay. Preserve meaningful back/forward behavior and deep-link identity where the product supports it; do not put secrets in URLs.

**SHOULD:** keep shareable filter/tab state in an appropriate route/query representation when useful, while leaving sensitive or transient drafts out. Moving between responsive navigation forms must not duplicate focusable controls or remove account/workspace actions. A command palette supplements discoverable navigation rather than replacing it.

**Verify:** direct load, refresh, back/forward, copied link, removed item, permission change, organization switch and narrow layout. Browser history must not reopen a dismissed destructive confirmation unexpectedly.

### SAFE-01 Consequential and destructive actions

**Applies: ALL. MUST:** identify what changes, the exact target/scope and irreversibility before a high-consequence commit. A bulk action states whether it affects selected rows, the visible page or all matching results. Server authorization and concurrency validation remain authoritative. Do not infer permission from a visible button or disabled control.

**SHOULD:** prefer reversible actions and useful review steps over repeated generic “Are you sure?” prompts. Use specific final labels such as “Delete 3 projects,” and safe initial focus where a confirmation is needed. Avoid confirmations for trivial reversible steps merely to claim safety. [N3, N11, N23]

**Verify:** wrong/stale target, mixed permissions, repeated click, partial bulk failure, cancel, navigation, delayed result and retry after an unknown outcome. Unknown outcome requires checking operation status before resubmitting an irreversible request.

### SAFE-02 Undo redo drafts and autosave

**Applies: ALL; especially EDITOR. MUST:** define what Undo restores, its scope and limitations; never advertise reversal of an operation that cannot be reversed. Separate draft, saving, saved, failed and conflicted states. Autosave must not claim persistence before confirmation or discard an unsaved local change after reconnect.

**SHOULD:** use platform undo conventions and label the affected operation. Group a continuous adjustment into a coherent undo step when appropriate; preserve meaningful multi-step history. Make offscreen undo results discoverable. [N24]

**Verify:** undo/redo after selection change, grouped edit, failure, save, reopen and concurrent modification as supported. Define whether undo is local, shared or authoritative. Never let a local undo silently overwrite another user's accepted edit. Test tab/window close warnings and recovery according to the app's actual persistence guarantees.

### SAFE-03 Offline reconnect and conflicts

**Applies: connected products. MUST:** distinguish local intent from accepted remote state. Show connection/freshness status when it changes task safety. Specify which actions can queue offline, whether they are idempotent, and what happens on rejection or conflicting edits. Preserve user drafts and present a comparison/recovery route when merging cannot be safe.

**SHOULD:** reconnect without rebuilding the entire working context. Background refresh should not move a user's selected row, cursor, timeline position or focused control unexpectedly.

**Verify:** response lost after acceptance, repeated reconnect, two devices editing, permission revoked mid-task, an object deleted elsewhere, out-of-order response and authentication expiry. A retry button is not sufficient evidence that retry is safe.

## Data and complex workspace contracts

### DATA-01 Tables and interactive grids

**Applies: WEB+EDITOR and corresponding native data views. MUST:** choose a semantic table for primarily reading/comparing data; choose an interactive grid only when its managed keyboard model is justified and implemented. Provide a meaningful caption/name, row/column relationships and understandable headers. Sorting exposes its current direction and does not silently change selection identity. [N25, N26, N27]

**SHOULD:** align comparable numeric values, state units and keep null, zero, unknown and unavailable distinct. Filtering exposes active constraints and a clear reset; pagination/loading exposes scope and total only when known. A small screen may use a scoped horizontal table scroll or a carefully labeled alternate presentation; do not destroy data relationships just to avoid all horizontal scrolling.

**Verify:** keyboard traversal/edit-mode entry and exit, screen-reader headers, sort/filter retention, bulk scope, changing totals, empty page after deletion, long cells, zoom and visible focus within a scroll container.

### DATA-02 Virtualization and live updates

**Applies: large lists, grids, logs and timelines. MUST:** virtualized items preserve stable identity, focus and accessible position/count where known. Loading more cannot create an endless keyboard trap. A focused item must not silently disappear because it crossed a render-window boundary. Sorting, editing and selection operate on stable IDs rather than row indexes.

**SHOULD:** virtualize only where measurement justifies it, and keep accessible search/navigation or an alternate view when windowing limits discovery. Paginate or bound logs when that better fits the task. Preserve scroll anchoring; do not force-scroll someone reading older messages merely because new content arrived. [N28]

**Verify:** first/last item, very large dataset, rapid fling, browser find limitations, keyboard jump, live insertion/removal, screen-reader navigation, selection across pages and returning to a previously edited item. Record any inability to reach unloaded content.

### WORK-01 Canvas and spatial editing

**Applies: EDITOR and GAME where appropriate. MUST:** separate selection, hover, focus, active tool, preview and committed state. Provide discoverable pan/zoom/reset and cancel behavior. Keep inspectors tied to the correct object and explain locked/hidden/unavailable state. For WEB, spatial content and relationships MUST have the alternatives required by applicable criteria such as 1.1.1 and 1.3.1. Use an appropriate object list, properties, coordinates, relationships or textual summary; feasibility alone is not a WCAG exception. Native/game adaptations follow their declared applicability profile.

**MUST for WEB:** provide the keyboard and single-pointer alternatives required by 2.1.1, 2.5.1 and 2.5.7, applying only their defined exceptions. Genuine freehand path-dependent input has a narrower keyboard exception; it does not exempt all editor operations. **SHOULD:** add direct numeric entry for precision when useful; another conforming alternative may satisfy the requirement. Use a stable coordinate model; screen-space handles and labels remain usable as zoom changes. Do not promise full screen-reader canvas access merely because the toolbar is accessible. [W3, W12, N18]

**Verify:** extreme zoom, viewport edge, overlapping objects, hidden/locked layers, multi-selection, keyboard-only operation, cancel mid-gesture, pointer capture loss, moving target, stale authority and restored session. In games, respect visibility/privacy and command authority rather than exposing unseen world data in an alternate list.

### WORK-02 Timelines and media workspaces

**Applies: timeline/video/audio interfaces. MUST:** label timebase, units and current position; distinguish playhead, selection range, clip position and zoom. Keep playback, seeking, editing and export states separate. A preview render is not a successful final export. For WEB, provide the keyboard and single-pointer alternatives required by applicable criteria for time entry and trim/move operations; “impractical” is not an exception. Numeric entry is one possible alternative, not a universally required widget. Other platforms follow their declared applicability profile.

**SHOULD:** maintain selection and viewport through updates; show snapping state and allow controlled precision; expose clip/layer identity in a structured list. Respect user-controlled playback and reduced motion without erasing time/progress information. Media accessibility requirements depend on the actual media, not simply on using a video component.

**Verify:** long duration, different frame/time formats, unavailable media, export failure, cancel, scrub/seek while loading, edit during playback, keyboard focus, zoom, out-of-view selected clip, and reopen with missing resources. Project-specific timecode/frame behavior requires a local contract, not an invented global rule.

### WORK-03 Toolbars trees and shortcuts

**Applies: EDITOR, GAME and app command surfaces. MUST:** follow the selected platform/widget keyboard pattern. Use a manageable tab sequence with internal arrow navigation for appropriate composite widgets. Distinguish focus from selection. Tree expand/collapse, selection and activation have different meanings. Scope shortcuts to the active context and protect text editing, browser/OS and assistive-technology commands. [N29, N30, N31]

**SHOULD:** expose searchable commands and shortcut help, allow remapping where the product needs it, and display the platform's correct modifier names. Single-character shortcuts must meet WCAG's disable/remap/focus-only provisions; do not fire game commands while users type in an input.

**Verify:** alternate keyboard layouts, IME composition, menu/dialog focus, global-vs-editor scope, repeated key events, chord conflicts, disabled command reason, controller mapping when supported and a visible way to discover/cancel the active tool.

## Content localization and inclusive presentation

### CONTENT-01 Clear language and truthful labels

**Applies: ALL. MUST:** use the product's shared vocabulary consistently. Name the object and outcome; distinguish “Save draft,” “Publish,” “Send” and “Export.” Errors explain what happened and what can be done without blaming the user or exposing private internals. Avoid presenting estimates, cached values or AI-generated suggestions as verified facts.

**SHOULD:** put essential information before optional explanation; write plain, concise sentences; use headings that describe the task; expand unfamiliar abbreviations on first useful occurrence. Do not use ALL CAPS or tiny text to carry long instructions. Images, diagrams and symbols need appropriate alternatives; decorative content should not produce noise. Keep copy and accessible labels in the same review.

**Verify:** a first-time user can identify current state and next action; labels remain meaningful out of context; errors are actionable; success messages match actual outcomes; essential information survives hidden images, grayscale and assistive reading order.

### CONTENT-02 Internationalization

**Applies: ALL; localization details when relevant. MUST for every WEB page:** declare the correct default language, including monolingual products; identify language changes in content when 3.1.2 applies. **MUST:** support Unicode end to end; externalize user-facing strings; use locale-aware number, date, plural and unit formatting. Do not build sentences by concatenating translated fragments. Preserve names and addresses without imposing unsupported cultural assumptions. [N32]

**SHOULD:** use flexible layout and pseudolocalization, including expanded labels, tall scripts and missing-glyph checks. No fixed expansion percentage guarantees every language fits. Test long unbroken identifiers and language-specific line breaking. [N33]

**MUST for RTL support:** use appropriate direction metadata and logical layout properties; isolate unknown-direction user content, including names and paths, with appropriate bidi markup. Mirror directional UI only where its meaning should mirror; media transport, code and technical diagrams need deliberate decisions rather than a blanket flip. [N34]

**Verify:** at least one long-text fixture, one non-Latin script and an RTL fixture when supported; mixed-direction names and numbers; locale decimal input; plural counts 0/1/many; daylight-saving/time-zone display; no text overlap or clipped action. State the supported-locale boundary rather than claiming every locale passed.

### CONTENT-03 Theme and user preferences

**Applies: ALL. MUST:** test meaning and contrast in every supported theme, high-contrast/forced-colors mode where applicable, reduced motion and enlarged text. Preserve explicit user choices across appropriate sessions and provide a recoverable default. Theme changes must not recolor content data or semantic status indiscriminately.

**SHOULD:** follow the system preference until the user chooses an override when consistent with the product profile. Respect tenant policy and locked semantic roles where established. Never infer that a dark palette automatically works in bright light or that a native default theme makes custom controls accessible.

**Verify:** first run, saved override, system change, theme switch while a dialog/menu is open, disabled/focus/selected/error states, images with transparency and charts using categorical colors.

## Tokens and implementation governance

### TOKEN-01 Role and ownership model

**Applies: ALL. MUST:** distinguish raw palette values, semantic roles and component-level decisions. A project may implement these layers differently, but each consumed value needs an owner and meaning. Aliases must resolve without cycles and values must have the correct type/unit. Keep state/theme mappings explicit; do not use a background gradient where an API expects a color.

**SHOULD:** consider DTCG 2025.10 when exchanging tokens between tools. Preserve working local formats when conversion adds no value. Use explicit adapters for CSS, native platform units and engine units; do not equate them numerically. The spec standardizes interchange, not UNNDEV naming or hierarchy. [N1]

**Verify:** schema/type validation where available, alias resolution, generated-output consistency, contrast-pair coverage, all theme variants and consumer builds. Generated files identify their source; agents edit the owner, not a disposable output.

### TOKEN-02 Component change control

**Applies: shared libraries. MUST:** a shared component change includes affected consumers, compatibility risk, relevant interaction/visual tests and migration instructions for breaking behavior. A local feature need cannot silently change every product's default shape or keyboard model.

**SHOULD:** prefer additive variants where they represent a real reusable concept. Deprecate old variants with a replacement and measured migration plan; remove only after consumers are accounted for. Avoid an unbounded “style escape hatch” that defeats token/component ownership.

**Verify:** representative consumer examples, theme/state fixtures, keyboard behavior and bundle/runtime consequences. A shared web package is not a mandate that native editors or game runtimes adopt web technology.

## Performance and reliability budgets

### PERF-01 Measured web outcomes

**Applies: WEB. SHOULD:** target Google's good Core Web Vitals thresholds at the 75th percentile, separated for mobile and desktop: LCP at most 2.5 seconds, INP at most 200ms and CLS at most 0.1. Use real-user measurement when available and record consent/privacy constraints. These are industry guidance, not universal legal limits. [N2]

**MUST:** identify route, device/network class, data volume and measurement method. A single Lighthouse run cannot establish field performance; an INP value is not the same as animation duration or server completion time. Long-lived editing flows need additional task latency and responsiveness measures.

**Verify:** cold/warm navigation, initial data, slow device/network, realistic long session, input during work and font/media loading shifts. Compare before/after under the same fixture and report remaining field-data gaps.

### PERF-02 Interaction and rendering

**Applies: ALL. MUST:** keep expensive computation and avoidable I/O off the input/render-critical path where the platform supports it; bound work, cancel obsolete work and update only affected UI. Preserve immediate input acknowledgment independently from backend completion. Avoid unnecessary continuous repaint or rebuilding every panel on each live event.

**SHOULD:** on web, prefer compositor-friendly animation where appropriate, but verify actual rendering cost; transform/opacity is not a guarantee of zero cost. Profile layout, paint and compositing rather than optimizing by folklore. On native/game clients, use the platform profiler and measure UI cost separately from simulation/rendering. [N35, N36]

**Verify:** typing during streaming, rapid selection, resize/scroll, hidden view, long list, high-refresh display, repeated open/close and sustained background updates. Watch for memory/resource growth and stale subscriptions as well as visible frame drops.

### PERF-03 Product budget record

**Applies: each performance-sensitive feature. MUST:** record a representative device class, dataset/scene, task, metric, baseline, agreed target/regression tolerance and measurement procedure. No universal bundle-size, FPS, memory or export-time number is imposed here.

A useful record names: startup/load; input-to-visible-feedback; search/filter/selection latency; sustained scroll/pan/frame pacing; update frequency; peak and steady-state memory; network payload/work volume; cancellation/recovery. Choose only relevant metrics. If no baseline exists, establish it before claiming an improvement. Budget decisions are versioned with the feature and require remeasurement when workload or platform changes.

**Verify:** repeated comparable measurements and trace evidence; identify noise, warm-up and cache effects. An unmeasured budget is open work, not a pass.

## Verification and release contract

### VERIFY-01 Definition of ready

**MUST before a material UI change:** record the user task, surface tags, governing profile, component owners, state transitions, data/permission boundary, responsive/input matrix, accessibility applicability, failure/recovery behavior and acceptance IDs. For small changes this can be a concise change description; do not create bureaucracy or duplicate permanent documents.

### VERIFY-02 Layered evidence

**MUST:** combine the appropriate layers rather than treating one as complete:

1. Static checks: type/lint/token usage and semantics where supported
2. Component tests: observable behavior, state transitions, names/roles and input interactions
3. Integration tests: real data contracts, permission boundaries, async ordering and recovery
4. Automated accessibility checks: useful coverage with known blind spots
5. Manual keyboard, zoom/reflow, theme/reduced-motion and assistive-technology checks
6. Rendered review: responsive fixtures, long content and visual/state hierarchy
7. Representative usability/performance evaluation for significant new workflows

Use the repository's permitted commands and evidence policy. Do not run forbidden full suites, create media commits, upload traces externally or test a live production account merely because this standard recommends verification.

### VERIFY-03 Minimum change matrix

For each affected surface, record relevant combinations rather than an unbounded Cartesian product:

- Viewport: smallest supported, normal and expanded; safe-area/virtual-keyboard cases
- Content: empty, typical, dense, long label, long identifier, missing asset and partial failure
- State: default/focus/selected/disabled, loading/refresh, success/error, offline/stale/recovery
- Input: keyboard, pointer, touch/assistive touch and controller where supported
- Preference: supported themes, enlarged text, reduced motion and forced colors where applicable
- Data authority: allowed, denied, read-only, stale, removed and concurrent-update cases
- Locale: supported translation, text expansion and bidi cases

**MUST:** document justified N/A entries and untested combinations. Do not turn “not tested” into “not applicable.” Test changes on the final integrated revision, not an earlier build before a later edit.

### VERIFY-04 Completion and defects

**MUST:** give a reviewer the exact revision/build, applicable rule IDs, tests and outcomes, evidence location, known failures, exceptions and residual risk. A blocker that prevents the user from completing a core task, loses data, misrepresents a consequential outcome or breaks required accessibility remains a release decision; cosmetic success does not cancel it.

**SHOULD:** automate stable regressions where economical and retain a repeatable manual procedure for the rest. Accessibility and usability reviews should include disabled users when validating significant workflows; one developer with a screen reader is useful testing, not a substitute for all user research.

### Agent change report template

- Change and user outcome:
- Product and surface tags:
- Governing sources and revision:
- Components/tokens reused or changed:
- Applicable rules and acceptance IDs:
- Behavior and adverse-state coverage:
- Accessibility applicability and manual/automated evidence:
- Responsive/input/theme/locale coverage:
- Performance baseline/result if relevant:
- Passed / failed / blocked / not run / justified N/A:
- Exceptions and owner decisions needed:
- Evidence location allowed by this project:

## Adoption and maintenance workflow

### GOV-01 Adopt deliberately

**Adoption sequence:** record the shared-rule owner, canonical home and version; resolve local source conflicts; classify existing gaps; add authorized entry-point links; and record enforcement scope. Existing projects do not become compliant merely by linking the document. Adopted shared rules apply to new or changed work with explicit treatment of legacy gaps; urgent accessibility or data-loss defects require their own prioritization.

### GOV-02 Keep one authority

**MUST after adoption:** maintain one master, local visual/feature profiles and a small version/exception record. Keep project-specific approved decisions scoped; they cannot silently weaken adopted shared accessibility outcomes. If a real conflict remains, stop that decision and route it to the owner. No new “global” rules in scattered AGENTS notes without reconciling this master.

### GOV-03 Review triggers

**SHOULD:** review sources when a platform/framework or stable accessibility standard changes, a shared primitive changes, a serious regression occurs or a product adds a new input/surface/locale. Record research date and source status. A Working Draft or a new visual trend prompts evaluation, not automatic migration. Recheck adoption and exceptions during meaningful releases; set a fixed recurring cadence only if the owner chooses one.

### GOV-04 Change proposal minimum

A standards change names the problem and affected rules, evidence, affected products, compatibility/accessibility effects, alternatives, migration and verification. Separate editorial clarification from a behavior change. Preserve old rule IDs and a concise revision history. Rollout, repository edits and package publication require their own authorization.


## What to take from the reel

Reference: [R1], “Same button. Six decisions,” as reviewed from the supplied Instagram reel. It is design inspiration, not a standards authority.

- **Shared practice:** inspect anatomy, readable action copy, contrast, understandable depth, coherent icon spacing and a complete interaction-state sequence.
- **Optional recipe only:** 48px height, 24px horizontal padding, 17px semibold, pill radius of half the height and 10px icon gap. They are suitable for some prominent calls to action, not mandatory defaults for dense editor controls or game command decks.
- **Project-dependent:** a lit upper edge, recessed track and downward shadow. A local no-shadow profile excludes this treatment; a component with established subtle depth should be reused rather than duplicated. Other skins need their own decision.
- **Correct the accessibility shorthand:** 4.5:1 is the usual normal-text threshold. A 3:1 edge is required when that visual edge is necessary to identify the control/state, not for every decorative border.
- **Correct the timing shorthand:** use about 100ms as an internal input-feedback target. A short transition may finish by roughly 300ms; the real operation may take longer and must remain honestly pending. Reduced motion still communicates state.

## Acceptance gates

These gates are reusable review criteria for adopting repositories, not a claim that an existing product currently passes them. Run checks permitted and required by the target repository; this document never authorizes broader execution or publication.

1. **UI-AC-01 Source and scope:** identify project profile, source revision, affected surfaces and any unresolved conflict. No unapproved skin change or duplicate primitive.
2. **UI-AC-02 Component ownership:** shared tokens/variants are used consistently; feature composition does not restyle generic primitives. New tokens have named semantic purpose and contrast pairs.
3. **UI-AC-03 Operability:** keyboard navigation, visible focus, modal entry/exit and focus restoration work. Pointer, touch and supported controller paths cover equivalent tasks without duplicate submission.
4. **UI-AC-04 Perception:** verify rendered text and meaningful non-text contrast in applicable themes/states. Icon names, selected state, errors and freshness remain understandable without color. Screen-reader status behavior is exercised on relevant web/native surfaces.
5. **UI-AC-05 Size and layout:** verify target bounds/spacing, narrow and populated layouts, long labels, text scaling, safe areas and project-specific scale/resolution matrices. No clipped required action or hidden focus.
6. **UI-AC-06 Async truth:** test fast success, slow success, rejection, timeout/disconnect, retry, repeated activation, cancellation and navigation during pending work as applicable. Confirm outcomes match actual state and newer intent wins over stale responses.
7. **UI-AC-07 Motion and performance:** reduced-motion mode retains meaning. No unneeded ongoing repaint, layout jumping or state loss. Test a realistic long-lived/dense view; measure regressions if the change affects rendering or data volume.
8. **UI-AC-08 Evidence:** run repository-required focused checks; provide or retain representative before/after evidence for visual changes and a short recording for timing through the project-authorized route when appropriate. Label untested combinations and remaining failures. Automated markup checks do not replace rendered and assistive-technology review.

### Agent completion checklist

- [ ] Read this standard's status and the applicable local profile/instructions
- [ ] Identify existing token and component owners and reuse them
- [ ] Resolve or report source conflicts without inventing approval
- [ ] Cover applicable interaction, async, unavailable and recovery states
- [ ] Complete applicable A+AA test-map rows, including authentication, timing, media and content requirements
- [ ] Verify keyboard/focus, names/status, contrast and hit areas
- [ ] Verify compact layouts, long content, scaling and reduced motion
- [ ] Check all affected entry points, clients, locales and reverse actions
- [ ] Use the relevant component/workspace contracts and performance budget record
- [ ] Run allowed project checks and record evidence against acceptance IDs
- [ ] State passed, failed and untested coverage separately
- [ ] Record any exception with scope, reason, owner and review trigger

## Accessibility applicability and test map

**Verification index, WEB.** Evaluate each row for the actual product scope and record **Pass / Fail / Not applicable with reason / Not tested / Blocked**. A rule record includes criterion ID, applicability, supported browser/OS/assistive-technology versions, automated and manual evidence, owner, remediation and retest. Use the native applicability profile when adapting outcomes to non-web software.

Conformance concerns complete pages and processes, not isolated widgets. Include responsive variations and third-party content in the scoped journey. Verify that relied-upon technology uses are accessibility-supported. Optional unsupported/nonconforming technology must not block access when enabled, disabled or unsupported; audio control, no keyboard trap, pause/stop/hide and flash limits (1.4.2, 2.1.2, 2.2.2, 2.3.1) still apply to all content under the non-interference requirement. A scan, screenshot, component-library claim or this checklist alone does not establish WCAG AA conformance. When evidence is partial, report the specific criteria and environments tested. Formal conformance claims require the standard's scope, date, version/level and technology information. This is not a legal-compliance opinion. [W3C conformance requirements](https://www.w3.org/TR/WCAG22/#conformance-reqs), [Understanding conformance](https://www.w3.org/WAI/WCAG22/Understanding/conformance.html)

The following is a **complete A+AA index for WCAG 2.2 (55 criteria)** with compact implementation/test contracts. The linked Understanding page supplies criterion text, definitions, exceptions, and techniques; the dated WCAG Recommendation remains authoritative. Tests are UNNDEV verification procedures, not a substitute for those full definitions.

### Perceivable

| SC / level | Applicability and required outcome | Minimum practical verification |
|---|---|---|
| [1.1.1 A](https://www.w3.org/WAI/WCAG22/Understanding/non-text-content.html) | Non-text content: equivalent-purpose alternatives; functional images name their action; decorative content is ignored by AT. Apply the specific media, test, sensory, and CAPTCHA rules rather than generic alt text. | Inspect rendered purpose and accessibility tree. Read descriptions in context, including charts, canvas, SVG, icon controls, emoji, and state changes. Validate meaningful data equivalents, not merely alt-attribute presence. |
| [1.2.1 A](https://www.w3.org/WAI/WCAG22/Understanding/audio-only-and-video-only-prerecorded.html) | Prerecorded audio-only needs equivalent timed-media alternative; video-only needs an equivalent alternative or audio track. A clearly labeled media alternative for existing text has an exception. | Inventory media, compare all information against the alternative, and test access to the alternative. |
| [1.2.2 A](https://www.w3.org/WAI/WCAG22/Understanding/captions-prerecorded.html) | Prerecorded synchronized media with audio needs captions, subject to its labeled text-alternative exception. | Watch with sound off: verify accuracy, timing, speaker identification, and meaningful non-speech sounds. A caption track's existence is insufficient. |
| [1.2.3 A](https://www.w3.org/WAI/WCAG22/Understanding/audio-description-or-media-alternative-prerecorded.html) | Prerecorded synchronized media needs audio description or a media alternative, subject to its exception. | Check visual information missing from the soundtrack against a descriptive transcript or description. See the stricter AA requirement below. |
| [1.2.4 AA](https://www.w3.org/WAI/WCAG22/Understanding/captions-live.html) | Live synchronized media with audio needs captions. | Test the live delivery path, caption availability, intelligibility, timing, and speaker/sound information; include embedded third-party players. |
| [1.2.5 AA](https://www.w3.org/WAI/WCAG22/Understanding/audio-description-prerecorded.html) | Prerecorded video in synchronized media needs audio description. A transcript alone does not satisfy this AA criterion. No extra description is needed when existing audio already conveys all relevant visual information. | Listen without viewing; compare against visual content. Verify description playback controls and actual description availability. |
| [1.3.1 A](https://www.w3.org/WAI/WCAG22/Understanding/info-and-relationships.html) | Structure/relationships conveyed visually must be programmatically available or in text. | Inspect headings, lists, landmarks, labels/groups, data-table headers, instructions, required/error states, and relationships in the accessibility tree. Verify correctness, not just presence. |
| [1.3.2 A](https://www.w3.org/WAI/WCAG22/Understanding/meaningful-sequence.html) | When sequence affects meaning, a correct reading order is programmatically determinable. | Read with CSS rearrangements removed and with a screen reader; check responsive layouts, cards, columns, inserted content, and RTL. |
| [1.3.3 A](https://www.w3.org/WAI/WCAG22/Understanding/sensory-characteristics.html) | Instructions cannot depend solely on shape, location, size, orientation, or sound. | Audit instructions such as “click the round button on the right”; supply a label or equivalent identifying text. |
| [1.3.4 AA](https://www.w3.org/WAI/WCAG22/Understanding/orientation.html) | Do not restrict view/operation to one display orientation unless essential. | Complete tasks in portrait and landscape; inspect rotation locks and blocking “rotate device” screens. Document an actual essential-use justification. |
| [1.3.5 AA](https://www.w3.org/WAI/WCAG22/Understanding/identify-input-purpose.html) | For inputs collecting user information with a purpose in WCAG's defined list, expose that purpose when supported by the implementation technology. | Inspect appropriate autocomplete/purpose tokens and test real autofill. A label alone is not equivalent purpose metadata. |
| [1.4.1 A](https://www.w3.org/WAI/WCAG22/Understanding/use-of-color.html) | Meaning, state, required action, and distinctions must not rely only on color. | Remove color mentally or with a diagnostic view; verify text, icons, patterns, or other cues for errors, selection, chart series, and links. |
| [1.4.2 A](https://www.w3.org/WAI/WCAG22/Understanding/audio-control.html) | Automatically playing audio lasting over three seconds needs pause/stop or independent volume control. | Load pages and trigger automatic states; verify controls are reachable and usable with AT while audio plays. Prefer no audible autoplay. |
| [1.4.3 AA](https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html) | Text/images of text: 4.5:1; large text: 3:1. Large means at least 18pt, or 14pt bold, or equivalent script size. Apply incidental/inactive/logo exceptions narrowly. | Measure resolved foreground/background colors across themes, states, gradients and images; include placeholders and errors. Do not round 4.499 to 4.5. Brand colors do not create a general exemption. |
| [1.4.4 AA](https://www.w3.org/WAI/WCAG22/Understanding/resize-text.html) | Text must resize to 200% without loss of information/functionality, except captions and images of text. | Test text enlargement and browser zoom as supported; check clipped labels, controls, menus, dialogs, truncation, and task completion. Do not disable user zoom. |
| [1.4.5 AA](https://www.w3.org/WAI/WCAG22/Understanding/images-of-text.html) | Use actual text where the technology can achieve the visual presentation, unless customizable or essential. | Inspect banners, labels, headings, promotional graphics, and screenshots used as instructions; document logo/essential exceptions. |
| [1.4.10 AA](https://www.w3.org/WAI/WCAG22/Understanding/reflow.html) | Preserve information/functionality without two-dimensional scrolling at 320 CSS px width for vertical content, or 256 CSS px height for horizontal content. Only genuinely two-dimensional regions are excepted. | Test actual viewport size, commonly 1280px viewport at 400% zoom. Check overlays and individual table-cell text; a wide table does not exempt the whole page. |
| [1.4.11 AA](https://www.w3.org/WAI/WCAG22/Understanding/non-text-contrast.html) | Necessary visual information identifying UI components/states and meaningful graphics needs 3:1 contrast against adjacent colors, with specific inactive/unmodified-UA/essential exceptions. | Check control boundaries where necessary, icons, selected states, focus indicators, chart marks, all themes and overlays. Do not interpret as “every border must be 3:1.” |
| [1.4.12 AA](https://www.w3.org/WAI/WCAG22/Understanding/text-spacing.html) | Supporting markup must tolerate user overrides simultaneously: line height 1.5×, paragraph spacing 2×, letter spacing 0.12×, word spacing 0.16× font size, with language/script applicability. These are override tests, not required default typography. | Apply only those spacing changes; inspect clipping, overlap, missing text, and controls in every relevant state. |
| [1.4.13 AA](https://www.w3.org/WAI/WCAG22/Understanding/content-on-hover-or-focus.html) | Additional hover/focus content must be dismissible without moving trigger focus/pointer where required, hoverable, and persistent until trigger removal, dismissal, or invalidity. Specific non-obscuring/error and unmodified-UA exceptions apply. | Test tooltips, hover cards and submenus with pointer, keyboard and magnification: move onto popup, wait, dismiss with Escape, and inspect obscured content. |

### Operable

| SC / level | Applicability and required outcome | Minimum practical verification |
|---|---|---|
| [2.1.1 A](https://www.w3.org/WAI/WCAG22/Understanding/keyboard.html) | All functionality is keyboard-operable without specific keystroke timing, except truly path-dependent underlying functions. | Complete every task using keyboard only, including scroll containers, complex widgets, reordering, charts and editors. MouseKeys is not a keyboard-interface alternative. Adopt conventional keys as UNNDEV rules. |
| [2.1.2 A](https://www.w3.org/WAI/WCAG22/Understanding/no-keyboard-trap.html) | Keyboard focus must be able to leave any component; explain a nonstandard exit method if needed. | Enter and exit widgets, embedded frames and dialogs. A modal's intentional focus containment must have an operable exit. |
| [2.1.4 A](https://www.w3.org/WAI/WCAG22/Understanding/character-key-shortcuts.html) | Character-only shortcuts must be disableable, remappable to include a non-printable key, or active only while the relevant component is focused. | Test letters, symbols, punctuation and character sequences; check alternate keyboard layouts and dictation. Shift plus a printable character is not a universal escape from this rule. |
| [2.2.1 A](https://www.w3.org/WAI/WCAG22/Understanding/timing-adjustable.html) | Content-set time limits need disable, adjustment of at least 10× default, or warning with at least 20 seconds to extend at least ten times, unless a defined real-time, essential, or over-20-hours exception applies. | Exercise real client/server timeouts and extension. Security is not a blanket exemption. Check transient messages with unique information/actions. |
| [2.2.2 A](https://www.w3.org/WAI/WCAG22/Understanding/pause-stop-hide.html) | Auto-start moving/blinking/scrolling content lasting over five seconds alongside other content needs pause/stop/hide unless essential. Auto-updating information needs pause/stop/hide or frequency control under its conditions. | Test carousels, tickers, animated decoration, live dashboards and progress behavior. Do not apply the five-second qualification to all auto-updating information. |
| [2.3.1 A](https://www.w3.org/WAI/WCAG22/Understanding/three-flashes-or-below-threshold.html) | Avoid over-three flashes per second unless below the general/red-flash thresholds. | Review animation/video; use a suitable flash analysis method for borderline content. Do not expose testers to potentially harmful flashing as the sole test. |
| [2.4.1 A](https://www.w3.org/WAI/WCAG22/Understanding/bypass-blocks.html) | Provide a way to bypass repeated blocks. | Keyboard-test skip links and their destination focus; inspect landmarks/headings and repeated chrome across pages. |
| [2.4.2 A](https://www.w3.org/WAI/WCAG22/Understanding/page-titled.html) | Page titles identify topic or purpose. | Check initial and client-side route titles, error routes and dialogs' separate names. |
| [2.4.3 A](https://www.w3.org/WAI/WCAG22/Understanding/focus-order.html) | Sequential focus order preserves meaning and operation. | Traverse forward/backward after opening, closing, inserting, deleting and navigating. Specify logical initial focus and return/fallback focus for dialogs; avoid positive tabindex as an internal rule. |
| [2.4.4 A](https://www.w3.org/WAI/WCAG22/Understanding/link-purpose-in-context.html) | Link purpose is determinable from its text or programmatically associated context, except generally ambiguous purpose. | Review repeated “read more” links, icon links, downloads and table links in accessible context. Standalone descriptive labels are a stronger usability preference. |
| [2.4.5 AA](https://www.w3.org/WAI/WCAG22/Understanding/multiple-ways.html) | More than one way locates a page within a set, except process steps/results. | Demonstrate two working routes, e.g. navigation and search/site map; inventory exceptions rather than assuming an SPA is exempt. |
| [2.4.6 AA](https://www.w3.org/WAI/WCAG22/Understanding/headings-and-labels.html) | Headings and labels describe topic or purpose. | Human-review naming, meaningful headings, repeated controls and field labels. Structural semantics are separately tested under 1.3.1. |
| [2.4.7 AA](https://www.w3.org/WAI/WCAG22/Understanding/focus-visible.html) | Keyboard operation has visible focus; it must not disappear on a timer. | Traverse all controls in every theme/state, including forced colors. Test author focus styling against 1.4.11 too. AA does not itself prescribe a two-pixel perimeter. |
| [2.4.11 AA](https://www.w3.org/WAI/WCAG22/Understanding/focus-not-obscured-minimum.html) | When a control receives keyboard focus it is not entirely hidden by author content. Apply repositionable/user-opened-content notes precisely. | Test sticky headers/footers, banners, drawers and author-created overlays at zoom; verify scroll behavior. Record UA-caused limitations separately. UNNDEV may require complete visibility as a stronger rule. |
| [2.5.1 A](https://www.w3.org/WAI/WCAG22/Understanding/pointer-gestures.html) | Multipoint/path-based gestures need single-pointer operation without a path-based gesture unless essential. | Complete pinch, swipe/path and multi-finger actions using simple controls; keyboard equivalence alone is insufficient. |
| [2.5.2 A](https://www.w3.org/WAI/WCAG22/Understanding/pointer-cancellation.html) | Single-pointer actions must meet a permitted cancellation design: no down-event activation, abort/undo, up-event reversal, or essential down-event behavior. | Press then move away/release, cancel gestures, and verify no accidental destructive action. Favor up-event/click behavior. |
| [2.5.3 A](https://www.w3.org/WAI/WCAG22/Understanding/label-in-name.html) | Controls with visible text labels have accessible names containing that visible text. | Compare computed name with rendered label, including overrides and icon+text buttons; test speech activation. Prefer the visible wording at the start of the name. |
| [2.5.4 A](https://www.w3.org/WAI/WCAG22/Understanding/motion-actuation.html) | Device/user-motion operations need UI alternatives and a way to disable motion response, except supported-interface or essential cases. | Test shake/tilt actions, alternate controls and disabling; include accidental activation scenarios. |
| [2.5.7 AA](https://www.w3.org/WAI/WCAG22/Understanding/dragging-movements.html) | Author-defined drag operations need a single-pointer alternative without dragging unless essential or unmodified UA behavior. | Tap/click through reorder, resize, slider and map tasks. Keyboard-only alternatives do not satisfy this separate requirement; usable move controls or numeric input can. |
| [2.5.8 AA](https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html) | Targets meet 24×24 CSS px or a valid spacing/equivalent/inline/unmodified-UA/essential exception. Spacing uses a 24px-diameter circle centered on each undersized bounding box; it must not intersect another target or another undersized target's circle. | Measure effective hit regions, not only icon bounds, at every responsive layout; exclude overlapping regions serving different actions. Record the exact exception. A 44px target policy is stronger than this AA minimum. |

### Understandable and robust

| SC / level | Applicability and required outcome | Minimum practical verification |
|---|---|---|
| [3.1.1 A](https://www.w3.org/WAI/WCAG22/Understanding/language-of-page.html) | Default human language is programmatically available. | Inspect valid language metadata and compare it with actual content. |
| [3.1.2 AA](https://www.w3.org/WAI/WCAG22/Understanding/language-of-parts.html) | Language changes in passages/phrases are programmatically determinable except proper names, technical terms, indeterminate language and assimilated words. | Review multilingual content, language tags and AT pronunciation; do not tag every foreign-derived word mechanically. |
| [3.2.1 A](https://www.w3.org/WAI/WCAG22/Understanding/on-focus.html) | Receiving focus does not itself initiate a context change. | Tab through all controls and inspect unsolicited navigation, submission, windows and focus movement. |
| [3.2.2 A](https://www.w3.org/WAI/WCAG22/Understanding/on-input.html) | Changing a component setting does not automatically change context unless users were advised beforehand. | Exercise select/radio/checkbox/text changes; distinguish content updates from context changes. Prefer explicit submission controls. |
| [3.2.3 AA](https://www.w3.org/WAI/WCAG22/Understanding/consistent-navigation.html) | Repeated navigation within a page set preserves relative order unless the user initiates the change. | Compare page templates and responsive variants at equivalent conditions. |
| [3.2.4 AA](https://www.w3.org/WAI/WCAG22/Understanding/consistent-identification.html) | Components with the same function are consistently identified across the page set. | Compare names, visible labels, alternative text and icons for common actions. |
| [3.2.6 A](https://www.w3.org/WAI/WCAG22/Understanding/consistent-help.html) | Repeated contact details/contact mechanisms/self-help/automated contact appear in the same relative order within the page set, unless user-changed. | Compare serialized relative order and visual placement across equivalent breakpoints. This criterion does not itself require adding a help mechanism to every page. |
| [3.3.1 A](https://www.w3.org/WAI/WCAG22/Understanding/error-identification.html) | Automatically detected input errors identify the affected item and describe the error in text. | Submit empty, malformed, server-invalid and conflicting inputs; check text errors and AT discovery. Color alone fails. |
| [3.3.2 A](https://www.w3.org/WAI/WCAG22/Understanding/labels-or-instructions.html) | User input has labels/instructions needed to understand requirements. | Review visible persistent labels, required status, formats, units and constraints before entry. A placeholder or accessible name alone may not supply needed instructions. |
| [3.3.3 AA](https://www.w3.org/WAI/WCAG22/Understanding/error-suggestion.html) | When correction suggestions are known, provide them unless doing so jeopardizes security or purpose. | Trigger errors and verify actionable fixes, safe format examples and retained values. Test that security exceptions are specific. |
| [3.3.4 AA](https://www.w3.org/WAI/WCAG22/Understanding/error-prevention-legal-financial-data.html) | Legal/financial commitments, modification/deletion of user-controlled stored data, and test-response submissions need reversibility, input checking with correction, or review/confirmation/correction. | Test final submission, wrong data and recovery end-to-end. This is not a requirement for a confirmation modal on every routine save. |
| [3.3.7 A](https://www.w3.org/WAI/WCAG22/Understanding/redundant-entry.html) | Information re-entered in the same process is auto-populated or selectable, except essential, security or invalid-data cases. | Complete multi-step flows including third-party transitions. Ordinary browser autocomplete is not sufficient by itself; cross-session storage is not required. |
| [3.3.8 AA](https://www.w3.org/WAI/WCAG22/Understanding/accessible-authentication-minimum.html) | Each authentication step avoids cognitive-function testing or provides an alternative/assistance mechanism, with the defined object-recognition and user-provided non-text-content exceptions. | Test login, MFA and recovery using password managers and copy/paste; do not force transcription of OTPs through hostile split fields. A password is not automatically prohibited if assistance works. Prefer non-puzzle flows beyond the exceptions. |
| [4.1.2 A](https://www.w3.org/WAI/WCAG22/Understanding/name-role-value.html) | Every UI component exposes name/role and relevant settable states/properties/values; changes are available to AT. | Inspect accessibility tree and operate with AT; verify expanded, selected, checked, pressed, disabled, value and relationships update. Valid ARIA syntax alone is insufficient. |
| [4.1.3 AA](https://www.w3.org/WAI/WCAG22/Understanding/status-messages.html) | Status messages about outcomes, waiting, progress or errors that do not change context are programmatically available without taking focus. | Test success, no results, loading/progress, errors and asynchronous updates with screen readers. Use suitable live semantics; avoid duplicate/noisy announcements. Not every DOM update or results list is a status message. |

**Removed criterion:** 4.1.1 Parsing is obsolete and absent from WCAG 2.2's conformance requirements. Keep HTML validity and unique-ID checks as engineering quality controls, and fix semantic/accessibility defects under applicable criteria. Do not list 4.1.1 as a WCAG 2.2 AA failure. [W3C changes](https://www.w3.org/WAI/standards-guidelines/wcag/new-in-22/)


## Source register

This distribution cites public primary guidance only. Local project profiles, source inventories, implementation audits and private adoption decisions are maintained in their owning repositories and are not included here.

- **R1** Instagram [Same button Six decisions](https://www.instagram.com/reel/Ddy5RJOha1U/) by @motion_ui_interface; public inspiration only

### Public accessibility references

- **W0** [WCAG 2.2 Recommendation](https://www.w3.org/TR/WCAG22/), normative web standard; Understanding pages explain criteria and are informative
- **W1** [Contrast Minimum](https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html), SC 1.4.3 AA
- **W2** [Non-text Contrast](https://www.w3.org/WAI/WCAG22/Understanding/non-text-contrast.html), SC 1.4.11 AA
- **W3** [Keyboard](https://www.w3.org/WAI/WCAG22/Understanding/keyboard.html), SC 2.1.1 A
- **W4** [WAI ARIA Button Pattern](https://www.w3.org/WAI/ARIA/apg/patterns/button/), implementation guidance
- **W5** [Focus Not Obscured Minimum](https://www.w3.org/WAI/WCAG22/Understanding/focus-not-obscured-minimum), SC 2.4.11 AA
- **W6** [Target Size Minimum](https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum), SC 2.5.8 AA
- **W7** [Android Accessibility](https://developer.android.com/design/ui/mobile/guides/foundations/accessibility), native platform guidance
- **W8** [WCAG 2.2 Name Role Value and Label in Name](https://www.w3.org/TR/WCAG22/), SC 4.1.2 and 2.5.3 A
- **W9** [Status Messages](https://www.w3.org/WAI/WCAG22/Understanding/status-messages.html), SC 4.1.3 AA
- **W10** [Reflow](https://www.w3.org/WAI/WCAG22/Understanding/reflow.html), SC 1.4.10 AA
- **W11** [WCAG 2.2 Resize Text](https://www.w3.org/TR/WCAG22/#resize-text), SC 1.4.4 AA
- **W12** [WCAG 2.2 Dragging Movements](https://www.w3.org/TR/WCAG22/#dragging-movements), SC 2.5.7 AA
- **W13** [Animation from Interactions](https://www.w3.org/WAI/WCAG22/Understanding/animation-from-interactions.html), SC 2.3.3 AAA

### Additional primary research sources

All checked 30 September 2026. Design-system, APG and performance pages are guidance, not independent certification requirements. The contracts citing them are internal rules for adopting repositories; a citation does not imply the source mandates every additional verification case.

- **N1** [DTCG Format Module 2025.10](https://www.designtokens.org/tr/2025.10/format/), stable Community Group specification; not a W3C Standard or Standards Track publication
- **N2** [Google Web Vitals](https://web.dev/articles/vitals), current web field-performance metrics and threshold guidance
- **N3** [GOV.UK Button](https://design-system.service.gov.uk/components/button/), action hierarchy and labels
- **N4** [GOV.UK Error summary](https://design-system.service.gov.uk/components/error-summary/), submitted-form recovery
- **N5** [GOV.UK Error message](https://design-system.service.gov.uk/components/error-message/), field-level messages
- **N6** [GOV.UK Recover from validation errors](https://design-system.service.gov.uk/patterns/validation/), retaining inputs and validation timing
- **N7** [APG Checkbox](https://www.w3.org/WAI/ARIA/apg/patterns/checkbox/), informative semantics and keyboard pattern
- **N8** [APG Radio group](https://www.w3.org/WAI/ARIA/apg/patterns/radio/), informative semantics and keyboard pattern
- **N9** [APG Switch](https://www.w3.org/WAI/ARIA/apg/patterns/switch/), stable label and state guidance
- **N10** [APG Combobox](https://www.w3.org/WAI/ARIA/apg/patterns/combobox/), selection/editing/focus guidance
- **N11** [APG Modal dialog](https://www.w3.org/WAI/ARIA/apg/patterns/dialog-modal/), contextual initial focus, containment and restoration
- **N12** [Fluent 2 Dialog](https://fluent2.microsoft.design/components/web/react/core/dialog/usage), supplemental/modal/alert distinctions; its exact dismissal choices are system-specific
- **N13** [Fluent 2 Drawer](https://fluent2.microsoft.design/components/web/react/core/drawer/usage), contextual layout and zoom/overflow considerations
- **N14** [APG Menu button](https://www.w3.org/WAI/ARIA/apg/patterns/menu-button/), action-menu keyboard/focus pattern
- **N15** [APG Tabs](https://www.w3.org/WAI/ARIA/apg/patterns/tabs/), selection and activation guidance
- **N16** [APG Disclosure](https://www.w3.org/WAI/ARIA/apg/patterns/disclosure/), expanded-state pattern
- **N17** [APG Tooltip](https://www.w3.org/WAI/ARIA/apg/patterns/tooltip/), explicitly work in progress without task-force consensus; consulted cautiously, not normative
- **N18** [APG Slider](https://www.w3.org/WAI/ARIA/apg/patterns/slider/), numeric semantics and touch-assistive-technology warning
- **N19** [APG Spinbutton](https://www.w3.org/WAI/ARIA/apg/patterns/spinbutton/), numeric-entry pattern
- **N20** [Android Compose progress indicators](https://developer.android.com/develop/ui/compose/components/progress), Material-aligned determinate/indeterminate implementation guidance
- **N21** [Fluent 2 Toast](https://fluent2.microsoft.design/components/web/react/core/toast/usage), nonblocking feedback guidance
- **N22** [GOV.UK Notification banner](https://design-system.service.gov.uk/components/notification-banner/), contextual notification guidance
- **N23** [GOV.UK Check answers](https://design-system.service.gov.uk/patterns/check-answers/), review-before-submission pattern
- **N24** [Apple Undo and redo](https://developer.apple.com/design/human-interface-guidelines/undo-and-redo), native behavior and predictable reversal guidance
- **N25** [GOV.UK Table](https://design-system.service.gov.uk/components/table/), tabular comparison, captions and headers
- **N26** [APG Table](https://www.w3.org/WAI/ARIA/apg/patterns/table/), static table semantics
- **N27** [APG Grid](https://www.w3.org/WAI/ARIA/apg/patterns/grid/), managed interactive-grid keyboard behavior
- **N28** [Google guidance on virtualizing large lists](https://web.dev/articles/virtualize-long-lists-react-window), performance technique; not a recommendation to install a particular library/version
- **N29** [APG Developing a keyboard interface](https://www.w3.org/WAI/ARIA/apg/practices/keyboard-interface/), focus/selection, composite navigation and shortcut guidance
- **N30** [APG Toolbar](https://www.w3.org/WAI/ARIA/apg/patterns/toolbar/), composite-control pattern
- **N31** [APG Tree view](https://www.w3.org/WAI/ARIA/apg/patterns/treeview/), hierarchical navigation/selection pattern
- **N32** [W3C Internationalization quick tips](https://www.w3.org/International/quicktips/), informative global-readiness guidance
- **N33** [W3C Text size in translation](https://www.w3.org/International/articles/article-text-size), expansion and layout guidance
- **N34** [W3C Inline bidi markup](https://www.w3.org/International/articles/inline-bidi-markup/), direction/isolation guidance
- **N35** [Google Rendering performance](https://web.dev/articles/rendering-performance), rendering-pipeline guidance
- **N36** [Google Optimize INP](https://web.dev/articles/optimize-inp), interaction-latency diagnosis

## Revision history

- **1.0, 30 September 2026:** distribution edition derived from the comprehensive 1.0 standard. Preserved shared rule IDs and substantive common contracts, the complete 55-criterion WCAG 2.2 A/AA map, public sources and verification guidance. Removed cross-project profiles, private source links, source-drift inventories and confidential adoption decisions. Repository adoption is recorded separately.
