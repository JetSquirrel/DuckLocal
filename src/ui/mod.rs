//! UI layer: title bar, sidebar, workspace, results, chart, status bar.

pub mod chart;
pub mod completion;
pub mod results;
pub mod sidebar;
pub mod status_bar;
pub mod title_bar;
pub mod workspace;

use gpui_kit::component::Theme;
use gpui_kit::{App, KeyBinding, px};

gpui_kit::actions!(ducklocal, [RunQuery]);

/// Key context that makes ⌘↵ reachable while the SQL editor is focused.
pub const WORKSPACE_KEY_CONTEXT: &str = "DuckLocal";

pub const RUN_QUERY_KEYSTROKE: &str = "cmd-enter";

pub fn init(cx: &mut App) {
    // Denser desktop density: 14px rem base (gpui-component ships 16).
    // Re-applied after every Theme::change (see main.rs, title_bar.rs).
    Theme::global_mut(cx).font_size = px(14.);
    cx.bind_keys([KeyBinding::new(
        RUN_QUERY_KEYSTROKE,
        RunQuery,
        Some(WORKSPACE_KEY_CONTEXT),
    )]);
}
