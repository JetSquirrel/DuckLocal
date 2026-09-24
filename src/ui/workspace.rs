//! Center workspace: query tabs and app tabs, the toolbar that belongs to
//! whichever is active, the SQL editor or the app, and the results panel,
//! split vertically by a resizable handle.
//!
//! A tab is either a query — an editor and the results of running it — an
//! app, an agent-authored JavaScript view over the database the window is
//! on, or a dashboard, a `.dash` spec rendered as a stack of resizable plots.
//! They are peers: the tab strip mixes them, closing one is closing a tab, and
//! renaming works on all three. What differs is the region and the toolbar
//! behind the strip, which is why every path that reaches for "the active
//! editor" goes through `as_query` rather than assuming one.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState, TabSize};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{resizable_panel, v_resizable};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use gpui_shell::ShellRuntime;

use crate::analysis::apps::{self, OpenApp};
use gpui_kit::assets::IconName as AssetIcon;
use crate::analysis::runtime;
use crate::analysis::view::AnalysisHost;
use crate::i18n::{tr, trf};
use crate::query::QueryOutcome;
use crate::spec::view::Dashboard;
use crate::state::{AppState, CatalogChanged, ConnectionChanged, QueryStats};
use crate::ui::completion;
use crate::ui::results::ResultsPanel;
use crate::ui::{
    pick_paths, PickerTarget, RunQuery, SaveSpec, RUN_QUERY_KEYSTROKE, WORKSPACE_KEY_CONTEXT,
};

const RESULTS_PANEL_DEFAULT: f32 = 320.;
const RESULTS_PANEL_MIN: f32 = 160.;
const RESULTS_PANEL_MAX: f32 = 640.;

/// Line length the first-run description wraps at. Wide enough for the
/// sentence to read as one thought, narrow enough that the eye does not have
/// to travel the whole window.
const FIRST_RUN_TEXT_WIDTH: f32 = 400.;

pub struct QueryTab {
    pub id: u64,
    pub title: SharedString,
    pub editor: Entity<EditorState>,
    /// This tab's own results. One panel shared by every tab showed tab A's
    /// rows under tab B's editor — and exported them with A's SQL.
    pub results: Entity<ResultsPanel>,
}

/// An agent-authored app, open as a tab.
pub struct AppTab {
    pub id: u64,
    pub title: SharedString,
    pub directory: PathBuf,
    pub host: Entity<AnalysisHost>,
}

/// A `.dash` spec, open as a tab.
pub struct DashboardTab {
    pub id: u64,
    pub title: SharedString,
    pub path: PathBuf,
    pub host: Entity<Dashboard>,
    /// The catalog changed while this tab was in the background; it re-runs
    /// when it is next shown rather than competing with the query that
    /// changed it.
    pub stale: bool,
}

pub enum WorkspaceTab {
    Query(QueryTab),
    App(AppTab),
    Dashboard(DashboardTab),
}

/// Which kind a tab is, without borrowing it.
///
/// The editor commands are decided from a list of these rather than from the
/// tabs themselves, so the decision is testable without a window: an app or a
/// dashboard is not an editor, and the arithmetic of what stays active after a
/// close does not depend on what the tabs hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabKind {
    Query,
    App,
    Dashboard,
}

impl TabKind {
    /// The one glyph a kind of tab wears — on the tab, in the `+` menu, and
    /// for apps in the sidebar too — so a kind reads the same everywhere.
    pub fn glyph(self) -> AssetIcon {
        match self {
            TabKind::Query => AssetIcon::SquareTerminal,
            TabKind::App => AssetIcon::AppWindow,
            TabKind::Dashboard => AssetIcon::LayoutDashboard,
        }
    }
}

/// Where content aimed at "the active editor" lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorTarget {
    /// The active tab is a query: write into it.
    Active,
    /// Write into the query tab at this index.
    Switch(usize),
    /// No query tab is open, so the content needs a new one.
    New,
}

/// The tab index left active after the tab at `closed` is removed.
fn active_after_close(active: usize, closed: usize, remaining: usize) -> usize {
    if remaining == 0 {
        return 0;
    }
    if active >= remaining {
        remaining - 1
    } else if active > closed {
        active - 1
    } else {
        active
    }
}

/// Where a request for the active editor's content goes.
///
/// An app tab has no editor at all, and a dashboard's editor holds `.dash`
/// spec rather than SQL, so the request is answered by the nearest query tab
/// — the last one open — or by a new one. Sending it nowhere would make the
/// action look broken, and writing into a hidden editor would be the same
/// thing with extra steps.
fn editor_target(kinds: &[TabKind], active: usize) -> EditorTarget {
    if kinds.get(active) == Some(&TabKind::Query) {
        return EditorTarget::Active;
    }
    match kinds.iter().rposition(|kind| *kind == TabKind::Query) {
        Some(index) => EditorTarget::Switch(index),
        None => EditorTarget::New,
    }
}

/// The titles that more than one tab carries. Two `dashboard.dash` files from
/// different folders both default to `dashboard`, and a strip of identical
/// labels gives no way to tell which tab is which.
pub(crate) fn duplicate_titles<'a>(titles: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut seen = Vec::new();
    let mut duplicates = Vec::new();
    for title in titles {
        if seen.contains(&title) {
            if !duplicates.contains(&title) {
                duplicates.push(title);
            }
        } else {
            seen.push(title);
        }
    }
    duplicates
}

/// The name of the folder `path` sits in: what tells two same-titled
/// documents apart.
pub(crate) fn parent_name(path: &Path) -> Option<String> {
    path.parent()?
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
}

impl WorkspaceTab {
    pub fn id(&self) -> u64 {
        match self {
            WorkspaceTab::Query(tab) => tab.id,
            WorkspaceTab::App(tab) => tab.id,
            WorkspaceTab::Dashboard(tab) => tab.id,
        }
    }

    pub fn title(&self) -> &SharedString {
        match self {
            WorkspaceTab::Query(tab) => &tab.title,
            WorkspaceTab::App(tab) => &tab.title,
            WorkspaceTab::Dashboard(tab) => &tab.title,
        }
    }

