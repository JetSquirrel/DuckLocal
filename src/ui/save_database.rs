//! Keeping what was built in the in-memory database: "Save as database…",
//! and the question asked before a quit that would lose it.
//!
//! A plain launch works in memory, so the tables and views made there end
//! with the process. The registered files and the query tabs come back at the
//! next launch and those do not, which is why leaving asks first: a window
//! that looks the way it was left should hold what it held.

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::WindowExt;
use gpui_kit::*;

use crate::db::DatabaseTarget;
use crate::i18n::{tr, trf};
use crate::state::{self, AppState, OpenOutcome, OpenRequest};

/// The name the save panel starts with.
const SUGGESTED_NAME: &str = "workspace.duckdb";

/// What was being done when the unsaved tables and views came up, and so
/// what happens once they are saved or let go.
#[derive(Clone)]
pub enum After {
    /// "Save as database…" by itself: nothing follows.
    Stay,
    Quit,
    /// Open what was asked for, which replaces the in-memory database.
    Open(OpenRequest),
}

/// The window's state, for the commands that belong to the app rather than
/// to a view (Quit, Save as database): their handlers get no window and no
/// view to read it from.
struct WindowState(Entity<AppState>);

impl Global for WindowState {}

pub fn register_state(state: Entity<AppState>, cx: &mut App) {
    cx.set_global(WindowState(state));
}

/// Run `f` in the window, once whatever is dispatching has let go of it: an
/// app-wide action handler runs while the window is lent out, and updating
/// it from there fails. Answers through `otherwise` when there is no window.
fn in_window(
    cx: &mut App,
    f: impl FnOnce(Entity<AppState>, &mut Window, &mut App) + 'static,
    otherwise: impl FnOnce(&mut App) + 'static,
) {
    cx.defer(move |cx| {
        let state = cx.try_global::<WindowState>().map(|held| held.0.clone());
        let window = cx.active_window().or_else(|| cx.windows().first().copied());
        let (Some(state), Some(window)) = (state, window) else {
            return otherwise(cx);
        };
        // `f` moves into the one closure that runs; a window that went away
        // in between leaves it uncalled, and there is nothing left to ask.
        if window
            .update(cx, move |_, window, cx| f(state, window, cx))
            .is_err()
        {
            otherwise(cx);
        }
    });
}

/// The Quit command: quits, unless that would lose tables or views, in which
/// case it asks.
pub fn quit(cx: &mut App) {
    in_window(cx, request_quit, |cx| cx.quit());
}

/// The "Save as database…" command.
pub fn save_from_menu(cx: &mut App) {
    in_window(
        cx,
        |state, window, cx| save(state, After::Stay, window, cx),
        |_| {},
    );
}

fn request_quit(state: Entity<AppState>, window: &mut Window, cx: &mut App) {
    match state.read(cx).unsaved_objects() {
        0 => cx.quit(),
        count => confirm(state, count, After::Quit, window, cx),
    }
}

/// Whether the window may close now. With tables or views that would be lost
/// it stays, and asks; the answer closes it.
pub fn may_close(state: Entity<AppState>, window: &mut Window, cx: &mut App) -> bool {
    match state.read(cx).unsaved_objects() {
        0 => true,
        count => {
            confirm(state, count, After::Quit, window, cx);
            false
        }
    }
}

/// An open request was held back because it would end the in-memory
/// database and the `count` tables and views in it: ask, then open.
pub fn confirm_open(
    state: Entity<AppState>,
    count: usize,
    request: OpenRequest,
    window: &mut Window,
    cx: &mut App,
) {
    confirm(state, count, After::Open(request), window, cx);
}

/// Carry on without saving: the person chose to let the tables and views go.
fn proceed(state: Entity<AppState>, after: After, window: &mut Window, cx: &mut App) {
    match after {
        After::Stay => {}
        After::Quit => cx.quit(),
        After::Open(request) => crate::ui::run_open_request(state, request, true, window, cx),
    }
}

/// About to lose `count` tables and views that are only in memory: save them
/// to a database file and carry on, carry on without them, or stay.
fn confirm(state: Entity<AppState>, count: usize, after: After, window: &mut Window, cx: &mut App) {
    let (title, body, discard) = match after {
        After::Open(_) => (
            "open_unsaved.title",
            "open_unsaved.body",
            "open_unsaved.discard",
        ),
        After::Quit | After::Stay => (
            "quit_unsaved.title",
            "quit_unsaved.body",
            "quit_unsaved.discard",
        ),
    };
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(tr(title))
            .w(crate::ui::scale::design(460.))
            .child(div().text_sm().child(trf(body, &[&count.to_string()])))
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
                        Button::new("without-saving")
                            .danger()
                            .label(tr(discard))
                            .on_click({
                                let (state, after) = (state.clone(), after.clone());
                                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                    window.close_dialog(cx);
                                    proceed(state.clone(), after.clone(), window, cx);
                                }
                            }),
                    )
                    .child(
                        Button::new("save-first")
                            .primary()
                            .label(tr("quit_unsaved.save"))
                            .on_click({
                                let (state, after) = (state.clone(), after.clone());
                                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                    window.close_dialog(cx);
                                    save(state.clone(), after.clone(), window, cx);
                                }
                            }),
                    ),
            )
    });
}

/// Ask where, write the in-memory database there, and move the window onto
/// the file; then do what `after` says. A save that is cancelled or fails
/// does none of it: the window stays, with everything still in memory.
pub fn save(state: Entity<AppState>, after: After, window: &mut Window, cx: &mut App) {
    if let Some(DatabaseTarget::File(path)) = &state.read(cx).target {
        let path = crate::db::compact_home(path);
        window.push_notification(trf("notify.save_database.already_file", &[&path]), cx);
        return;
    }
    let rx = cx.prompt_for_new_path(&start_directory(), Some(SUGGESTED_NAME));
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(path))) = rx.await else {
                return;
            };
            let path = path.to_string_lossy().into_owned();
            let opening = state.clone();
            if cx
                .update(move |_, cx| opening.update(cx, |state, cx| state.begin_open(cx)))
                .is_err()
            {
                return;
            }
            let result = smol::unblock(move || state::save_memory_as(&path)).await;
            cx.update(move |window, cx| {
                state.update(cx, |state, cx| state.end_open(cx));
                match result {
                    Ok(outcome) => {
                        let saved = match &outcome {
                            OpenOutcome::Connected(connected) => connected.target.file_label(),
                            OpenOutcome::Attached(_) => None,
                        };
                        crate::ui::apply_open_outcome(state.clone(), outcome, window, cx);
                        if matches!(after, After::Quit) {
                            return cx.quit();
                        }
                        // Said before the next database opens over it.
                        if let Some(saved) = saved {
                            window.push_notification(
                                trf("notify.save_database.saved", &[&saved]),
                                cx,
                            );
                        }
                        proceed(state, after, window, cx);
                    }
                    Err(e) => window.push_notification(
                        Notification::error(trf("notify.save_database.failed", &[&e.to_string()])),
                        cx,
                    ),
                }
            })
            .ok();
        })
        .detach();
}

/// Where the save panel opens: Documents, or the home folder without one.
fn start_directory() -> PathBuf {
    directories::UserDirs::new()
        .map(|dirs| {
            dirs.document_dir()
                .unwrap_or_else(|| dirs.home_dir())
                .to_path_buf()
        })
        .unwrap_or_else(|| PathBuf::from("."))
}
