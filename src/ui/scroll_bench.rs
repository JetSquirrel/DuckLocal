//! Results-grid scroll benchmark: `ducklocal __scroll-bench`, built only with
//! `--features scroll-bench`.
//!
//! Opens a real window on a `ResultsPanel`, feeds it one wheel event per
//! display frame, and reads back GPUI's own frame trace: the time spent in
//! `Window::draw` (element build, layout, prepaint, paint — the CPU half of a
//! frame) and the interval between presented frames (what the user sees,
//! including Metal). A frame that misses the display's refresh shows up as a
//! long present interval.
//!
//! ```text
//! cargo run --release --features scroll-bench -- __scroll-bench
//! ```
//!
//! The window must stay on screen and unobscured while it runs: a hidden
//! window is not drawn, so its frames would not be counted.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::{Theme, ThemeMode, TitleBar};
use gpui_kit::profiler::{set_trace_enabled, FrameEvent, FrameTimingCollector};
use gpui_kit::*;

use crate::query::QueryOutcome;
use crate::ui::results::ResultsPanel;

const WARMUP_FRAMES: usize = 30;
const FRAMES: usize = 300;
/// Inside the grid for the panel at this window size: below the tab strip,
/// above the filter bar.
const POINTER: (f32, f32) = (720., 420.);

struct Scenario {
    name: &'static str,
    sql: String,
    /// Wheel delta per frame, in pixels; positive scrolls down/right.
    step: (f32, f32),
}

fn narrow() -> String {
    "SELECT i AS id, 'label_' || (i % 997) AS name, i * 1.5 AS amount,
            (i % 7 = 0) AS flag, DATE '2020-01-01' + (i % 900)::INT AS d,
            'city_' || (i % 31) AS city, i % 1000 AS bucket,
            'note text for row ' || i AS note
     FROM range(100000) t(i)"
        .to_string()
}

fn wide() -> String {
    let columns: Vec<String> = (0..30)
        .map(|c| match c % 3 {
            0 => format!("i * {c} AS n{c}"),
            1 => format!("'text_' || (i % {}) AS s{c}", 50 + c),
            _ => format!("(i * {c}) / 7.0 AS f{c}"),
        })
        .collect();
    format!("SELECT {} FROM range(60000) t(i)", columns.join(", "))
}

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario { name: "100k x 8, 40 px/frame", sql: narrow(), step: (0., 40.) },
        Scenario { name: "100k x 8, 800 px/frame", sql: narrow(), step: (0., 800.) },
        Scenario { name: "60k x 30, 40 px/frame", sql: wide(), step: (0., 40.) },
        Scenario { name: "60k x 30, 800 px/frame", sql: wide(), step: (0., 800.) },
        Scenario { name: "60k x 30, diagonal 40/40", sql: wide(), step: (40., 40.) },
    ]
}

struct Run {
    panel: Entity<ResultsPanel>,
    scenarios: Vec<Scenario>,
    ix: usize,
    frame: usize,
    collector: FrameTimingCollector,
    /// Draw time of the frame that first showed the scenario's result.
    first_draw: Option<Duration>,
    /// Time to handle each wheel event, which happens outside `draw`.
    dispatch: Vec<Duration>,
}

/// One scenario's frame trace, split by where the time went.
#[derive(Default)]
struct Trace {
    draw: Vec<Duration>,
    present: Vec<Duration>,
    interval: Vec<Duration>,
}

pub fn run() {
    crate::history::init().ok();
    crate::i18n::set_current(crate::i18n::initial_language());
    crate::ui::scale::load();

    let application = gpui_kit::application().with_assets(crate::assets::AppAssets);
    application.run(move |cx| {
        gpui_kit::init(cx);
        crate::ui::init(cx);
        Theme::change(ThemeMode::Light, None, cx);
        crate::ui::scale::apply(cx);
        set_trace_enabled(true);
        // Launched from a terminal, the app is not frontmost; a window that
        // is not on screen is not drawn and its frame loop never ticks.
        cx.activate(true);

        let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            focus: true,
            ..TitleBar::window_options()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            let panel = cx.new(|cx| ResultsPanel::new(window, cx));
            let state = Rc::new(RefCell::new(Run {
                panel: panel.clone(),
                scenarios: scenarios(),
                ix: 0,
                frame: 0,
                collector: FrameTimingCollector::new(),
                first_draw: None,
                dispatch: Vec::new(),
            }));
            show_scenario(&state, window, cx);
            window.on_next_frame(move |window, cx| step(state, window, cx));
            panel
        })
        .expect("open bench window");
    });
}

