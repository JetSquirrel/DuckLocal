//! Root view: composes title bar, sidebar, workspace, and status bar, and owns
//! the shared `AppState` entity. Dialog, sheet and notification layers are
//! mounted by the window's Root itself, not rendered here.

use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::{v_flex, ActiveTheme, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::analysis::apps;
use crate::i18n::trf;
use crate::state::{self, AppState};
use crate::ui::sidebar::Sidebar;
use crate::ui::status_bar::StatusBarView;
use crate::ui::title_bar::TitleBarView;
use crate::ui::workspace::Workspace;
use crate::ui::{apply_open_outcome, open_paths, CloseTab, NewQuery, OpenData, ToggleSidebar};

pub struct DuckLocalApp {
    state: Entity<AppState>,
    title_bar: Entity<TitleBarView>,
    sidebar: Entity<Sidebar>,
    workspace: Entity<Workspace>,
    status_bar: Entity<StatusBarView>,
}

impl DuckLocalApp {
    /// `paths` are the command-line arguments: data files, folders, patterns,
    /// a database file to open instead of the in-memory connection, an app
    /// directory or a `.dash` spec to open as a tab.
    pub fn new(paths: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(AppState::new);
        let workspace = cx.new(|cx| Workspace::new(state.clone(), window, cx));
        let sidebar = cx.new(|cx| Sidebar::new(state.clone(), workspace.clone(), window, cx));
        let title_bar = cx.new(|cx| TitleBarView::new(state.clone(), cx));
        let status_bar = cx.new(|cx| StatusBarView::new(state.clone(), cx));
        cx.subscribe(&state, |_, _, _: &state::SidebarToggled, cx| cx.notify())
            .detach();

        let open_state = state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = smol::unblock(move || {
                crate::history::init().ok();
                // A `.dash` file is a dashboard, not data; a directory with the
                // app entry file in it is an app, not a folder of data. Both
                // open as tabs, and the rest of the command line keeps the
                // behaviour it had.
                let (from_command_line, rest) = apps::split_paths(&paths);
                let (dashboards, data) = crate::spec::tabs::split(rest);
                let outcome = state::open_request(&data, true)?;
                // Read here rather than in the workspace: this is the one place
                // in the app that is already off the UI thread and has the
                // history store open.
                let remembered = apps::restore();
                let remembered_dashboards = crate::spec::tabs::restore();
                Ok::<_, anyhow::Error>((
                    from_command_line,
                    dashboards,
                    remembered,
                    remembered_dashboards,
                    outcome,
                ))
            })
            .await;

            this.update_in(cx, |this, window, cx| match result {
                Ok((from_command_line, dashboards, remembered, remembered_dashboards, outcome)) => {
                    apply_open_outcome(open_state, outcome, window, cx);
                    this.workspace.update(cx, |ws, cx| {
                        let mut directories = from_command_line;
                        directories.extend(
                            remembered
                                .apps
                                .iter()
                                .map(|app| std::path::PathBuf::from(&app.path)),
                        );
                        ws.open_apps(directories, window, cx);
                        for path in dashboards {
                            ws.open_dashboard(path, window, cx);
                        }
                        for spec in &remembered_dashboards.specs {
                            ws.open_dashboard(std::path::PathBuf::from(&spec.path), window, cx);
                        }
                        ws.focus_active_editor(window, cx);
                    });
                    // A remembered app or dashboard that is gone, or is no
                    // longer one, is named rather than silently dropped.
                    for problem in remembered
                        .problems
                        .into_iter()
                        .chain(remembered_dashboards.problems)
                    {
                        window.push_notification(Notification::error(problem), cx);
                    }
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

    /// Paths dropped on the window: an app directory opens an app tab, a
    /// `.dash` file a dashboard tab, and everything else is the same request
    /// the command line and the pickers make.
    fn drop_paths(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let requested: Vec<String> = paths
            .paths()
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect();
        let (directories, rest) = apps::split_paths(&requested);
        for directory in directories {
            self.workspace
                .update(cx, |ws, cx| ws.open_app(directory, window, cx));
        }
        let (dashboards, data) = crate::spec::tabs::split(rest);
        for path in dashboards {
            self.workspace
                .update(cx, |ws, cx| ws.open_dashboard(path, window, cx));
        }
        if !data.is_empty() {
            open_paths(self.state.clone(), data, window, cx);
        }
    }
}

impl Render for DuckLocalApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.state.update(cx, |state, cx| state.toggle_sidebar(cx));
            }))
            .on_action(cx.listener(|this, _: &NewQuery, window, cx| {
                this.workspace
                    .update(cx, |workspace, cx| workspace.add_query_tab(window, cx));
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                this.workspace
                    .update(cx, |workspace, cx| workspace.close_active_tab(window, cx));
            }))
            .on_action(cx.listener(|this, _: &OpenData, window, cx| {
                this.title_bar
                    .update(cx, |title_bar, cx| title_bar.open_data(window, cx));
            }))
            .child(self.title_bar.clone())
            .child(div().flex_1().min_h_0().map(|this| {
                if self.state.read(cx).is_sidebar_collapsed() {
                    this.child(self.workspace.clone())
                } else {
                    this.child(
                        h_resizable("main-split")
                            .child(
                                resizable_panel()
                                    .size(px(280.))
                                    .size_range(px(220.)..px(420.))
                                    // Cached: the sidebar re-renders when it
                                    // is notified (its state events, hover,
                                    // clicks) or the window refreshes (theme,
                                    // language, size) — not with every frame
                                    // of the editor's blinking cursor, which
                                    // would rebuild the history list and the
                                    // tree twice a second while idle.
                                    .child(
                                        self.sidebar
                                            .clone()
                                            .cached(StyleRefinement::default().size_full()),
                                    ),
                            )
                            .child(resizable_panel().child(self.workspace.clone())),
                    )
                }
            }))
            .child(self.status_bar.clone())
    }
}
