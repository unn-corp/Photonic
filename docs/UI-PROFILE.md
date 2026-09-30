# Photonic UI profile

Companion to the [adoption entry](UNNDEV-UI-STANDARDS.md) and [shared core](UNNDEV-UI-STANDARDS-CORE.md).

## Applicable surfaces

NATIVE + EDITOR for the desktop GUI; apply TOUCH only where touch is supported and tested. Headless protocol operations are not web UI.

## Local authority and preservation

- [Design authority](../DESIGN.md) retains Deep Violet / Soft Lavender roles, dense editor typography, rail-and-floating-drawer structure and documented component exceptions.
- [Theme implementation](../crates/photonic-gui/src/theme.rs) and [GUI source](../crates/photonic-gui/src/) own native rendering and interaction. Reuse those controls before adding a competing widget or palette.
- Preserve the distinction between decorative borders and interactive boundaries, and between accent graphics and readable accent text. Existing contrast measurements are scoped evidence, not complete accessibility certification.
- Keep document editing, selection, undo/redo and export state truthful. Destructive changes need the applicable recovery path; do not disguise an unsaved or failed result.
- Map shared accessibility outcomes through egui and supported platform accessibility/input APIs. Record native applicability and test limitations rather than claiming browser WCAG certification.

## Verification

Follow the existing contribution gates for future implementation: formatting, clippy, build and tests. Retain design-contrast and glyph-coverage tests where affected. No GUI or accessibility runtime audit is claimed by this docs-only installation.

For each future implementation, record the affected states and input methods, relevant core rule IDs, focused checks and manual evidence. Documentation installation alone establishes neither runtime compliance nor completion of the existing release gates.

## Exceptions

No new exception is granted by this profile. Keep named existing local exceptions with their governing source. Any new exception must identify the rule ID, bounded surface, reason, owner, compensating behavior, verification and review date. Escalate unresolved conflicts before implementation.
