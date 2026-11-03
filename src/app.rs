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

use crate::handoff::{Action, ActionLog, ActionStatus};
use crate::i18n::{tr, trf};
use crate::state::{self, AppState};
use crate::ui::sidebar::Sidebar;
use crate::ui::status_bar::StatusBarView;
use crate::ui::title_bar::TitleBarView;
use crate::ui::tour::{NeedsEditor, Tour};
use crate::ui::workspace::Workspace;
use crate::ui::{
    apply_open_outcome, open_paths, ActivateTab, CloseTab, NewQuery, NextTab, OpenData, OpenSetup,
    PreviousTab, ToggleSidebar,
};

pub struct DuckLocalApp {
    state: Entity<AppState>,
    title_bar: Entity<TitleBarView>,
    sidebar: Entity<Sidebar>,
    workspace: Entity<Workspace>,
    status_bar: Entity<StatusBarView>,
    tour: Entity<Tour>,
    /// Startup's own open request has landed, or failed. Until then, Finder
    /// opens and `ducklocal open` requests wait in their queues: run beside
    /// it, the one that finished last decided what the window said it was on.
    started: bool,
    /// What became of each `ducklocal open`, for the state to report.
    actions: ActionLog,
}

impl DuckLocalApp {
    /// `paths` are the command-line arguments: data files, folders, patterns,
    /// a database file to open instead of the in-memory connection, or a
    /// `.dash` spec to open as a tab.
    pub fn new(paths: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(AppState::new);
        let workspace = cx.new(|cx| Workspace::new(state.clone(), window, cx));
        let sidebar = cx.new(|cx| Sidebar::new(state.clone(), workspace.clone(), window, cx));
        let title_bar = cx.new(|cx| TitleBarView::new(state.clone(), cx));
        let status_bar = cx.new(|cx| StatusBarView::new(state.clone(), cx));
        let tour = cx.new(Tour::new);
        cx.subscribe(&state, |_, _, _: &state::SidebarToggled, cx| cx.notify())
            .detach();
        // The guide's editor and results steps need an editor on screen.
        cx.subscribe_in(&tour, window, |this, _, _: &NeedsEditor, window, cx| {
            this.workspace
                .update(cx, |workspace, cx| workspace.show_editor(window, cx));
        })
        .detach();

        let open_state = state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (tabs, crashed, show_tour, result) = smol::unblock(move || {
                crate::history::init().ok();
                // Read whatever the open below does: a launch whose data fails
                // to open still gets its tabs back, and still hears about a
                // crash. Read here rather than in the workspace: this is the
                // one place in the app already off the UI thread with the
                // history store open.
                let query_tabs = crate::query_tabs::restore();
                let remembered_dashboards = crate::spec::tabs::restore();
                // A `.dash` file is a dashboard, not data: it opens as a tab,
                // and the rest of the command line keeps the behaviour it had.
                let (dashboards, data) = crate::spec::tabs::split(paths);
                let crashed = crate::crash::take_pending();
                let show_tour = crate::ui::tour::offer_at_launch();
                let result = (|| {
                    let outcome = state::open_request(&data, true)?;
                    // One invitation at a time: with the guide open, the setup
                    // offer waits for the next launch, still unspent.
                    let offer_setup = !show_tour && crate::setup::offer_once();
                    Ok::<_, anyhow::Error>((outcome, offer_setup))
                })();
                (
                    (query_tabs, remembered_dashboards, dashboards),
                    crashed,
                    show_tour,
                    result,
                )
            })
            .await;
            let (query_tabs, remembered_dashboards, dashboards) = tabs;

            this.update_in(cx, |this, window, cx| {
                this.started = true;
                // Before the dashboards, so a dashboard named on the command
                // line still comes up in front.
                let query_in_front = this
                    .workspace
                    .update(cx, |ws, cx| ws.restore_query_tabs(query_tabs, window, cx));
                if show_tour {
                    this.tour.update(cx, |tour, cx| tour.start(cx));
                }
                // The last run ended in a panic: say so once, and where its
                // report is.
                if let Some(report) = crashed {
                    let path = report.path.clone();
                    window.push_notification(
                        Notification::error(trf("crash.notice", &[&report.summary]))
                            .title(tr("crash.title"))
                            .action(move |_, _, cx| {
                                let path = path.clone();
                                Button::new("show-crash-report")
                                    .small()
                                    .label(tr("crash.show_report"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.reveal_path(&path);
                                        this.dismiss(window, cx);
                                    }))
                            }),
                        cx,
                    );
                }
                // The dashboards come back whether or not the data opened: a
                // launch that failed to open a file must not lose them, and
                // the next tab closed would have written them off for good.
                let named_dashboards = !dashboards.is_empty();
                this.workspace.update(cx, |ws, cx| {
                    // The remembered ones first, so a dashboard named on the
                    // command line opens last: in front, which is what asking
                    // for it means.
                    for spec in &remembered_dashboards.specs {
                        ws.open_dashboard(std::path::PathBuf::from(&spec.path), window, cx);
                    }
                    for path in dashboards {
                        ws.open_dashboard(path, window, cx);
                    }
                    // Written once, whole, now that they are all back.
                    ws.dashboards_restored();
                    // The query tab that was in front last time is again,
                    // unless a dashboard was just asked for.
                    match query_in_front.filter(|_| !named_dashboards) {
                        Some(index) => ws.activate_tab(index, window, cx),
                        None => ws.focus_active_editor(window, cx),
                    }
                });
                // A remembered dashboard that is gone, or is no longer one, is
                // named rather than silently dropped.
                for problem in remembered_dashboards.problems {
                    window.push_notification(Notification::error(problem), cx);
                }
                match result {
                    Ok((outcome, offer_setup)) => {
                        apply_open_outcome(open_state, outcome, window, cx);
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
                        window.push_notification(
                            trf("notify.init_memory.failed", &[&e.to_string()]),
                            cx,
                        );
                    }
                }
            })
            .ok();
        })
        .detach();

        // Finder opens queue from the moment the platform callback is
        // registered — possibly before this view exists — and `ducklocal
        // open` requests from the moment the listener starts, so drain both
        // on a slow poll, once startup's own open has landed, and open them
        // like a drop on the window. The task ends with
        // the window: `update_in` fails once the view is gone.
        cx.spawn_in(window, async move |this, cx| {
            loop {
                smol::Timer::after(Duration::from_millis(200)).await;
                match this.read_with(cx, |this, _| this.started) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(_) => break,
                }
                let paths: Vec<String> = std::mem::take(
                    &mut *FINDER_OPENS.lock().unwrap_or_else(PoisonError::into_inner),
                );
                let requests = crate::remote::take_pending();
                let state_wanted = crate::remote::state_wanted();
                let presence = crate::remote::take_presence();
                if paths.is_empty() && requests.is_empty() && !state_wanted && presence.is_empty() {
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
                        if !presence.is_empty() {
                            this.workspace.update(cx, |workspace, cx| {
                                for update in &presence {
                                    workspace.apply_presence(update, cx);
                                }
                            });
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
            tour,
            started: false,
            actions: ActionLog::default(),
        }
    }

    /// Paths from outside the app — a drop on the window, or a document
    /// Finder opens with it: a `.dash` file opens a dashboard tab, and
    /// everything else is the same request the command line and the pickers
    /// make.
    fn open_external(
        &mut self,
        requested: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (dashboards, data) = crate::spec::tabs::split(requested);
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
        // Refused before anything moves: the agent decided this from a window
        // that is no longer what the person sees.
        if let Some(based_on) = &request.based_on_revision {
            let current = crate::handoff::revision(&self.snapshot_body(cx));
            if *based_on != current {
                if let Some(id) = request.request_id {
                    let mut action = Action::new(id, ActionStatus::Stale);
                    action.based_on_revision = Some(based_on.clone());
                    action.current_revision = Some(current);
                    action.error = Some("the window changed since that revision".into());
                    self.actions.record(action);
                }
                return;
            }
        }
        window.activate_window();
        let failures_before = self.state.read(cx).open_failures();
        self.open_external(request.paths, window, cx);
        let tab = request.sql.map(|sql| {
            self.workspace.update(cx, |ws, cx| {
                ws.open_query_tab(sql, request.title, window, cx)
            })
        });
        let run_tab = tab.filter(|_| request.run);
        let opening = self.state.read(cx).is_opening();
        if let Some(id) = &request.request_id {
            let status = if opening || run_tab.is_some() {
                ActionStatus::Queued
            } else {
                ActionStatus::Opened
            };
            let mut action = Action::new(id.clone(), status);
            action.tab_id = tab;
            self.actions.record(action);
        }
        if !opening && run_tab.is_none() {
            return;
        }
        let id = request.request_id;
        // The SQL may name what the paths attach, and attaching is off the
        // UI thread; a run already going would turn this one away. Wait out
        // both, then see the run through, so the action says how it ended.
        cx.spawn_in(window, async move |this, cx| {
            let for_run = run_tab.is_some();
            while this
                .read_with(cx, |this, cx| this.waits_to_run(for_run, cx))
                .unwrap_or(false)
            {
                smol::Timer::after(Duration::from_millis(50)).await;
            }
            let open_error = this
                .read_with(cx, |this, cx| {
                    let state = this.state.read(cx);
                    (state.open_failures() != failures_before)
                        .then(|| state.last_open_failure().map(str::to_string))
                        .flatten()
                })
                .unwrap_or(None);
            let Some(tab) = run_tab else {
                this.update(cx, |this, _| {
                    this.settle(&id, open_error, |action| {
                        action.status = ActionStatus::Opened;
                    });
                })
                .ok();
                return;
            };
            let started = loop {
                let attempt = this.update_in(cx, |this, window, cx| {
                    let started = this
                        .workspace
                        .update(cx, |ws, cx| ws.run_query_tab(tab, window, cx));
                    if started {
                        if let Some(id) = &id {
                            this.actions
                                .update(id, |action| action.status = ActionStatus::Running);
                        }
                    }
                    (started, this.workspace.read(cx).busy())
                });
                match attempt {
                    // The person started a run just before this one: wait again.
                    Ok((false, true)) => {
                        smol::Timer::after(Duration::from_millis(50)).await;
                    }
                    Ok((started, _)) => break started,
                    Err(_) => return,
                }
            };
            if !started {
                this.update(cx, |this, _| {
                    this.settle(&id, open_error, |action| {
                        action.status = ActionStatus::Failed;
                        action.error = Some("the tab was closed before its SQL could run".into());
                    });
                })
                .ok();
                return;
            }
            while this
                .read_with(cx, |this, cx| this.waits_to_run(true, cx))
                .unwrap_or(false)
            {
                smol::Timer::after(Duration::from_millis(50)).await;
            }
            this.update(cx, |this, cx| {
                let result = this.workspace.read(cx).query_tab_result(tab, cx);
                this.settle(&id, open_error, |action| match result {
                    None => {
                        action.status = ActionStatus::Failed;
                        action.error = Some("the tab was closed while its SQL ran".into());
                    }
                    Some(result) => {
                        let status = result["status"].as_str().unwrap_or("empty").to_string();
                        if status == "failed" {
                            action.status = ActionStatus::Failed;
                            action.error = result["error"].as_str().map(str::to_string);
                        } else {
                            action.status = ActionStatus::Succeeded;
                        }
                        action.result_status = Some(status);
                    }
                });
            })
            .ok();
        })
        .detach();
    }

    /// Whether an agent's request must keep waiting: for what it attaches,
    /// and, when it runs SQL, for the run already going.
    fn waits_to_run(&self, for_run: bool, cx: &App) -> bool {
        self.state.read(cx).is_opening() || (for_run && self.workspace.read(cx).busy())
    }

    /// Finish request `id`'s action. An attach that failed fails the request
    /// when nothing else did, even if its SQL could run without it: the agent
    /// asked for that data and did not get it.
    fn settle(
        &mut self,
        id: &Option<String>,
        open_error: Option<String>,
        change: impl FnOnce(&mut Action),
    ) {
        let Some(id) = id else { return };
        self.actions.update(id, |action| {
            change(action);
            if let (None, Some(open_error)) = (&action.error, open_error) {
                action.status = ActionStatus::Failed;
                action.error = Some(open_error);
            }
        });
    }

    /// What the window shows, for `ducklocal open --state`: the database, the files
    /// attached to it, and the workspace's tabs.
    fn snapshot(&self, cx: &App) -> serde_json::Value {
        let mut snapshot = crate::handoff::publish(self.snapshot_body(cx), &self.actions);
        self.add_connections(&mut snapshot, cx);
        snapshot
    }

    /// What the connection reaches, and how far each result went. Outside the
    /// revision, like `limits`: a query holding the connection leaves the list
    /// unknown, and that does not change what the window shows.
    fn add_connections(&self, snapshot: &mut serde_json::Value, cx: &App) {
        use crate::connections::{self, AttachedFile};
        let state = self.state.read(cx);
        let databases =
            crate::db::try_with_connection(connections::databases_of).and_then(Result::ok);
        snapshot["connections"] =
            connections::describe(state.s3_config.as_ref(), databases.as_deref());
        let attached: Vec<AttachedFile> = state
            .attached_files
            .iter()
            .map(|file| AttachedFile {
                name: &file.view_name,
                path: &file.path,
            })
            .collect();
        let tabs = snapshot["tabs"].as_array_mut().into_iter().flatten();
        for tab in tabs {
            let Some(sql) = tab["result"]["sql"].as_str().map(str::to_string) else {
                continue;
            };
            tab["result"]["reach"] = connections::reach(
                &sql,
                &attached,
                databases.as_deref(),
                state.s3_config.is_some(),
            );
        }
    }

    /// What the window shows, which is what its revision covers.
    fn snapshot_body(&self, cx: &App) -> serde_json::Value {
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
                "path": if file.path.contains("://") {
                    crate::session::safe_address(&file.path)
                } else {
                    file.path.clone()
                },
                "name": file.view_name,
                "rows": file.row_count,
            }))
            .collect::<Vec<_>>());
        crate::handoff::withhold_credentials(&mut snapshot);
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
            .on_action(cx.listener(|this, action: &ActivateTab, window, cx| {
                let index = action.index;
                this.workspace.update(cx, |workspace, cx| {
                    workspace.activate_tab(index, window, cx)
                });
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| {
                this.workspace
                    .update(cx, |workspace, cx| workspace.cycle_tab(true, window, cx));
            }))
            .on_action(cx.listener(|this, _: &PreviousTab, window, cx| {
                this.workspace
                    .update(cx, |workspace, cx| workspace.cycle_tab(false, window, cx));
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
                                    // Scaled with the interface size: at a
                                    // larger size a fixed 220px is narrower
                                    // than the sidebar's own tab row, and the
                                    // whole sidebar lays out wider than its
                                    // panel and is clipped at the edge.
                                    .size(crate::ui::scale::design(280.))
                                    .size_range(
                                        crate::ui::scale::design(220.)
                                            ..crate::ui::scale::design(420.),
                                    )
                                    // Cached: the sidebar re-renders when it
                                    // is notified (its state events, hover,
                                    // clicks) or the window refreshes (theme,
                                    // language, size) — not with every frame
                                    // of the editor's blinking cursor, which
                                    // would rebuild the history list and the
                                    // tree twice a second while idle.
                                    // The guide's anchor sits outside the
                                    // cached view, whose prepaint is replayed
                                    // rather than run.
                                    .child(
                                        div()
                                            .relative()
                                            .size_full()
                                            .child(
                                                self.sidebar
                                                    .clone()
                                                    .cached(StyleRefinement::default().size_full()),
                                            )
                                            .child(crate::ui::tour::anchor(
                                                crate::ui::tour::Target::Sidebar,
                                            )),
                                    ),
                            )
                            .child(resizable_panel().child(self.workspace.clone())),
                    )
                }
            }))
            .child(self.status_bar.clone())
            .child(self.tour.clone())
    }
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: that pulls in `gpui_kit::*`, whose
    // `test` macro shadows the built-in `#[test]`.
    use super::file_url_path;

    /// What `ducklocal open` does to the window, end to end from the request:
    /// the app is built without its startup, which reads the person's own
    /// remembered tabs and crash reports, on the in-memory stores.
    mod requests {
        use super::super::DuckLocalApp;
        use crate::handoff::{contract::schema_violations, ActionLog};
        use crate::remote::Request;
        use crate::state::AppState;
        use crate::ui::sidebar::Sidebar;
        use crate::ui::status_bar::StatusBarView;
        use crate::ui::title_bar::TitleBarView;
        use crate::ui::tour::Tour;
        use crate::ui::workspace::Workspace;
        use gpui_kit::component::{Theme, ThemeMode};
        use gpui_kit::{
            px, size, AnyWindowHandle, AppContext, Bounds, Entity, Point, TestAppContext,
            WindowBounds, WindowOptions,
        };
        use serde_json::Value;

        fn open_app(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<DuckLocalApp>) {
            cx.update(|cx| {
                gpui_kit::init(cx);
                crate::ui::init(cx);
                Theme::change(ThemeMode::Light, None, cx);
            });
            cx.update(|cx| {
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds {
                            origin: Point::default(),
                            size: size(px(1024.), px(640.)),
                        })),
                        ..Default::default()
                    },
                    cx,
                    |window, cx| {
                        let state = cx.new(AppState::new);
                        let workspace = cx.new(|cx| Workspace::new(state.clone(), window, cx));
                        let sidebar =
                            cx.new(|cx| Sidebar::new(state.clone(), workspace.clone(), window, cx));
                        let title_bar = cx.new(|cx| TitleBarView::new(state.clone(), cx));
                        let status_bar = cx.new(|cx| StatusBarView::new(state.clone(), cx));
                        let tour = cx.new(Tour::new);
                        cx.new(|_| DuckLocalApp {
                            state,
                            title_bar,
                            sidebar,
                            workspace,
                            status_bar,
                            tour,
                            started: true,
                            actions: ActionLog::default(),
                        })
                    },
                )
            })
            .expect("open test window")
        }

        fn send(
            cx: &mut TestAppContext,
            (window, app): &(AnyWindowHandle, Entity<DuckLocalApp>),
            request: Request,
        ) {
            cx.update_window(*window, |_, window, cx| {
                app.update(cx, |app, cx| app.open_remote(request, window, cx));
            })
            .unwrap();
        }

        fn state(cx: &mut TestAppContext, app: &Entity<DuckLocalApp>) -> Value {
            cx.read(|cx| app.read(cx).snapshot(cx))
        }

        /// The state once request `id` has finished; the run happens on real
        /// threads, so this lets them work between turns of the executor.
        fn finished(cx: &mut TestAppContext, app: &Entity<DuckLocalApp>, id: &str) -> Value {
            for _ in 0..1000 {
                cx.run_until_parked();
                let state = state(cx, app);
                let actions = state["actions"].as_array().unwrap();
                let action = actions.iter().find(|a| a["request_id"] == id);
                let status = action.and_then(|a| a["status"].as_str());
                if !matches!(status, None | Some("queued" | "running")) {
                    return action.unwrap().clone();
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("request {id} never finished: {}", state(cx, app)["actions"]);
        }

        fn sql(id: &str, sql: &str) -> Request {
            Request {
                sql: Some(sql.into()),
                run: true,
                request_id: Some(id.into()),
                ..Request::default()
            }
        }

        /// The window as an agent reads it, S3 configured and a key in a tab:
        /// what it reaches is there, and the key is nowhere.
        #[gpui_kit::test]
        fn the_state_says_what_it_reaches_and_no_key(cx: &mut TestAppContext) {
            cx.executor().allow_parking();
            let _guard = crate::db::connection_guard();
            crate::history::with_test_history(|| {
                crate::db::open_memory().unwrap();
                let opened = open_app(cx);
                let app = &opened.1;
                let state_entity = cx.read(|cx| app.read(cx).state.clone());
                state_entity.update(cx, |state, cx| {
                    state.set_s3_configured(
                        Some(crate::s3::S3Config {
                            endpoint: "s3.example.com".into(),
                            region: "eu-west-1".into(),
                            key_id: "AKIDEXAMPLEVALUE".into(),
                            secret: "wJalrXUtnFEMIEXAMPLEVALUE".into(),
                        }),
                        cx,
                    );
                });

                send(cx, &opened, sql("local", "SELECT 1 AS n"));
                assert_eq!(finished(cx, app, "local")["status"], "succeeded");
                let typed = "CREATE SECRET s (TYPE s3, KEY_ID 'AKIDEXAMPLEVALUE', \
                             SECRET 'wJalrXUtnFEMIEXAMPLEVALUE')";
                send(
                    cx,
                    &opened,
                    Request {
                        sql: Some(typed.into()),
                        request_id: Some("typed".into()),
                        ..Request::default()
                    },
                );
                finished(cx, app, "typed");

                let shown = state(cx, app);
                assert_eq!(schema_violations(&shown), Vec::<String>::new());
                let text = shown.to_string();
                assert!(!text.contains("AKIDEXAMPLEVALUE"), "{text}");
                assert!(!text.contains("wJalrXUtnFEMI"), "{text}");
                assert_eq!(shown["connections"]["s3"]["endpoint"], "s3.example.com");
                assert_eq!(shown["connections"]["s3"]["region"], "eu-west-1");
                assert!(shown["connections"]["databases"].is_array());

                let tabs = shown["tabs"].as_array().unwrap();
                let ran = tabs
                    .iter()
                    .find(|tab| tab["sql"] == "SELECT 1 AS n")
                    .unwrap();
                assert_eq!(ran["result"]["reach"]["reach"], "local");
                assert!(ran.get("sql_withheld").is_none());
                let hidden = tabs.iter().find(|tab| tab["sql_withheld"] == true).unwrap();
                assert_eq!(hidden["sql"], crate::session::REDACTED);

                // Reading twice is the same window.
                assert_eq!(state(cx, app)["revision"], shown["revision"]);
            });
        }

        #[gpui_kit::test]
        fn a_request_reports_how_it_went(cx: &mut TestAppContext) {
            // The run is real: its query and timers wake from other threads.
            cx.executor().allow_parking();
            let _guard = crate::db::connection_guard();
            crate::history::with_test_history(|| {
                crate::db::open_memory().unwrap();
                let opened = open_app(cx);
                let app = &opened.1;

                // Based on a window that is gone: refused, and nothing moved.
                let before = state(cx, app);
                let tabs = before["tabs"].as_array().unwrap().len();
                let mut stale = sql("stale", "SELECT 1");
                stale.based_on_revision = Some("not-this-one".into());
                send(cx, &opened, stale);
                let action = finished(cx, app, "stale");
                assert_eq!(action["status"], "stale");
                assert_eq!(action["based_on_revision"], "not-this-one");
                assert_eq!(action["current_revision"], before["revision"]);
                let after = state(cx, app);
                assert_eq!(after["tabs"].as_array().unwrap().len(), tabs);
                assert_eq!(after["revision"], before["revision"]);

                // Based on the window as it is: runs, under the tab it says.
                let mut current = sql("current", "SELECT 41 + 1 AS answer");
                current.based_on_revision = before["revision"].as_str().map(str::to_string);
                send(cx, &opened, current);
                let action = finished(cx, app, "current");
                assert_eq!(action["status"], "succeeded", "{action}");
                assert_eq!(action["result_status"], "rows");
                let after = state(cx, app);
                assert_eq!(schema_violations(&after), Vec::<String>::new());
                let tab = after["tabs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|tab| tab["id"] == action["tab_id"])
                    .expect("the tab the action names");
                assert_eq!(tab["result"]["preview"][0][0], "42");

                // A failed run is told apart from one that was delivered.
                send(cx, &opened, sql("broken", "SELECT * FROM no_such_table"));
                let action = finished(cx, app, "broken");
                assert_eq!(action["status"], "failed");
                assert_eq!(action["result_status"], "failed");
                assert!(action["error"].as_str().unwrap().contains("no_such_table"));

                // Two at once: the second used to be dropped while the first
                // ran, and would have run whichever tab was then in front.
                send(cx, &opened, sql("first", "SELECT 1 AS n"));
                send(cx, &opened, sql("second", "SELECT 2 AS n"));
                let first = finished(cx, app, "first");
                let second = finished(cx, app, "second");
                assert_eq!(first["status"], "succeeded", "{first}");
                assert_eq!(second["status"], "succeeded", "{second}");
                assert_ne!(first["tab_id"], second["tab_id"]);
                let shown = state(cx, app);
                let result_of = |action: &Value| {
                    shown["tabs"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|tab| tab["id"] == action["tab_id"])
                        .unwrap()["result"]["preview"][0][0]
                        .clone()
                };
                assert_eq!(result_of(&first), "1");
                assert_eq!(result_of(&second), "2");

                // Nothing to run: opened, and no result to report.
                send(
                    cx,
                    &opened,
                    Request {
                        sql: Some("SELECT 3".into()),
                        request_id: Some("shown".into()),
                        ..Request::default()
                    },
                );
                let action = finished(cx, app, "shown");
                assert_eq!(action["status"], "opened");
                assert_eq!(action["result_status"], Value::Null);

                // Data that will not attach is a failure, not a delivery.
                send(
                    cx,
                    &opened,
                    Request {
                        paths: vec!["ducklocal_no_such_file_anywhere.csv".into()],
                        request_id: Some("missing".into()),
                        ..Request::default()
                    },
                );
                let action = finished(cx, app, "missing");
                assert_eq!(action["status"], "failed", "{action}");
                assert!(action["error"].is_string(), "{action}");

                // A request from an older command is carried out, untracked.
                let quiet = state(cx, app)["actions"].as_array().unwrap().len();
                send(
                    cx,
                    &opened,
                    Request {
                        sql: Some("SELECT 4".into()),
                        ..Request::default()
                    },
                );
                cx.run_until_parked();
                assert_eq!(state(cx, app)["actions"].as_array().unwrap().len(), quiet);
            });
        }
    }

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
