//! The sidebar's metrics: a semantic model beside a remembered dashboard
//! shows as a heading, the model and its metrics, and a metric's run button
//! puts the compiled SQL in the editor.

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::test::TestWindowExt;
// No `gpui_kit::*` glob here: it imports gpui's `test` attribute macro, which
// shadows the built-in `#[test]` in the macro-generated code and recurses.
use gpui_kit::{
    px, size, AppContext, Bounds, Entity, InputEvent, Modifiers, MouseButton, MouseDownEvent,
    MouseUpEvent, Point, TestAppContext, WindowBounds, WindowOptions,
};

use crate::i18n::Language;
use crate::recents::{RecentDocument, RecentKind};
use crate::state::AppState;
use crate::ui::sidebar::Sidebar;
use crate::ui::workspace::Workspace;

fn example_dashboard() -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples/cloudtrail_investigation/reports/investigation.dash")
        .to_string_lossy()
        .into_owned()
}

#[gpui_kit::test]
fn a_model_beside_a_dashboard_lists_its_metrics_and_runs_one(cx: &mut TestAppContext) {
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
                        size: size(px(320.), px(900.)),
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
                    path: example_dashboard(),
                    kind: RecentKind::Dashboard,
                    title: "investigation".into(),
                }],
                cx,
            )
        })
    });
    cx.run_until_parked();

    let labels: Vec<String> = cx.update(|cx| {
        let sidebar = sidebar.read(cx);
        (0..64)
            .map_while(|ix| sidebar.tree_label(ix, cx))
            .map(|label| label.to_string())
            .collect()
    });
    let at = |name: &str| labels.iter().position(|label| label == name);
    let heading = at("指标").unwrap_or_else(|| panic!("no 指标 heading in {labels:?}"));
    let model = at("cloudtrail").expect("the model's row");
    let metric = at("error_count").expect("a metric's row");
    assert!(heading < model && model < metric, "{labels:?}");
    for name in ["event_count", "error_rate", "access_changes", "last_seen"] {
        assert!(at(name).is_some(), "{name} missing from {labels:?}");
    }

    let row = metric;
    window
        .update(cx, |_, window, cx| {
            window.render_frame(cx);
            let button = window.find(("metric-run", row)).bounds();
            let position = button.center();
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

    let sql = cx
        .update(|cx| workspace.read(cx).active_editor_text(cx))
        .expect("a query tab");
    assert!(
        sql.starts_with("-- Metric error_count, from model cloudtrail"),
        "{sql}"
    );
    assert!(
        sql.contains("\"events\" AS ("),
        "the dashboard's view comes along: {sql}"
    );
    assert!(sql.contains("count(events.error_code)"), "{sql}");
}
