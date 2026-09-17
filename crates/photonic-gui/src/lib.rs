// GUI interaction paths intentionally favor explicit indexed traversal and
// incremental `Default` setup, keeping event/state transitions debuggable.
// These expectations are self-checking under the strict workspace gate.
#![expect(
    clippy::doc_lazy_continuation,
    clippy::empty_line_after_doc_comments,
    clippy::field_reassign_with_default,
    clippy::manual_clamp,
    clippy::needless_range_loop,
    clippy::type_complexity,
    clippy::unnecessary_sort_by,
    dead_code,
    deprecated,
    unused_doc_comments
)]

pub mod app;
pub mod color_convert;
pub mod color_popup;
pub mod commands;
pub mod disk_search;
mod file_io;
pub mod global_search;
pub mod hotbar;
pub mod lightfall;
pub mod multi_button;
pub mod panels;
pub mod preferences;
pub mod quit;
pub mod radial_wheel;
pub mod release_notes;
pub mod snap;
pub mod theme;
pub mod tools;
pub mod update;
pub mod viewport;
pub mod welcome;

pub use app::engine::EngineBridge;
pub use app::{NativeClipboardPaste, PhotonicApp};
pub(crate) use file_io::read_svg_file;
pub use preferences::AppPreferences;
pub use theme::{build_dark_theme, build_light_theme};
pub use tools::Tool;
