//! Root view: composes title bar, sidebar, workspace, and status bar, owns the
//! shared `AppState` entity, and renders the Root overlay layers.

use gpui_kit::component::Root;
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::{ActiveTheme, WindowExt, v_flex};
use gpui_kit::*;

use crate::db::DatabaseTarget;
use crate::state::{self, AppState};
use crate::ui::sidebar::Sidebar;
use crate::ui::status_bar::StatusBarView;
use crate::ui::title_bar::TitleBarView;
use crate::ui::workspace::Workspace;

pub struct DuckLocalApp {
    #[allow(dead_code)]
    state: Entity<AppState>,
    title_bar: Entity<TitleBarView>,
    sidebar: Entity<Sidebar>,
    workspace: Entity<Workspace>,
    status_bar: Entity<StatusBarView>,
}

impl DuckLocalApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(AppState::new);
        let workspace = cx.new(|cx| Workspace::new(state.clone(), window, cx));
        let sidebar = cx.new(|cx| Sidebar::new(state.clone(), workspace.clone(), window, cx));
        let title_bar = cx.new(|cx| TitleBarView::new(state.clone(), cx));
        let status_bar = cx.new(|cx| StatusBarView::new(state.clone(), cx));

        let app_state = state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = smol::unblock(|| -> anyhow::Result<_> {
                crate::history::init().ok();
                crate::db::open_memory()?;
                state::reattach_registered_files();
                let server = crate::db::server_info(DatabaseTarget::Memory)?;
                let (catalog, history) = state::load_sidebar_data();
                let attached = state::load_attached_files();
                Ok((server, catalog, history, attached))
            })
            .await;

            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok((server, catalog, history, attached)) => {
                        app_state.update(cx, |s, cx| {
                            s.set_connection(DatabaseTarget::Memory, server, catalog, cx);
                            s.set_history(history, cx);
                            s.set_attached_files(attached, cx);
                        });
                        this.workspace.update(cx, |ws, cx| {
                            ws.focus_active_editor(window, cx);
                        });
                    }
                    Err(e) => {
                        window.push_notification(
                            format!("初始化内存数据库失败：{e}"),
                            cx,
                        );
                    }
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
}

impl Render for DuckLocalApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
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
