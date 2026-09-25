//! UI layer: title bar, sidebar, workspace, results, chart, status bar.

pub mod chart;
pub mod completion;
pub mod results;
pub mod scale;
pub mod setup_dialog;
pub mod sidebar;
pub mod status_bar;
pub mod title_bar;
pub mod workspace;

use gpui_kit::component::notification::Notification;
use gpui_kit::component::WindowExt;
use gpui_kit::{App, Entity, KeyBinding, PathPromptOptions, Window};

use crate::i18n::{tr, trf};
use crate::sources::MAX_FILES;
use crate::state::{self, AppState, AttachOutcome, OpenOutcome, RequestReport};

gpui_kit::actions!(
    ducklocal,
    [
        RunQuery,
        StopQuery,
        SaveSpec,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        ToggleSidebar,
        NewQuery,
        CloseTab,
        OpenData,
        OpenSetup,
        Quit
    ]
);

/// Key context that makes ⌘↵ reachable while the SQL editor is focused.
pub const WORKSPACE_KEY_CONTEXT: &str = "DuckLocal";

// `secondary` is ⌘ on macOS and Ctrl on Windows and Linux. `cmd` would be
// the Windows key there, which left every shortcut unreachable.
pub const RUN_QUERY_KEYSTROKE: &str = "secondary-enter";
pub const SAVE_SPEC_KEYSTROKE: &str = "secondary-s";
pub const TOGGLE_SIDEBAR_KEYSTROKE: &str = "secondary-b";

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(RUN_QUERY_KEYSTROKE, RunQuery, Some(WORKSPACE_KEY_CONTEXT)),
        KeyBinding::new(SAVE_SPEC_KEYSTROKE, SaveSpec, Some(WORKSPACE_KEY_CONTEXT)),
        // Interface size, with the keys every Mac app uses for it. Bound with
        // no context so they work wherever focus is, the editor included;
        // `secondary-+` is what a keyboard without a separate plus key sends
        // as shift-equals.
        KeyBinding::new("secondary-=", ZoomIn, None),
        KeyBinding::new("secondary-+", ZoomIn, None),
        KeyBinding::new("secondary--", ZoomOut, None),
        KeyBinding::new("secondary-0", ZoomReset, None),
        // Handled by the root view, which owns the layout, the tabs and the
        // title bar these commands act on.
        KeyBinding::new(TOGGLE_SIDEBAR_KEYSTROKE, ToggleSidebar, None),
        KeyBinding::new("secondary-t", NewQuery, None),
        KeyBinding::new("secondary-w", CloseTab, None),
        KeyBinding::new("secondary-o", OpenData, None),
        KeyBinding::new("secondary-.", StopQuery, None),
        KeyBinding::new("secondary-q", Quit, None),
    ]);
    cx.on_action(|_: &ZoomIn, cx| scale::set(scale::current().larger(), cx));
    cx.on_action(|_: &ZoomOut, cx| scale::set(scale::current().smaller(), cx));
    cx.on_action(|_: &ZoomReset, cx| scale::set(scale::UiSize::Default, cx));
    cx.on_action(|_: &StopQuery, _| crate::db::interrupt());
    cx.on_action(|_: &Quit, cx| cx.quit());
    // After the bindings: the menu bar reads each item's shortcut from the
    // keymap at the moment it is built.
    set_menus(cx);
}

/// The menu bar: every command the window has, with its shortcut, where a
/// Mac user looks for it. Rebuilt when the language changes.
pub fn set_menus(cx: &mut App) {
    use gpui_kit::component::input;
    use gpui_kit::{Menu, MenuItem};
    let menu = |name: &'static str, items: Vec<MenuItem>| Menu {
        name: tr(name).into(),
        items,
        disabled: false,
    };
    cx.set_menus(vec![
        Menu {
            name: "DuckLocal".into(),
            items: vec![
                MenuItem::action(tr("setup.open"), OpenSetup),
                MenuItem::Separator,
                MenuItem::action(tr("menu.quit"), Quit),
            ],
            disabled: false,
        },
        menu(
            "menu.file",
            vec![
                MenuItem::action(tr("workspace.new_query"), NewQuery),
                MenuItem::action(tr("title_bar.open_data"), OpenData),
                MenuItem::Separator,
                MenuItem::action(tr("dashboard.save"), SaveSpec),
                MenuItem::action(tr("menu.close_tab"), CloseTab),
            ],
        ),
        menu(
            "menu.edit",
            vec![
                MenuItem::action(tr("menu.undo"), input::Undo),
                MenuItem::action(tr("menu.redo"), input::Redo),
                MenuItem::Separator,
                MenuItem::action(tr("menu.cut"), input::Cut),
                MenuItem::action(tr("menu.copy"), input::Copy),
                MenuItem::action(tr("menu.paste"), input::Paste),
                MenuItem::action(tr("menu.select_all"), input::SelectAll),
            ],
        ),
        menu(
            "menu.query",
            vec![
                MenuItem::action(tr("workspace.run"), RunQuery),
                MenuItem::action(tr("workspace.stop"), StopQuery),
            ],
        ),
        menu(
            "menu.view",
            vec![
                MenuItem::action(tr("menu.toggle_sidebar"), ToggleSidebar),
                MenuItem::Separator,
                MenuItem::action(tr("menu.zoom_in"), ZoomIn),
                MenuItem::action(tr("menu.zoom_out"), ZoomOut),
                MenuItem::action(tr("menu.zoom_reset"), ZoomReset),
            ],
        ),
    ]);
}

