//! Root view: composes title bar, sidebar, workspace, and status bar, owns the
//! shared `AppState` entity, and renders the Root overlay layers.

use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::Root;
use gpui_kit::component::{v_flex, ActiveTheme, WindowExt};
use gpui_kit::*;

use crate::i18n::trf;
use crate::state::{self, AppState};
use crate::ui::sidebar::Sidebar;
use crate::ui::status_bar::StatusBarView;
use crate::ui::title_bar::TitleBarView;
use crate::ui::workspace::Workspace;
use crate::ui::{apply_open_outcome, open_paths};

pub struct DuckLocalApp {
    #[allow(dead_code)]
    state: Entity<AppState>,
    title_bar: Entity<TitleBarView>,
    sidebar: Entity<Sidebar>,
    workspace: Entity<Workspace>,
    status_bar: Entity<StatusBarView>,
}

impl DuckLocalApp {
    /// `paths` are the command-line arguments: data files, folders, patterns,
    /// or a database file to open instead of the in-memory connection.
    pub fn new(paths: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(AppState::new);
        let workspace = cx.new(|cx| Workspace::new(state.clone(), window, cx));
        let sidebar = cx.new(|cx| Sidebar::new(state.clone(), workspace.clone(), window, cx));
        let title_bar = cx.new(|cx| TitleBarView::new(state.clone(), cx));
        let status_bar = cx.new(|cx| StatusBarView::new(state.clone(), cx));

        let open_state = state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = smol::unblock(move || {
                crate::history::init().ok();
                state::open_request(&paths, true)
            })
            .await;

            this.update_in(cx, |this, window, cx| match result {
                Ok(outcome) => {
                    apply_open_outcome(open_state, outcome, window, cx);
                    this.workspace.update(cx, |ws, cx| {
                        ws.focus_active_editor(window, cx);
                    });
                }
                Err(e) => {
                    window
                        .push_notification(trf("notify.init_memory.failed", &[&e.to_string()]), cx);
                }
            })
            .ok();
        })
        .detach();

        Self {
            state,
            title_bar,
            sidebar,
            workspace,
            status_bar,
        }
    }

    /// Files and folders dropped on the window: the same request the command
    /// line and the pickers make.
    fn drop_paths(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let requested = paths
            .paths()
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect();
        open_paths(self.state.clone(), requested, window, cx);
    }
}

impl Render for DuckLocalApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // The whole window accepts data files, so a drop lands wherever
            // the pointer happens to be. The border is always present — only
            // its color changes while an external drag is over the window —
            // so highlighting does not shift the layout.
            .on_drop(cx.listener(Self::drop_paths))
            .border_2()
            .border_color(cx.theme().background)
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.border_color(cx.theme().primary))
            .child(self.title_bar.clone())
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("main-split")
                        .child(
                            resizable_panel()
                                .size(px(280.))
                                .size_range(px(220.)..px(420.))
                                .child(self.sidebar.clone()),
                        )
                        .child(resizable_panel().child(self.workspace.clone())),
                ),
            )
            .child(self.status_bar.clone())
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_sheet_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}
