//! The Metrics tab: a model row's button opens the model there, ticked
//! metrics and fields compile to one query, the query opens in a tab, and a
//! condition the model cannot answer is refused before anything runs.

use gpui_kit::component::input::InputEvent;
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::test::TestWindowExt;
// gpui's platform-event trait, for `to_platform_input`.
use gpui_kit::InputEvent as _;
// No `gpui_kit::*` glob here: it imports gpui's `test` attribute macro, which
// shadows the built-in `#[test]` in the macro-generated code and recurses.
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext, Bounds, ElementId, Entity, Modifiers, MouseButton,
    MouseDownEvent, MouseUpEvent, Point, TestAppContext, WindowBounds, WindowOptions,
};

use crate::i18n::Language;
use crate::recents::{RecentDocument, RecentKind};
use crate::semantic::session::Session;
use crate::state::AppState;
use crate::ui::sidebar::Sidebar;
use crate::ui::workspace::Workspace;

fn example() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/cloudtrail_investigation")
}

/// Press and release the left button over the element `id`, then let what
/// it started finish.
fn click(window: &AnyWindowHandle, cx: &mut TestAppContext, id: impl Into<ElementId>) {
    let id = id.into();
    window
        .update(cx, |_, window, cx| {
            window.render_frame(cx);
            let position = window.find(id).bounds().center();
            window.dispatch_event(
                MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Modifiers::default(),
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
                    modifiers: Modifiers::default(),
                    click_count: 1,
                }
                .to_platform_input(),
                cx,
            );
        })
        .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn ticked_metrics_and_fields_open_as_one_query(cx: &mut TestAppContext) {
    let _guard = crate::ui::language_toggle_tests::language_guard();
    crate::i18n::set_current(Language::Zh);
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::ui::init(cx);
        Theme::change(ThemeMode::Light, None, cx);
    });
    let mut parts: Option<(Entity<Sidebar>, Entity<Workspace>)> = None;
    let (window, _) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Point::default(),
                        size: size(px(320.), px(1400.)),
                    })),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    let state = cx.new(AppState::new);
                    let workspace = cx.new(|cx| Workspace::new(state.clone(), window, cx));
                    let sidebar =
                        cx.new(|cx| Sidebar::new(state.clone(), workspace.clone(), window, cx));
                    parts = Some((sidebar.clone(), workspace));
                    sidebar
                },
            )
        })
        .expect("open test window");
    let (sidebar, workspace) = parts.unwrap();
    cx.update(|cx| {
        sidebar.update(cx, |sidebar, cx| {
            sidebar.set_recents(
                vec![RecentDocument {
                    path: example()
                        .join("reports/investigation.dash")
                        .to_string_lossy()
                        .into_owned(),
                    kind: RecentKind::Dashboard,
                    title: "investigation".into(),
                }],
                cx,
            )
        })
    });
    cx.run_until_parked();

    // The model row's button opens the model in the Metrics tab.
    let model_row = cx
        .update(|cx| {
            let sidebar = sidebar.read(cx);
            (0..64)
                .map_while(|ix| sidebar.tree_label(ix, cx))
                .position(|label| label == "cloudtrail")
        })
        .expect("the model's row");
    click(&window, cx, ("model-build", model_row));

    // The boxes are numbered as the model lists them: metrics from 0,
    // fields from 1 across every dataset.
    let (_, vocabulary) = Session::open(&example().join("model.ossie.yaml")).unwrap();
    let metric = |name: &str| {
        vocabulary
            .metrics
            .iter()
            .position(|m| m.name == name)
            .unwrap()
    };
    let field = |reference: &str| {
        1 + vocabulary
            .datasets
            .iter()
            .flat_map(|d| d.fields.iter())
            .position(|f| f.reference == reference)
            .unwrap()
    };
    assert_eq!(cx.update(|cx| sidebar.read(cx).metrics_outcome()), None);
    click(&window, cx, ("metric", metric("error_count")));
    click(&window, cx, ("metric", metric("event_count")));
    click(&window, cx, ("dimension", field("events.event_time")));

    let sql = match cx.update(|cx| sidebar.read(cx).metrics_outcome()) {
        Some(Ok(sql)) => sql,
        other => panic!("the ticked request did not compile: {other:?}"),
    };
    assert!(
        sql.starts_with(
            "-- Metrics error_count, event_count by events.event_time:day, from model cloudtrail"
        ),
        "{sql}"
    );
    assert!(
        sql.contains("\"events\" AS ("),
        "the dashboard's view comes along: {sql}"
    );

    click(&window, cx, "metrics-open");
    let opened = cx
        .update(|cx| workspace.read(cx).active_editor_text(cx))
        .expect("a query tab");
    assert_eq!(opened, sql);

    // A condition over a field the model does not have is refused while it
    // is typed, not when the query runs.
    let filter = cx.update(|cx| sidebar.read(cx).metrics_filter());
    window
        .update(cx, |_, window, cx| {
            filter.update(cx, |input, cx| {
                input.set_value("events.no_such_field = 1", window, cx);
                cx.emit(InputEvent::Change);
            });
        })
        .unwrap();
    cx.run_until_parked();
    assert!(
        matches!(
            cx.update(|cx| sidebar.read(cx).metrics_outcome()),
            Some(Err(kind)) if kind.starts_with("metric_")
        ),
        "{:?}",
        cx.update(|cx| sidebar.read(cx).metrics_outcome())
    );
}
