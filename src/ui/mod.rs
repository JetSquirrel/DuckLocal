//! UI layer: title bar, sidebar, workspace, results, chart, status bar.

pub mod chart;
pub mod completion;
pub mod results;
pub mod sidebar;
pub mod status_bar;
pub mod title_bar;
pub mod workspace;

use gpui_kit::{App, KeyBinding};

gpui_kit::actions!(ducklocal, [RunQuery]);

/// Key context that makes ⌘↵ reachable while the SQL editor is focused.
pub const WORKSPACE_KEY_CONTEXT: &str = "DuckLocal";

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "cmd-enter",
        RunQuery,
        Some(WORKSPACE_KEY_CONTEXT),
    )]);
}