    /// What to show after a title another tab shares: the folder a document
    /// came from. A query tab is only ever named by the user, so it has none.
    fn disambiguator(&self) -> Option<String> {
        match self {
            WorkspaceTab::Query(_) => None,
            // An app's title is its folder, so the folder above says where.
            WorkspaceTab::App(tab) => parent_name(&tab.directory),
            WorkspaceTab::Dashboard(tab) => parent_name(&tab.path),
        }
    }

    /// The tab as a query, which is the only kind whose editor takes SQL and
    /// has a toolbar of SQL commands and a result set to run into.
    pub fn as_query(&self) -> Option<&QueryTab> {
        match self {
            WorkspaceTab::Query(tab) => Some(tab),
            WorkspaceTab::App(_) | WorkspaceTab::Dashboard(_) => None,
        }
    }

    pub fn kind(&self) -> TabKind {
        match self {
            WorkspaceTab::Query(_) => TabKind::Query,
            WorkspaceTab::App(_) => TabKind::App,
            WorkspaceTab::Dashboard(_) => TabKind::Dashboard,
        }
    }

    pub fn as_app(&self) -> Option<&AppTab> {
        match self {
            WorkspaceTab::App(tab) => Some(tab),
            WorkspaceTab::Query(_) | WorkspaceTab::Dashboard(_) => None,
        }
    }

    pub fn as_dashboard(&self) -> Option<&DashboardTab> {
        match self {
            WorkspaceTab::Dashboard(tab) => Some(tab),
            WorkspaceTab::Query(_) | WorkspaceTab::App(_) => None,
        }
    }

    /// The tab's focusable text editor: a query's SQL editor, or a
    /// dashboard's source editor once its source view has been opened. An
    /// app has none.
    pub fn editor(&self, cx: &App) -> Option<Entity<EditorState>> {
        match self {
            WorkspaceTab::Query(tab) => Some(tab.editor.clone()),
            WorkspaceTab::Dashboard(tab) => tab.host.read(cx).source_editor(),
            WorkspaceTab::App(_) => None,
        }
    }
}

pub struct Workspace {
    state: Entity<AppState>,
    tabs: Vec<WorkspaceTab>,
    active: usize,
    next_tab_id: u64,
    running: bool,
    explaining: bool,
    /// Set once the user asks for the editor on a connection with no data yet,
    /// which is otherwise the first-run screen's job to keep out of the way.
    editor_shown: bool,
    rename_input: Option<Entity<InputState>>,
    _subscriptions: Vec<Subscription>,
    /// Declared last on purpose: fields drop in declaration order, and every
    /// app tab's mounted script view holds QuickJS handles into this
    /// runtime, so they have to be released before it is. See
    /// [`crate::analysis::runtime`] for why dropping it late is fatal rather
    /// than untidy.
    runtime: Option<Rc<ShellRuntime>>,
}

