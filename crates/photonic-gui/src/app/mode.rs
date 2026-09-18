//! [`AppMode`] — the vector/video editor mode switch (video-editor-module
//! `04-ui-mode-timeline.md` §1). D-02: switching mode preserves the existing
//! shell layout; this enum is a value threaded through a few read sites
//! (toolbar, rail contents, central-panel branch), not a second top-level
//! screen.

use serde::{Deserialize, Serialize};

/// Which editor a tab is currently presenting: the vector canvas (today's
/// default) or the video timeline/program-monitor shell (04 §1.1).
///
/// Per-tab, not global (04 §1): lives on `DocTab::mode` and is mirrored on
/// `PhotonicApp::mode` for the active tab, restored by `switch_tab` exactly
/// like `selected_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AppMode {
    #[default]
    Vector,
    Video,
}
