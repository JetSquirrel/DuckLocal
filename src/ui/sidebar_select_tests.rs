//! Picking several local files in the sidebar with Shift and Cmd+click, as
//! the pointer does it: the press reaches the row through the tree, which
//! keeps a single selection of its own.

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::test::TestWindowExt;
// No `gpui_kit::*` glob here: it imports gpui's `test` attribute macro, which
// shadows the built-in `#[test]` in the macro-generated code and recurses.
use gpui_kit::{
    point, px, size, AnyWindowHandle, App, AppContext, Bounds, Entity, InputEvent, Modifiers,
    MouseButton, MouseDownEvent, MouseUpEvent, Point, TestAppContext, Window, WindowBounds,
    WindowOptions,
};

use crate::state::{AppState, AttachedFileView};
use crate::ui::sidebar::Sidebar;
use crate::ui::workspace::Workspace;

/// A sidebar listing files 1 to 5 under the Local files heading.
/// A language switch in another test rebuilds the tree, and with it drops the
/// tree's selected row: hold the language still while a test runs.
fn open_sidebar(
    cx: &mut TestAppContext,
) -> (
    AnyWindowHandle,
    Entity<Sidebar>,
    std::sync::MutexGuard<'static, ()>,
) {
    let guard = crate::ui::language_toggle_tests::language_guard();
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::ui::init(cx);
        Theme::change(ThemeMode::Light, None, cx);
    });
    let (window, sidebar) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(320.), px(600.)),
                })),
                ..Default::default()
            },
            cx,
            |window, cx| {
                let state = cx.new(AppState::new);
                let workspace = cx.new(|cx| Workspace::new(state.clone(), window, cx));
                let view = cx.new(|cx| Sidebar::new(state.clone(), workspace, window, cx));
                let files = (1..=5)
                    .map(|id| AttachedFileView {
                        id,
                        path: format!("data{id}.csv"),
                        view_name: format!("data{id}"),
                        row_count: None,
                    })
                    .collect();
                state.update(cx, |state, cx| state.set_attached_files(files, cx));
                view
            },
        )
        .expect("open test window")
    });
    (window, sidebar, guard)
}

/// Press and release on file `id`'s row, on its name, with `modifiers` held;
/// `rows_up` moves the press that many rows above it.
fn press(window: &mut Window, id: i64, rows_up: f32, modifiers: Modifiers, cx: &mut App) {
    window.render_frame(cx);
    // Found by the row's remove button, the one element on it the window
    // registers by a unique id, and pressed left of it, on the name: a press
    // on the button keeps the picked files as they are.
    let button = window.find(("remove-file", id as usize)).bounds();
    let height = button.size.height.max(px(24.));
    let position = point(
        button.origin.x - px(120.),
        button.center().y - height * rows_up,
    );
    window.dispatch_event(
        MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
            first_mouse: false,
        }
        .to_platform_input(),
        cx,
    );
    window.dispatch_event(
        MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
}

fn click_file(window: &mut Window, id: i64, modifiers: Modifiers, cx: &mut App) {
    press(window, id, 0., modifiers, cx);
}

fn shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..Default::default()
    }
}

#[gpui_kit::test]
fn shift_click_picks_the_files_between(cx: &mut TestAppContext) {
    let (window, sidebar, _guard) = open_sidebar(cx);
    cx.update_window(window, |_, window, cx| {
        click_file(window, 2, Modifiers::default(), cx);
        assert_eq!(sidebar.read(cx).picked_files(cx), (vec![2], Some(2)));

        click_file(window, 4, shift(), cx);
        let (picked, selected) = sidebar.read(cx).picked_files(cx);
        assert_eq!(picked, [2, 3, 4], "the run from the first click to here");
        assert_eq!(selected, Some(4), "the tree follows the row just clicked");

        // A plain click starts over.
        click_file(window, 5, Modifiers::default(), cx);
        assert_eq!(sidebar.read(cx).picked_files(cx).0, [5]);
    })
    .unwrap();
}

#[gpui_kit::test]
fn cmd_click_adds_and_drops_one_file(cx: &mut TestAppContext) {
    let (window, sidebar, _guard) = open_sidebar(cx);
    cx.update_window(window, |_, window, cx| {
        click_file(window, 1, Modifiers::default(), cx);
        click_file(window, 3, Modifiers::secondary_key(), cx);
        assert_eq!(sidebar.read(cx).picked_files(cx), (vec![1, 3], Some(3)));

        click_file(window, 3, Modifiers::secondary_key(), cx);
        assert_eq!(
            sidebar.read(cx).picked_files(cx),
            (vec![1], None),
            "a dropped file is not left as the tree's selected row"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn clicking_the_group_heading_clears_the_picked_files(cx: &mut TestAppContext) {
    let (window, sidebar, _guard) = open_sidebar(cx);
    cx.update_window(window, |_, window, cx| {
        click_file(window, 1, Modifiers::default(), cx);
        click_file(window, 3, shift(), cx);
        // The row above the first file is the group heading.
        press(window, 1, 1., Modifiers::default(), cx);
        assert!(sidebar.read(cx).picked_files(cx).0.is_empty());
    })
    .unwrap();
}