impl Workspace {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            state: state.clone(),
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 1,
            running: false,
            explaining: false,
            editor_shown: false,
            rename_input: None,
            _subscriptions: vec![
                cx.subscribe(&state, |this, _, _: &ConnectionChanged, cx| {
                    // A dashboard queries the window's connection, so a switch
                    // of database re-runs it — the way an app follows the
                    // window to another database.
                    for tab in &mut this.tabs {
                        if let WorkspaceTab::Dashboard(tab) = tab {
                            tab.stale = false;
                            tab.host.update(cx, |dashboard, cx| dashboard.reload(cx));
                        }
                    }
                    cx.notify()
                }),
                cx.subscribe(&state, |this, _, _: &CatalogChanged, cx| {
                    // A table a dashboard reads may have changed: re-run the
                    // one on screen, and the rest when they are next shown.
                    // Re-running every open dashboard after each DDL would
                    // compete with the user's next query.
                    for tab in &mut this.tabs {
                        if let WorkspaceTab::Dashboard(tab) = tab {
                            tab.stale = true;
                        }
                    }
                    this.refresh_stale_dashboard(cx);
                }),
            ],
            runtime: None,
        };
        let tab = this.new_tab_editor(window, cx);
        let run_hint = Keystroke::parse(RUN_QUERY_KEYSTROKE)
            .map(|k| Kbd::format(&k))
            .unwrap_or_else(|_| "⌘↵".to_string());
        tab.editor.update(cx, |editor, cx| {
            editor.set_value(trf("workspace.welcome_sql", &[&run_hint]), window, cx);
        });
        this.tabs.push(WorkspaceTab::Query(tab));
        this
    }

    fn new_tab_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> QueryTab {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .tab_size(TabSize {
                    tab_size: 2,
                    hard_tabs: false,
                })
        });
        let provider = completion::SqlCompletionProvider::new(self.state.clone());
        editor.update(cx, |state, cx| {
            state.lsp_mut().completion_provider = Some(provider);
            cx.notify();
        });
        // ⌘↵ reaches the editor as `secondary-enter`, which the Input key
        // context (deeper than the workspace's) binds to "insert newline".
        // The keybinding in `ui::init` therefore never fires while the editor
        // is focused; the PressEnter event is the only signal left. The
        // newline is already inserted by then, so undo it before running.
        let subscription = cx.subscribe_in(&editor, window, |this, editor, event, window, cx| {
            if matches!(
                event,
                InputEvent::PressEnter {
                    secondary: true,
                    ..
                }
            ) {
                Self::remove_secondary_enter_newline(editor, window, cx);
                this.run_active(window, cx);
            }
        });
        self._subscriptions.push(subscription);
        QueryTab {
            id,
            title: trf("workspace.tab.default_title", &[&id.to_string()]).into(),
            editor,
            results: cx.new(|cx| ResultsPanel::new(window, cx)),
        }
    }

    /// The runtime every app shares, created with the first app.
    ///
    /// A failure is returned rather than logged so the app that asked for it
    /// can say why it cannot run, in its own tab.
    fn runtime(&mut self, cx: &mut Context<Self>) -> Result<Rc<ShellRuntime>, String> {
        if let Some(runtime) = &self.runtime {
            return Ok(runtime.clone());
        }
        match runtime::create(cx) {
            Ok(created) => {
                self.runtime = Some(created.clone());
                Ok(created)
            }
            Err(error) => Err(format!("{error:#}")),
        }
    }

    /// Open an app tab. `directory` is validated by the host, so a folder that
    /// cannot be an app is reported inside the tab it would have filled.
    pub fn open_app(&mut self, directory: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        // The same app twice is the same tab brought forward: apps are
        // documents, and two tabs on one document is a way to edit half of it
        // and wonder why the other half is stale.
        let existing = self
            .tabs
            .iter()
            .position(|tab| tab.as_app().is_some_and(|app| app.directory == directory));
        if let Some(ix) = existing {
            let title = self.tabs[ix].title().to_string();
            self.note_recent(&directory, crate::recents::RecentKind::App, &title, cx);
            self.activate(ix, window, cx);
            return;
        }

        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let title = apps::title_for(&directory);
        let runtime = self.runtime(cx);
        let host = cx.new(|cx| AnalysisHost::new(directory.clone(), runtime, window, cx));
        self.tabs.push(WorkspaceTab::App(AppTab {
            id,
            title: title.clone().into(),
            directory: directory.clone(),
            host,
        }));
        self.active = self.tabs.len() - 1;
        self.note_recent(&directory, crate::recents::RecentKind::App, &title, cx);
        self.remember_apps();
        cx.notify();
    }

    /// Open a `.dash` spec as a dashboard tab. Like an app, the same file
    /// twice is the same tab brought forward, and the open set is remembered
    /// between launches.
    pub fn open_dashboard(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self.tabs.iter().position(|tab| {
            tab.as_dashboard()
                .is_some_and(|dashboard| dashboard.path == path)
        });
        if let Some(ix) = existing {
            let title = self.tabs[ix].title().to_string();
            self.note_recent(&path, crate::recents::RecentKind::Dashboard, &title, cx);
            self.activate(ix, window, cx);
            return;
        }

        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let title = crate::spec::tabs::title_for(&path);
        let host = cx.new(|cx| Dashboard::new(id, path.clone(), cx));
        self.tabs.push(WorkspaceTab::Dashboard(DashboardTab {
            id,
            title: title.clone().into(),
            path: path.clone(),
            host,
            stale: false,
        }));
        self.active = self.tabs.len() - 1;
        self.note_recent(&path, crate::recents::RecentKind::Dashboard, &title, cx);
        self.remember_dashboards();
        cx.notify();
    }

    /// Note an opening in the sidebar's recent-documents list, and let the
    /// sidebar know its document groups changed.
    fn note_recent(
        &self,
        path: &std::path::Path,
        kind: crate::recents::RecentKind,
        title: &str,
        cx: &mut Context<Self>,
    ) {
        crate::recents::add(path, kind, title);
        self.state
            .update(cx, |_, cx| cx.emit(crate::state::RecentsChanged));
    }

    /// Open several apps, as launch does: the command line's directories
    /// first, then the ones remembered from last time.
    pub fn open_apps(
        &mut self,
        directories: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for directory in directories {
            self.open_app(directory, window, cx);
        }
    }

    /// Write the open apps down, so the next launch can put them back.
    fn remember_apps(&self) {
        let open: Vec<OpenApp> = self
            .tabs
            .iter()
            .filter_map(|tab| {
                tab.as_app()
                    .map(|app| OpenApp::new(app.directory.clone(), app.title.to_string()))
            })
            .collect();
        // Blocking, like the language setting: one small row, on a user action.
        apps::remember(&open);
    }

    /// Write the open dashboards down, the same deal the apps get.
    fn remember_dashboards(&self) {
        let open: Vec<OpenApp> = self
            .tabs
            .iter()
            .filter_map(|tab| {
                tab.as_dashboard().map(|dashboard| {
                    OpenApp::new(dashboard.path.clone(), dashboard.title.to_string())
                })
            })
            .collect();
        crate::spec::tabs::remember(&open);
    }

    pub fn focus_active_editor(&self, window: &mut Window, cx: &mut App) {
        if let Some(editor) = self
            .tabs
            .get(self.active)
            .and_then(|tab| tab.editor(cx))
        {
            editor.read(cx).focus_handle(cx).focus(window, cx);
        }
    }

    /// Replace the active editor's content with a history entry.
    ///
    /// An app tab has no editor, and a dashboard's editor holds `.dash` spec
    /// rather than SQL, so the SQL goes to the query tab the reader would
    /// expect it in: the last one open, or a new one. Sending it nowhere
    /// would make the action look broken, and sending it to a hidden editor
    /// would be the same thing with extra steps.
    pub fn fill_active_editor(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        let kinds: Vec<TabKind> = self.tabs.iter().map(WorkspaceTab::kind).collect();
        self.active = match editor_target(&kinds, self.active) {
            EditorTarget::Active => self.active,
            EditorTarget::Switch(index) => index,
            EditorTarget::New => {
                let tab = self.new_tab_editor(window, cx);
                self.tabs.push(WorkspaceTab::Query(tab));
                self.tabs.len() - 1
            }
        };
        if let Some(tab) = self.tabs.get(self.active).and_then(WorkspaceTab::as_query) {
            tab.editor.update(cx, |editor, cx| {
                editor.set_value(sql, window, cx);
            });
        }
        self.editor_shown = true;
        cx.notify();
        self.focus_active_editor(window, cx);
    }

    /// The editor inserts `"\n" + indent` before emitting `PressEnter`; the
    /// cursor then sits right after the insertion. Delete exactly that
    /// insertion — walk back over the indent, then require a newline, and do
    /// nothing if the text before the cursor does not match that shape.
    /// With multiple cursors only the active one is repaired.
    fn remove_secondary_enter_newline(
        editor: &Entity<EditorState>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let range = editor.update(cx, |editor, _| {
            let cursor = editor.cursor();
            let text = editor.text();
            let mut start = cursor;
            let mut chars = text.chars_at(cursor);
            let mut prev = chars.prev();
            // The inserted indent is plain spaces/tabs (ASCII), so byte
            // arithmetic on `start` stays on char boundaries.
            while matches!(prev, Some(' ' | '\t')) {
                start -= 1;
                prev = chars.prev();
            }
            if prev != Some('\n') {
                return None;
            }
            Some(start - 1..cursor)
        });
        if let Some(range) = range {
            editor.update(cx, |editor, cx| {
                editor.set_selected_range(range, cx);
                editor.replace("", window, cx);
            });
        }
    }

    fn active_tab(&self) -> Option<&WorkspaceTab> {
        self.tabs.get(self.active)
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = index;
        self.refresh_stale_dashboard(cx);
        cx.notify();
        self.focus_active_editor(window, cx);
    }

    /// Re-run the active tab's dashboard if the catalog changed since it last
    /// ran (see `DashboardTab::stale`).
    fn refresh_stale_dashboard(&mut self, cx: &mut Context<Self>) {
        let active = self.active;
        if let Some(WorkspaceTab::Dashboard(tab)) = self.tabs.get_mut(active) {
            if std::mem::take(&mut tab.stale) {
                tab.host.update(cx, |dashboard, cx| dashboard.reload(cx));
            }
        }
    }

    fn add_query_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.new_tab_editor(window, cx);
        self.tabs.push(WorkspaceTab::Query(tab));
        self.active = self.tabs.len() - 1;
        self.editor_shown = true;
        cx.notify();
        self.focus_active_editor(window, cx);
    }

    /// The tab strip's "+": a query, or an app folder to open as one.
    fn pick_app_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(tr("analysis.picker.prompt").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(directory) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                // A folder that is not an app opens the tab that says so,
                // rather than being refused with nothing to look at.
                this.open_app(directory, window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// The `+` menu's "Open dashboard…": a `.dash` file, from the system
    /// picker. Anything else is refused with a note, not opened as data.
    fn pick_dashboard_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr("dashboard.picker.prompt").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                if crate::spec::tabs::is_spec(&path) {
                    this.open_dashboard(path, window, cx);
                } else {
                    window.push_notification(
                        trf("dashboard.picker.not_a_spec", &[&path.to_string_lossy()]),
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    fn close_tab(&mut self, tab_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 {
            return;
        }
        let Some(ix) = self.tabs.iter().position(|tab| tab.id() == tab_id) else {
            return;
        };
        // A dashboard with unsaved edits asks first; everything else closes.
        if let Some(dashboard) = self.tabs[ix].as_dashboard() {
            if dashboard.host.read(cx).is_dirty(cx) {
                self.confirm_close_dashboard(tab_id, window, cx);
                return;
            }
        }
        self.close_tab_confirmed(tab_id, window, cx);
    }

    fn close_tab_confirmed(&mut self, tab_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 {
            return;
        }
        let Some(ix) = self.tabs.iter().position(|tab| tab.id() == tab_id) else {
            return;
        };
        self.tabs.remove(ix);
        self.active = active_after_close(self.active, ix, self.tabs.len());
        self.refresh_stale_dashboard(cx);
        self.remember_apps();
        self.remember_dashboards();
        cx.notify();
        self.focus_active_editor(window, cx);
    }

    /// Closing a dashboard whose source holds unsaved edits: save and close,
    /// close without saving, or stay.
    fn confirm_close_dashboard(&mut self, tab_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = self
            .tabs
            .iter()
            .find(|tab| tab.id() == tab_id)
            .map(|tab| tab.title().to_string())
        else {
            return;
        };
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(tr("dashboard.close_unsaved.title"))
                .w(crate::ui::scale::design(420.))
                .child(
                    div()
                        .text_sm()
                        .child(trf("dashboard.close_unsaved.body", &[&title])),
                )
                .footer(
                    DialogFooter::new()
                        .gap_2()
                        .child(
                            Button::new("cancel")
                                .outline()
                                .label(tr("common.cancel"))
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("discard")
                                .danger()
                                .label(tr("dashboard.close_unsaved.discard"))
                                .on_click({
                                    let view = view.clone();
                                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                        window.close_dialog(cx);
                                        if let Some(view) = view.upgrade() {
                                            view.update(cx, |this, cx| {
                                                this.close_tab_confirmed(tab_id, window, cx);
                                            });
                                        }
                                    }
                                }),
                        )
                        .child(
                            Button::new("save-and-close")
                                .primary()
                                .label(tr("dashboard.close_unsaved.save_close"))
                                .on_click({
                                    let view = view.clone();
                                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                        window.close_dialog(cx);
                                        if let Some(view) = view.upgrade() {
                                            view.update(cx, |this, cx| {
                                                // A failed save keeps the tab:
                                                // closing now would lose the
                                                // edits the dialog promised to
                                                // keep.
                                                if this.save_dashboard(tab_id, window, cx) {
                                                    this.close_tab_confirmed(tab_id, window, cx);
                                                }
                                            });
                                        }
                                    }
                                }),
                        ),
                )
        });
    }

    /// Save the dashboard tab's source back to its file; `false` — with the
    /// reason notified — when the write failed.
    fn save_dashboard(&mut self, tab_id: u64, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(host) = self
            .tabs
            .iter()
            .find(|tab| tab.id() == tab_id)
            .and_then(|tab| tab.as_dashboard())
            .map(|tab| tab.host.clone())
        else {
            return false;
        };
        match host.update(cx, |dashboard, cx| dashboard.save(cx)) {
            Ok(()) => true,
            Err(message) => {
                window.push_notification(Notification::error(message), cx);
                false
            }
        }
    }

    /// ⌘S saves the active dashboard's source; other tabs have nothing to
    /// save, so the keystroke is theirs to ignore.
    fn save_spec_action(&mut self, _: &SaveSpec, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab_id) = self
            .tabs
            .get(self.active)
            .and_then(WorkspaceTab::as_dashboard)
            .map(|tab| tab.id)
        else {
            return;
        };
        self.save_dashboard(tab_id, window, cx);
    }

    /// The active query's SQL, with the results panel its outcome belongs to:
    /// captured when the run starts, so a result that lands after the user
    /// switched tabs still goes to the tab that asked.
    fn active_sql(&self, cx: &App) -> Option<(String, Entity<ResultsPanel>)> {
        let tab = self
            .tabs
            .get(self.active)
            .and_then(WorkspaceTab::as_query)?;
        let sql = tab.editor.read(cx).value().to_string();
        (!sql.trim().is_empty()).then(|| (sql, tab.results.clone()))
    }

    fn open_rename_dialog(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(tab.title().clone()));
        self.rename_input = Some(input.clone());
        let view = cx.entity().downgrade();

        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(tr("dialog.rename.title"))
                .w(crate::ui::scale::design(360.))
                .child(Input::new(&input))
                .footer(
                    DialogFooter::new()
                        .gap_2()
                        .child(
                            Button::new("cancel")
                                .outline()
                                .label(tr("common.cancel"))
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("confirm-rename")
                                .primary()
                                .label(tr("dialog.rename.confirm"))
                                .on_click({
                                    let input = input.clone();
                                    let view = view.clone();
                                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                        let title = input.read(cx).value().trim().to_string();
                                        window.close_dialog(cx);
                                        if let Some(view) = view.upgrade() {
                                            view.update(cx, |this, cx| {
                                                this.rename_active_tab(&title, cx);
                                            });
                                        }
                                    }
                                }),
                        ),
                )
        });
    }

    /// Rename the active tab; an empty title falls back to the tab's own name.
    fn rename_active_tab(&mut self, title: &str, cx: &mut Context<Self>) {
        let default_title = self.tabs.get(self.active).map(|tab| match tab {
            WorkspaceTab::Query(tab) => trf("workspace.tab.default_title", &[&tab.id.to_string()]),
            WorkspaceTab::App(tab) => apps::title_for(&tab.directory),
            WorkspaceTab::Dashboard(tab) => crate::spec::tabs::title_for(&tab.path),
        });
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let fallback = default_title.unwrap_or_default();
        let title = if title.is_empty() {
            fallback
        } else {
            title.to_string()
        };
        match tab {
            WorkspaceTab::Query(tab) => tab.title = title.into(),
            WorkspaceTab::App(tab) => tab.title = title.into(),
            WorkspaceTab::Dashboard(tab) => tab.title = title.into(),
        }
        self.remember_apps();
        self.remember_dashboards();
        cx.notify();
    }

    fn run_query_action(&mut self, _: &RunQuery, window: &mut Window, cx: &mut Context<Self>) {
        self.run_active(window, cx);
    }

    fn run_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running || self.explaining {
            return;
        }
        let Some((sql, results)) = self.active_sql(cx) else {
            return;
        };
        // ⌘↵ on the first-run screen means "I want to write SQL": show the
        // editor the results belong to instead of running behind it.
        self.editor_shown = true;
        self.running = true;
        results.update(cx, |results, cx| results.set_running(cx));
        cx.notify();

        let state = self.state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let run_sql = sql.clone();
            let (outcome, history, catalog) = smol::unblock(move || {
                // Decided before `run_sql` is handed to the history entry.
                let reload_catalog = crate::query::may_change_catalog(&run_sql);
                let outcome = crate::query::run(&run_sql);
                let (duration_ms, row_count, ok, error) = match &outcome {
                    Ok(QueryOutcome::Rows(result)) => (
                        result.elapsed_ms as i64,
                        Some(result.row_count() as i64),
                        true,
                        None,
                    ),
                    Ok(QueryOutcome::Affected { count, elapsed_ms }) => {
                        (*elapsed_ms as i64, Some(*count as i64), true, None)
                    }
                    Err(e) => (0, None, false, Some(e.to_string())),
                };
                crate::history::record(&crate::history::HistoryEntry {
                    id: 0,
                    sql: run_sql,
                    started_at: crate::history::now_timestamp(),
                    duration_ms,
                    row_count,
                    ok,
                    error,
                })
                .ok();
                let history = crate::history::recent(200).unwrap_or_default();
                // A plain read leaves the sidebar valid, so skip the reload
                // instead of paying for it on the way back from every query.
                let catalog =
                    reload_catalog.then(|| crate::schema::load_catalog().unwrap_or_default());
                (outcome, history, catalog)
            })
            .await;

            this.update_in(cx, move |this, window, cx| {
                this.running = false;
                let stats = match &outcome {
                    Ok(QueryOutcome::Rows(result)) => Some(QueryStats {
                        elapsed_ms: result.elapsed_ms,
                        rows: result.row_count(),
                        cols: result.columns.len(),
                    }),
                    Ok(QueryOutcome::Affected { elapsed_ms, count }) => Some(QueryStats {
                        elapsed_ms: *elapsed_ms,
                        rows: *count as usize,
                        cols: 0,
                    }),
                    Err(_) => None,
                };
                results.update(cx, |results, cx| {
                    results.set_outcome(outcome, sql.clone(), window, cx);
                });
                state.update(cx, |s, cx| {
                    s.set_history(history, cx);
                    if let Some(catalog) = catalog {
                        s.set_catalog(catalog, cx);
                    }
                    if let Some(stats) = stats {
                        s.set_last_query(stats, sql.clone(), cx);
                    }
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn format_active(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active).and_then(WorkspaceTab::as_query) else {
            return;
        };
        tab.editor.update(cx, |editor, cx| {
            let sql = editor.value().to_string();
            let formatted = sqlformat::format(
                &sql,
                &sqlformat::QueryParams::None,
                &sqlformat::FormatOptions {
                    indent: sqlformat::Indent::Spaces(2),
                    uppercase: Some(true),
                    ..Default::default()
                },
            );
            if !formatted.trim().is_empty() {
                editor.replace_all(formatted, window, cx);
            }
        });
    }

    fn explain_active(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.explaining || self.running {
            return;
        }
        let Some((sql, results)) = self.active_sql(cx) else {
            return;
        };
        self.explaining = true;
        results.update(cx, |results, cx| results.set_running(cx));
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let explain_sql = sql.clone();
            let result = smol::unblock(move || {
                crate::db::with_connection(|conn| crate::query::explain_of(conn, &explain_sql))
            })
            .await;
            this.update_in(cx, move |this, window, cx| {
                this.explaining = false;
                results.update(cx, |results, cx| {
                    results.set_explain(result, window, cx);
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_tab_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let closable = self.tabs.len() > 1;
        let duplicates = duplicate_titles(self.tabs.iter().map(|tab| tab.title().as_ref()));
        TabBar::new("query-tabs")
            .small()
            .selected_index(self.active)
            .on_click(cx.listener(|this, ix, window, cx| {
                this.activate(*ix, window, cx);
            }))
            .children(self.tabs.iter().map(|tab| {
                let tab_id = tab.id();
                let title = tab.title().clone();
                // Same-titled tabs say which folder they came from, muted,
                // so the label still reads as the title first.
                let hint = duplicates
                    .contains(&title.as_ref())
                    .then(|| tab.disambiguator())
                    .flatten();
                // The glyph and the label share the tab's padded content.
                // `prefix` would sit outside that padding — flush against the
                // previous tab's close button, and a gap away from its own
                // label — so the glyph reads as the wrong tab's.
                let glyph = tab.kind().glyph();
                // A dashboard shows a dot while its source buffer holds edits
                // the file does not.
                let dirty = tab
                    .as_dashboard()
                    .is_some_and(|dashboard| dashboard.host.read(cx).is_dirty(cx));
                Tab::new()
                    .aria_label(title.clone())
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(Icon::new(glyph).small())
                            .child(title)
                            .when_some(hint, |this, hint| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(hint),
                                )
                            }),
                    )
                    .when(closable || dirty, |this| {
                        this.suffix(
                            h_flex()
                                .gap_1()
                                .pr_1()
                                .items_center()
                                .when(dirty, |this| {
                                    this.child(
                                        div()
                                            .size_1p5()
                                            .rounded_full()
                                            .bg(cx.theme().warning),
                                    )
                                })
                                .when(closable, |this| {
                                    this.child(
                                        Button::new(("close-tab", tab_id as usize))
                                            .ghost()
                                            .xsmall()
                                            .icon(IconName::Close)
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.close_tab(tab_id, window, cx);
                                            })),
                                    )
                                }),
                        )
                    })
            }))
            .suffix({
                let view = cx.entity().downgrade();
                Button::new("add-tab")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip(tr("workspace.add_tab.tooltip"))
                    .dropdown_menu(move |menu, _window, _cx| {
                        let new_query = view.clone();
                        let open_app = view.clone();
                        let open_dashboard = view.clone();
                        // Each entry wears the glyph its tab will wear.
                        menu.item(
                            PopupMenuItem::new(tr("workspace.new_query"))
                                .icon(TabKind::Query.glyph())
                                .on_click(move |_, window, cx| {
                                    if let Some(view) = new_query.upgrade() {
                                        view.update(cx, |this, cx| this.add_query_tab(window, cx));
                                    }
                                }),
                        )
                        .item(
                            PopupMenuItem::new(tr("workspace.open_panel"))
                                .icon(TabKind::App.glyph())
                                .on_click(move |_, window, cx| {
                                    if let Some(view) = open_app.upgrade() {
                                        view.update(cx, |this, cx| {
                                            this.pick_app_directory(window, cx)
                                        });
                                    }
                                }),
                        )
                        .item(
                            PopupMenuItem::new(tr("workspace.open_dashboard"))
                                .icon(TabKind::Dashboard.glyph())
                                .on_click(move |_, window, cx| {
                                    if let Some(view) = open_dashboard.upgrade() {
                                        view.update(cx, |this, cx| {
                                            this.pick_dashboard_file(window, cx)
                                        });
                                    }
                                }),
                        )
                    })
            })
    }

    fn render_toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        match self.active_tab() {
            Some(WorkspaceTab::App(_)) => self.render_app_toolbar(cx).into_any_element(),
            Some(WorkspaceTab::Dashboard(_)) => self.render_dashboard_toolbar(cx).into_any_element(),
            _ => self.render_query_toolbar(cx).into_any_element(),
        }
    }

    fn render_query_toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let run_keystroke = Keystroke::parse(RUN_QUERY_KEYSTROKE).ok();

        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("run-query")
                    .primary()
                    .small()
                    .icon(IconName::Play)
                    .label(tr("workspace.run"))
                    .loading(self.running)
                    .tooltip(tr("workspace.run.tooltip"))
                    .when_some(run_keystroke, |this, keystroke| {
                        this.child(Kbd::new(keystroke))
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.run_active(window, cx);
                    })),
            )
            // An accidental cross join can run for minutes; this ends it
            // rather than leaving the only way out to quit the app.
            .when(self.running, |this| {
                this.child(
                    Button::new("stop-query")
                        .outline()
                        .small()
                        .icon(AssetIcon::CircleStop)
                        .label(tr("workspace.stop"))
                        .tooltip(tr("workspace.stop.tooltip"))
                        .on_click(|_, _, _| crate::db::interrupt()),
                )
            })
            .child(
                Button::new("format-sql")
                    .outline()
                    .small()
                    .icon(AssetIcon::WandSparkles)
                    .label(tr("workspace.format"))
                    .tooltip(tr("workspace.format.tooltip"))
                    .on_click(cx.listener(Self::format_active)),
            )
            .child(
                Button::new("explain-sql")
                    .outline()
                    .small()
                    .icon(AssetIcon::ListTree)
                    .label("EXPLAIN")
                    .loading(self.explaining)
                    .tooltip(tr("workspace.explain.tooltip"))
                    .on_click(cx.listener(Self::explain_active)),
            )
            .child(div().flex_1())
            .child(Self::rename_button("rename-tab", cx))
    }

    /// Every toolbar ends with the same Rename: bordered, with its glyph,
    /// like the buttons beside it.
    fn rename_button(id: &'static str, cx: &mut Context<Self>) -> Button {
        Button::new(id)
            .outline()
            .small()
            .icon(AssetIcon::Pencil)
            .label(tr("workspace.rename"))
            .tooltip(tr("workspace.rename.tooltip"))
            .on_click(cx.listener(Self::open_rename_dialog))
    }

    /// What an app tab's toolbar says instead of Run/Format/EXPLAIN: where the
    /// app came from, how to reload it, and how to read what it is.
    fn render_app_toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let (directory, showing_definition) = match self.active_tab() {
            Some(WorkspaceTab::App(tab)) => (
                Some(tab.directory.clone()),
                tab.host.read(cx).is_showing_definition(),
            ),
            _ => (None, false),
        };
        let host = self
            .active_tab()
            .and_then(WorkspaceTab::as_app)
            .map(|tab| tab.host.clone());

        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Icon::new(AssetIcon::AppWindow)
                    .small()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .children(directory.map(|directory| directory.to_string_lossy().to_string())),
            )
            .child(
                Button::new("app-definition")
                    .outline()
                    .small()
                    // The glyph of where the button goes: the source, or back
                    // to the app.
                    .icon(if showing_definition {
                        AssetIcon::AppWindow
                    } else {
                        AssetIcon::Code
                    })
                    .label(if showing_definition {
                        tr("analysis.definition.back")
                    } else {
                        tr("analysis.definition.show")
                    })
                    .tooltip(tr("analysis.definition.tooltip"))
                    .when_some(host.clone(), |this, host| {
                        this.on_click(cx.listener(move |_, _, _, cx| {
                            host.update(cx, |host, cx| host.toggle_definition(cx));
                        }))
                    }),
            )
            .child(
                Button::new("app-refresh")
                    .outline()
                    .small()
                    .icon(IconName::RotateCw)
                    .label(tr("analysis.reload"))
                    .tooltip(tr("analysis.reload.tooltip"))
                    .when_some(host, |this, host| {
                        this.on_click(cx.listener(move |_, _, window, cx| {
                            host.update(cx, |host, cx| host.refresh(window, cx));
                        }))
                    }),
            )
            .child(Self::rename_button("app-rename", cx))
    }

    /// What a dashboard tab's toolbar says instead: which spec this is, a
    /// toggle between the rendered dashboard and the spec's source, and a
    /// reload that re-reads it and re-runs its queries.
    fn render_dashboard_toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let (path, host, running, showing_source, dirty) = match self.active_tab() {
            Some(WorkspaceTab::Dashboard(tab)) => (
                Some(tab.path.clone()),
                Some(tab.host.clone()),
                tab.host.read(cx).is_running(),
                tab.host.read(cx).is_showing_source(),
                tab.host.read(cx).is_dirty(cx),
            ),
            _ => (None, None, false, false, false),
        };

        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Icon::new(IconName::LayoutDashboard)
                    .small()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .children(path.map(|path| path.to_string_lossy().to_string())),
            )
            .child(
                Button::new("dashboard-save")
                    .outline()
                    .small()
                    .icon(AssetIcon::Save)
                    .label(tr("dashboard.save"))
                    .tooltip(tr("dashboard.save.tooltip"))
                    .disabled(!dirty)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(tab_id) = this
                            .tabs
                            .get(this.active)
                            .and_then(WorkspaceTab::as_dashboard)
                            .map(|tab| tab.id)
                        {
                            this.save_dashboard(tab_id, window, cx);
                        }
                    })),
            )
            .child(
                Button::new("dashboard-source")
                    .outline()
                    .small()
                    .icon(if showing_source {
                        AssetIcon::LayoutDashboard
                    } else {
                        AssetIcon::Code
                    })
                    .label(if showing_source {
                        tr("dashboard.source.back")
                    } else {
                        tr("dashboard.source.show")
                    })
                    .tooltip(tr("dashboard.source.tooltip"))
                    .when_some(host.clone(), |this, host| {
                        this.on_click(cx.listener(move |_, _, _, cx| {
                            host.update(cx, |dashboard, cx| dashboard.toggle_source(cx));
                        }))
                    }),
            )
            .child(
                Button::new("dashboard-reload")
                    .outline()
                    .small()
                    .icon(IconName::RotateCw)
                    .label(tr("analysis.reload"))
                    .loading(running)
                    .tooltip(tr("dashboard.reload.tooltip"))
                    .when_some(host, |this, host| {
                        this.on_click(cx.listener(move |_, _, _, cx| {
                            host.update(cx, |dashboard, cx| dashboard.reload(cx));
                        }))
                    }),
            )
            .child(Self::rename_button("dashboard-rename", cx))
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let first_run = self.is_first_run(cx);

        v_flex()
            .id("workspace")
            .size_full()
            .min_w_0()
            .key_context(WORKSPACE_KEY_CONTEXT)
            .on_action(cx.listener(Self::run_query_action))
            .on_action(cx.listener(Self::save_spec_action))
            .when(first_run, |this| this.child(self.render_first_run(cx)))
            .when(!first_run, |this| {
                let region = self.render_region();
                this.child(self.render_tab_bar(cx))
                    .child(self.render_toolbar(cx))
                    .child(region)
            })
    }
}