fn show_scenario(state: &Rc<RefCell<Run>>, window: &mut Window, cx: &mut App) {
    let s = state.borrow();
    let scenario = &s.scenarios[s.ix];
    let conn = duckdb::Connection::open_in_memory().expect("in-memory database");
    let outcome = crate::query::run_of(&conn, &scenario.sql);
    if let Ok(QueryOutcome::Rows(result)) = &outcome {
        eprintln!(
            "-- {}: {} rows x {} columns",
            scenario.name,
            result.rows.len(),
            result.columns.len()
        );
    }
    let sql = scenario.sql.clone();
    let panel = s.panel.clone();
    drop(s);
    panel.update(cx, |panel, cx| panel.set_outcome(outcome, sql, window, cx));
}

fn step(state: Rc<RefCell<Run>>, window: &mut Window, cx: &mut App) {
    let mut s = state.borrow_mut();
    if std::env::var_os("SCROLL_BENCH_TRACE").is_some() && s.frame % 50 == 0 {
        eprintln!("   scenario {} frame {}", s.ix, s.frame);
    }
    if s.frame == 1 {
        // Frame 0 was the one that laid the new result out.
        s.first_draw = trace(s.collector.collect_unseen()).draw.first().copied();
    }
    if s.frame == WARMUP_FRAMES {
        s.collector = FrameTimingCollector::new();
        s.dispatch.clear();
    }
    if s.frame == WARMUP_FRAMES + FRAMES {
        let trace = trace(s.collector.collect_unseen());
        let dispatch = std::mem::take(&mut s.dispatch);
        report(s.scenarios[s.ix].name, s.first_draw, trace, dispatch);
        s.ix += 1;
        s.frame = 0;
        if s.ix == s.scenarios.len() {
            cx.quit();
            return;
        }
        drop(s);
        show_scenario(&state, window, cx);
        s = state.borrow_mut();
        s.collector = FrameTimingCollector::new();
    } else {
        let (dx, dy) = s.scenarios[s.ix].step;
        // GPUI's wheel delta is negative for content moving up, as a
        // trackpad reports it.
        let started = Instant::now();
        window.dispatch_event(
            PlatformInput::ScrollWheel(ScrollWheelEvent {
                position: point(px(POINTER.0), px(POINTER.1)),
                delta: ScrollDelta::Pixels(point(px(-dx), px(-dy))),
                ..Default::default()
            }),
            cx,
        );
        s.dispatch.push(started.elapsed());
    }
    s.frame += 1;
    drop(s);
    window.refresh();
    window.on_next_frame(move |window, cx| step(state, window, cx));
}

fn trace(events: Vec<FrameEvent>) -> Trace {
    let mut trace = Trace::default();
    let mut presented: Vec<Instant> = Vec::new();
    for event in events {
        match event {
            FrameEvent::Draw(timing) => trace.draw.push(timing.draw_duration()),
            FrameEvent::Present(timing) => {
                trace.present.push(timing.present_duration());
                presented.push(timing.present_end);
            }
        }
    }
    trace.interval = presented.windows(2).map(|w| w[1] - w[0]).collect();
    trace
}

fn report(name: &str, first: Option<Duration>, trace: Trace, dispatch: Vec<Duration>) {
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let stats = |mut v: Vec<Duration>| -> String {
        if v.is_empty() {
            return "no samples".to_string();
        }
        v.sort();
        let pct = |p: f64| ms(v[((v.len() - 1) as f64 * p).round() as usize]);
        let mean = ms(v.iter().sum::<Duration>() / v.len() as u32);
        format!(
            "mean {mean:>6.2}  p50 {:>6.2}  p95 {:>6.2}  max {:>7.2}",
            pct(0.5),
            pct(0.95),
            ms(*v.last().unwrap())
        )
    };
    let late = trace.interval.iter().filter(|d| ms(**d) > 12.5).count();
    let frames = trace.interval.len();
    println!("{name}");
    println!(
        "  first frame draw   {}",
        first.map_or("-".to_string(), |d| format!("{:.2} ms", ms(d)))
    );
    println!("  wheel dispatch     {}", stats(dispatch));
    println!("  draw (CPU)         {}", stats(trace.draw));
    println!("  present (submit)   {}", stats(trace.present));
    println!(
        "  frame interval     {}   late {late}/{frames}",
        stats(trace.interval)
    );
}
