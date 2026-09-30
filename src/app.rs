//! Root view: composes title bar, sidebar, workspace, and status bar, and owns
//! the shared `AppState` entity. Dialog, sheet and notification layers are
//! mounted by the window's Root itself, not rendered here.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::{v_flex, ActiveTheme, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// Documents Finder asks the app to open — "Open With", a double-clicked
/// `.dash`, a drop on the Dock icon — while it runs or as it launches. The
/// platform's openURLs callback gets no App context, so it queues the paths
/// here and the root view drains them once a frame is up.
static FINDER_OPENS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Queue one URL from the platform's openURLs callback. Only `file://` URLs
/// are documents; anything else was not meant for us.
pub(crate) fn queue_finder_open(url: &str) {
    if let Some(path) = file_url_path(url) {
        FINDER_OPENS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(path);
    }
}

/// The path a `file://` URL points at. Percent-escapes are UTF-8; the
/// authority is empty or `localhost` for a local file.
fn file_url_path(url: &str) -> Option<String> {
    fn hex(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    let authority_and_path = url.strip_prefix("file://")?;
    let path_start = authority_and_path.find('/')?;
    let encoded = &authority_and_path[path_start..];
    let mut decoded = Vec::with_capacity(encoded.len());
    let mut bytes = encoded.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let hi = hex(bytes.next()?)?;
            let lo = hex(bytes.next()?)?;
            decoded.push(hi << 4 | lo);
        } else {
            decoded.push(byte);
        }
    }
    String::from_utf8(decoded).ok()
}

use crate::analysis::apps;
use crate::i18n::{tr, trf};
use crate::state::{self, AppState};
use crate::ui::sidebar::Sidebar;
use crate::ui::status_bar::StatusBarView;
use crate::ui::title_bar::TitleBarView;
use crate::ui::workspace::Workspace;
use crate::ui::{
    apply_open_outcome, open_paths, CloseTab, NewQuery, OpenData, OpenSetup, ToggleSidebar,
};

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
                let offer_setup = crate::setup::offer_once();
                Ok::<_, anyhow::Error>((
                    from_command_line,
                    dashboards,
                    remembered,
                    remembered_dashboards,
                    outcome,
                    offer_setup,
                ))
            })
            .await;

            this.update_in(cx, |this, window, cx| match result {
                Ok((
                    from_command_line,
                    dashboards,
                    remembered,
                    remembered_dashboards,
                    outcome,
                    offer_setup,
                )) => {
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
                    // Installing the app does not put `ducklocal` on the PATH;
                    // say once where that is done.
                    if offer_setup {
                        window.push_notification(
                            Notification::info(tr("setup.offer"))
                                .title(tr("setup.title"))
                                .action(|_, _, cx| {
                                    Button::new("offer-setup")
                                        .primary()
                                        .small()
                                        .label(tr("setup.offer.action"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.dismiss(window, cx);
                                            crate::ui::setup_dialog::open(window, cx);
                                        }))
                                }),
                            cx,
                        );
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

        // Finder opens queue from the moment the platform callback is
        // registered — possibly before this view exists — and `ducklocal
        // open` requests from the moment the listener starts, so drain both
        // on a slow poll and open them like a drop on the window. The task ends with
        // the window: `update_in` fails once the view is gone.
        cx.spawn_in(window, async move |this, cx| {
            loop {
                smol::Timer::after(Duration::from_millis(200)).await;
                let paths: Vec<String> = std::mem::take(
                    &mut *FINDER_OPENS
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner),
                );
                let requests = crate::remote::take_pending();
                let state_wanted = crate::remote::state_wanted();
                if paths.is_empty() && requests.is_empty() && !state_wanted {
                    continue;
                }
                if this
                    .update_in(cx, |this, window, cx| {
                        if !paths.is_empty() {
                            this.open_external(paths, window, cx);
                        }
                        for request in requests {
                            this.open_remote(request, window, cx);
                        }
                        // After the opens, so an `open` then a `state` sees
                        // the tab the open made.
                        if state_wanted {
                            crate::remote::answer_state(this.snapshot(cx));
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
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

    /// Paths from outside the app — a drop on the window, or a document
    /// Finder opens with it: an app directory opens an app tab, a `.dash`
    /// file a dashboard tab, and everything else is the same request the
    /// command line and the pickers make.
    fn open_external(&mut self, requested: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
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

    /// A request from `ducklocal open`: its paths open like a drop, then its
    /// SQL gets a tab of its own, in front, in a window brought forward — the
    /// command's caller wants someone to see it.
    fn open_remote(
        &mut self,
        request: crate::remote::Request,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.activate_window();
        self.open_external(request.paths, window, cx);
        let Some(sql) = request.sql else {
            return;
        };
        self.workspace
            .update(cx, |ws, cx| ws.open_query_tab(sql, request.title, window, cx));
        if !request.run {
            return;
        }
        // The SQL may name what the paths attach, and attaching is off the
        // UI thread: run once no open request is in flight.
        let state = self.state.clone();
        cx.spawn_in(window, async move |this, cx| {
            while cx
                .update(|_, cx| state.read(cx).is_opening())
                .unwrap_or(false)
            {
                smol::Timer::after(Duration::from_millis(50)).await;
            }
            this.update_in(cx, |this, window, cx| {
                this.workspace
                    .update(cx, |ws, cx| ws.run_active(window, cx));
            })
            .ok();
        })
        .detach();
    }

    /// What the window shows, for `ducklocal open --state`: the database, the files
    /// attached to it, and the workspace's tabs.
    fn snapshot(&self, cx: &App) -> serde_json::Value {
        use serde_json::json;
        let state = self.state.read(cx);
        let mut snapshot = self.workspace.read(cx).snapshot(cx);
        snapshot["database"] = match &state.target {
            Some(crate::db::DatabaseTarget::File(path)) => json!(path),
            Some(crate::db::DatabaseTarget::Memory) => json!(":memory:"),
            None => serde_json::Value::Null,
        };
        snapshot["attached"] = json!(state
            .attached_files
            .iter()
            .map(|file| json!({
                "path": file.path,
                "name": file.view_name,
                "rows": file.row_count,
            }))
            .collect::<Vec<_>>());
        snapshot
    }

    fn drop_paths(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let requested: Vec<String> = paths
            .paths()
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect();
        self.open_external(requested, window, cx);
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
            .on_action(cx.listener(|_, _: &OpenSetup, window, cx| {
                crate::ui::setup_dialog::open(window, cx);
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

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: that pulls in `gpui_kit::*`, whose
    // `test` macro shadows the built-in `#[test]`.
    use super::file_url_path;

    #[test]
    fn file_urls_decode_to_paths() {
        assert_eq!(
            file_url_path("file:///Users/admin/aiops2_overview.dash"),
            Some("/Users/admin/aiops2_overview.dash".to_string())
        );
        // Spaces and CJK arrive percent-encoded.
        assert_eq!(
            file_url_path("file:///Users/admin/My%20Reports/%E6%97%A5%E6%8A%A5.dash"),
            Some("/Users/admin/My Reports/日报.dash".to_string())
        );
        assert_eq!(
            file_url_path("file://localhost/Users/admin/x.dash"),
            Some("/Users/admin/x.dash".to_string())
        );
    }

    #[test]
    fn non_file_urls_are_not_documents() {
        assert_eq!(file_url_path("https://example.com/x.dash"), None);
        assert_eq!(file_url_path("file://"), None);
        assert_eq!(file_url_path("file:///bad%zz"), None);
    }
}