impl Workspace {
    /// The tab's own region: a query is an editor over its results, and an
    /// app or dashboard is the view itself, with nothing split off below it.
    fn render_region(&mut self) -> AnyElement {
        match self.active_tab() {
            Some(WorkspaceTab::App(tab)) => div()
                .flex_1()
                .min_h_0()
                .child(tab.host.clone())
                .into_any_element(),
            Some(WorkspaceTab::Dashboard(tab)) => div()
                .flex_1()
                .min_h_0()
                .child(tab.host.clone())
                .into_any_element(),
            _ => {
                let query = self.tabs.get(self.active).and_then(WorkspaceTab::as_query);
                let editor = query.map(|tab| tab.editor.clone());
                let results = query.map(|tab| tab.results.clone());
                div()
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_resizable("editor-results")
                            .child(resizable_panel().child(div().size_full().when_some(
                                editor,
                                |this, editor| {
                                    this.child(Editor::new(&editor).h(relative(1.)).bordered(false))
                                },
                            )))
                            .child(
                                resizable_panel()
                                    .size(px(RESULTS_PANEL_DEFAULT))
                                    .size_range(px(RESULTS_PANEL_MIN)..px(RESULTS_PANEL_MAX))
                                    .children(results),
                            ),
                    )
                    .into_any_element()
            }
        }
    }

    /// The first-run screen stands where the editor would be: only once
    /// startup has landed, only while there is nothing to query, and only
    /// until the user asks for the editor instead. An app or dashboard tab is
    /// something to look at, so it takes the screen over the invitation.
    fn is_first_run(&self, cx: &App) -> bool {
        let state = self.state.read(cx);
        !self.editor_shown
            && self.tabs.iter().all(|tab| tab.as_query().is_some())
            && state.is_ready()
            && !state.has_data()
    }

    /// What the app opens onto before any data is around: the drop zone. The
    /// window accepts drops anywhere, so this is the invitation rather than the
    /// only target.
    fn render_first_run(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.clone();
        let picker_state = state.clone();

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                // Drawn from the bundle the app ships: the wider Lucide
                // catalog names icons whose SVGs are not embedded.
                Icon::new(IconName::FolderOpen)
                    .large()
                    // Deliberately faded: the icon is decoration, not data.
                    .text_color(cx.theme().muted_foreground.alpha(0.5)),
            )
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(tr("workspace.empty.title")),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .max_w(crate::ui::scale::design(FIRST_RUN_TEXT_WIDTH))
                    .text_center()
                    .child(tr("workspace.empty.description")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("first-run-open-files")
                            .primary()
                            .label(tr("workspace.empty.open_files"))
                            .on_click(move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                pick_paths(
                                    state.clone(),
                                    PickerTarget::Files,
                                    tr("workspace.empty.files_prompt"),
                                    window,
                                    cx,
                                );
                            }),
                    )
                    .child(
                        Button::new("first-run-open-folder")
                            .outline()
                            .label(tr("workspace.empty.open_folder"))
                            .on_click(move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                pick_paths(
                                    picker_state.clone(),
                                    PickerTarget::Folder,
                                    tr("workspace.empty.folder_prompt"),
                                    window,
                                    cx,
                                );
                            }),
                    )
                    .child(
                        Button::new("first-run-new-query")
                            .ghost()
                            .label(tr("workspace.new_query"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.editor_shown = true;
                                cx.notify();
                                this.focus_active_editor(window, cx);
                            })),
                    )
                    .child(
                        Button::new("first-run-open-app")
                            .ghost()
                            .label(tr("workspace.open_panel"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.pick_app_directory(window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("workspace.empty.cli_hint")),
            )
    }
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: that pulls in `gpui_kit::*`, whose
    // `test` macro shadows the built-in `#[test]`.
    use super::{
        active_after_close, duplicate_titles, editor_target, parent_name, EditorTarget, TabKind,
    };
    use std::path::Path;

    /// Three tabs with an app in the middle, the arrangement the branch
    /// mistakes would show up in.
    const MIXED: [TabKind; 3] = [TabKind::Query, TabKind::App, TabKind::Query];

    #[test]
    fn only_titles_carried_twice_are_duplicates() {
        let titles = ["Query 1", "dashboard", "sales", "dashboard", "dashboard"];
        assert_eq!(duplicate_titles(titles), vec!["dashboard"]);
        assert!(duplicate_titles(["Query 1", "Query 2"]).is_empty());
    }

    #[test]
    fn a_document_is_told_apart_by_its_folder() {
        assert_eq!(
            parent_name(Path::new("/x/examples/usage_panel/dashboard.dash")).as_deref(),
            Some("usage_panel")
        );
        assert_eq!(parent_name(Path::new("/")), None);
    }

    #[test]
    fn closing_the_active_tab_moves_to_the_one_before_it() {
        // The app in the middle closes: the tab that takes its place is the
        // one that slid into its index.
        assert_eq!(active_after_close(1, 1, 2), 1);
        // The last tab closes: there is nothing after it to move to.
        assert_eq!(active_after_close(2, 2, 2), 1);
        // The only tab left cannot be closed, and the index stays put.
        assert_eq!(active_after_close(0, 0, 1), 0);
        assert_eq!(active_after_close(0, 0, 0), 0);
    }

    #[test]
    fn closing_a_tab_before_the_active_one_keeps_its_place() {
        // A query tab before the active app closes: the app is still the
        // active tab, one index earlier.
        assert_eq!(active_after_close(2, 0, 2), 1);
        assert_eq!(active_after_close(1, 0, 2), 0);
    }

    #[test]
    fn closing_a_tab_after_the_active_one_leaves_the_index_alone() {
        assert_eq!(active_after_close(0, 2, 2), 0);
        assert_eq!(active_after_close(0, 1, 2), 0);
    }

    #[test]
    fn an_app_tab_never_takes_editor_content() {
        // The active tab is the app: the content goes to a query tab, and
        // never into the app.
        assert_eq!(editor_target(&MIXED, 1), EditorTarget::Switch(2));
        // With the app last, the query tab before it is the nearest one.
        let active_app_last = [TabKind::Query, TabKind::Query, TabKind::App];
        assert_eq!(
            editor_target(&active_app_last, 2),
            EditorTarget::Switch(1)
        );
    }

    #[test]
    fn a_query_tab_takes_the_content_where_it_stands() {
        assert_eq!(editor_target(&MIXED, 0), EditorTarget::Active);
        assert_eq!(editor_target(&MIXED, 2), EditorTarget::Active);
    }

    #[test]
    fn with_no_query_tab_open_the_content_needs_a_new_one() {
        let only_apps = [TabKind::App, TabKind::App];
        assert_eq!(editor_target(&only_apps, 0), EditorTarget::New);
        assert_eq!(editor_target(&[], 0), EditorTarget::New);
    }
}
