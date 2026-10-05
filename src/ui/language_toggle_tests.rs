//! Regression test for the Windows language-toggle report. Two things were
//! wrong there: the press itself fell through to the OS (the title bar is an
//! HTCAPTION region on Windows, so an unconsumed mouse-down starts a window
//! move that eats the mouse-up — the click never fires), which these tests
//! pin down as "a mouse-down on the button must not bubble to an ancestor";
//! and the switch must re-render the button's label, which rides entirely on
//! `cx.refresh_windows()` since no entity notifies on a language change.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Mutex;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::test::TestWindowExt;
// No `gpui_kit::*` glob here: it imports gpui's `test` attribute macro, which
// shadows the built-in `#[test]` in the macro-generated code and recurses.
use gpui_kit::{
    AppContext, Bounds, Context, Entity, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Point, Render, Styled, TestAppContext, Window, WindowBounds, WindowOptions,
    div, px, size,
};

use crate::i18n::Language;
use crate::state::AppState;
use crate::ui::title_bar::TitleBarView;

/// Both tests drive the process-wide language setting, so they must not run
/// at the same time.
static LANGUAGE_LOCK: Mutex<()> = Mutex::new(());

fn language_guard() -> std::sync::MutexGuard<'static, ()> {
    LANGUAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Records whether a left mouse-down bubbled up past the title bar.
struct Harness {
    title_bar: Entity<TitleBarView>,
    bubbled_press: Rc<Cell<bool>>,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let bubbled = self.bubbled_press.clone();
        div()
            .size_full()
            .on_mouse_down(MouseButton::Left, move |_, _, _| bubbled.set(true))
            .child(self.title_bar.clone())
    }
}

#[gpui_kit::test]
fn clicking_toggle_language_switches_language_and_relabels_the_button(cx: &mut TestAppContext) {
    let _guard = language_guard();
    crate::i18n::set_current(Language::Zh);
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::ui::init(cx);
        Theme::change(ThemeMode::Light, None, cx);
    });
    let (window, _view) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Point::default(),
                        size: size(px(1024.), px(200.)),
                    })),
                    ..Default::default()
                },
                cx,
                |_, cx| {
                    let state = cx.new(AppState::new);
                    cx.new(|cx| TitleBarView::new(state, cx))
                },
            )
        })
        .expect("open test window");

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let before = window.find("toggle-language");
        assert_eq!(
            before.label(),
            Some("EN"),
            "zh UI offers the switch to English"
        );

        window.click("toggle-language", cx);
        assert_eq!(
            crate::i18n::current(),
            Language::En,
            "the click handler switches the global language"
        );

        window.render_frame(cx);
        let after = window.find("toggle-language");
        assert_eq!(
            after.label(),
            Some("中"),
            "the button re-renders with the new language's label"
        );
    })
    .unwrap();

    crate::i18n::set_current(Language::Zh);
}

#[gpui_kit::test]
fn pressing_toggle_language_does_not_bubble_to_ancestors(cx: &mut TestAppContext) {
    let _guard = language_guard();
    crate::i18n::set_current(Language::Zh);
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::ui::init(cx);
        Theme::change(ThemeMode::Light, None, cx);
    });
    let bubbled_press = Rc::new(Cell::new(false));
    let harness_press = bubbled_press.clone();
    let (window, _view) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Point::default(),
                        size: size(px(1024.), px(200.)),
                    })),
                    ..Default::default()
                },
                cx,
                |_, cx| {
                    let state = cx.new(AppState::new);
                    cx.new(|cx| Harness {
                        title_bar: cx.new(|cx| TitleBarView::new(state, cx)),
                        bubbled_press: harness_press.clone(),
                    })
                },
            )
        })
        .expect("open test window");

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-language", cx);
        assert!(
            !bubbled_press.get(),
            "the button consumes the press, so Windows never turns it into a window drag"
        );
    })
    .unwrap();

    crate::i18n::set_current(Language::Zh);
}

/// The title bar's dropdowns are kept out of the Windows caption hit-test;
/// doing that on the trigger button itself hid the popover's trigger area and
/// a click opened nothing.
#[gpui_kit::test]
fn clicking_open_opens_its_menu(cx: &mut TestAppContext) {
    clicking_opens_a_menu("open-data", cx);
}

#[gpui_kit::test]
fn clicking_ui_size_opens_its_menu(cx: &mut TestAppContext) {
    clicking_opens_a_menu("ui-size", cx);
}

fn clicking_opens_a_menu(trigger: &'static str, cx: &mut TestAppContext) {
    let _guard = language_guard();
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::ui::init(cx);
        Theme::change(ThemeMode::Light, None, cx);
    });
    let (window, _view) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Point::default(),
                        size: size(px(1024.), px(200.)),
                    })),
                    ..Default::default()
                },
                cx,
                |_, cx| {
                    let state = cx.new(AppState::new);
                    cx.new(|cx| TitleBarView::new(state, cx))
                },
            )
        })
        .expect("open test window");

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("popup-menu").is_none());
        window.click(trigger, cx);
        window.render_frame(cx);
        assert!(
            window.try_find("popup-menu").is_some(),
            "clicking {trigger} shows its menu"
        );
    })
    .unwrap();
}