/// Run an "open these paths" request off the UI thread and apply the result.
///
/// This is the one path request shared by dropped files and the pickers, so a
/// drop and a chosen file cannot behave differently.
pub fn open_paths(state: Entity<AppState>, paths: Vec<String>, window: &mut Window, cx: &mut App) {
    if paths.is_empty() {
        return;
    }
    run_open_request(
        state,
        move || state::open_request(&paths, false),
        window,
        cx,
    );
}

pub fn open_dialog_path(state: Entity<AppState>, path: String, window: &mut Window, cx: &mut App) {
    run_open_request(state, move || state::open_dialog_path(&path), window, cx);
}

/// Open the in-memory connection, re-attaching the registered files: the way
/// back from a database connection that is no longer wanted.
pub fn open_memory(state: Entity<AppState>, window: &mut Window, cx: &mut App) {
    run_open_request(state, || state::open_request(&[], true), window, cx);
}

fn run_open_request(
    state: Entity<AppState>,
    request: impl FnOnce() -> anyhow::Result<OpenOutcome> + Send + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    state.update(cx, |state, cx| state.begin_open(cx));
    window
        .spawn(cx, async move |cx| {
            let result = smol::unblock(request).await;
            cx.update(move |window, cx| {
                state.update(cx, |state, cx| state.end_open(cx));
                match result {
                    Ok(outcome) => apply_open_outcome(state, outcome, window, cx),
                    Err(e) => window.push_notification(
                        Notification::error(trf("notify.open.failed", &[&e.to_string()])),
                        cx,
                    ),
                }
            })
            .ok();
        })
        .detach();
}

/// Which entries the system picker accepts.
#[derive(Clone, Copy)]
pub enum PickerTarget {
    /// Data files only.
    Files,
    /// One folder, whose data files are expanded recursively.
    Folder,
}

impl PickerTarget {
    fn options(self, prompt: &'static str) -> PathPromptOptions {
        PathPromptOptions {
            files: matches!(self, PickerTarget::Files),
            directories: matches!(self, PickerTarget::Folder),
            multiple: matches!(self, PickerTarget::Files),
            prompt: Some(prompt.into()),
        }
    }
}

/// Ask the system picker for entries of `target` and open the selection.
/// Nothing happens when the user cancels.
pub fn pick_paths(
    state: Entity<AppState>,
    target: PickerTarget,
    prompt: &'static str,
    window: &mut Window,
    cx: &mut App,
) {
    let rx = cx.prompt_for_paths(target.options(prompt));
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let paths: Vec<String> = paths
                .iter()
                .map(|path| path.to_string_lossy().to_string())
                .collect();
            cx.update(move |window, cx| open_paths(state, paths, window, cx))
                .ok();
        })
        .detach();
}

/// Apply what an open request produced: the connection when a database was
/// opened, the sidebar's data either way, and one notification per outcome.
pub fn apply_open_outcome(
    state: Entity<AppState>,
    outcome: OpenOutcome,
    window: &mut Window,
    cx: &mut App,
) {
    let (connected, attach) = match outcome {
        OpenOutcome::Connected(connected) => (
            Some((connected.target, connected.server, connected.database_error)),
            connected.attach,
        ),
        OpenOutcome::Attached(attach) => (None, attach),
    };
    let database_error = connected.as_ref().and_then(|(_, _, error)| error.clone());
    let AttachOutcome {
        report,
        catalog,
        history,
        attached,
    } = attach;

    state.update(cx, |state, cx| match connected {
        Some((target, server, _)) => {
            state.set_connection(target, server, catalog, cx);
            state.set_history(history, cx);
            state.set_attached_files(attached, cx);
        }
        None => {
            state.set_catalog(catalog, cx);
            state.set_history(history, cx);
            state.set_attached_files(attached, cx);
        }
    });

    if let Some(error) = database_error {
        window.push_notification(
            Notification::error(trf("notify.connect.failed", &[&error])),
            cx,
        );
    }
    notify_request_report(&report, window, cx);
}

/// One notification per request: what was attached, then what went wrong.
/// Failures are collapsed to the first message plus a count — a folder of
/// unreadable files should not bury the window in toasts.
pub fn notify_request_report(report: &RequestReport, window: &mut Window, cx: &mut App) {
    for problem in &report.problems {
        window.push_notification(Notification::error(problem.clone()), cx);
    }

    match report.created.len() {
        0 => {}
        1 => window.push_notification(trf("notify.attach.success", &[&report.created[0]]), cx),
        count => window.push_notification(trf("notify.attach.count", &[&count.to_string()]), cx),
    }

    if let Some(first) = report.failed.first() {
        window.push_notification(Notification::error(first.clone()), cx);
        if report.failed.len() > 1 {
            let remaining = (report.failed.len() - 1).to_string();
            window.push_notification(
                Notification::error(trf("notify.attach.more_failed", &[&remaining])),
                cx,
            );
        }
    }

    if report.truncated {
        window.push_notification(
            trf("notify.attach.truncated", &[&MAX_FILES.to_string()]),
            cx,
        );
    }
}
