//! Running a panel once, in a window nobody sees, to capture what it asks.
//!
//! The panel is a script: it asks the database for what it wants while it
//! loads, so the only way to know what a panel shows is to run it. This runs
//! it the way the app does — same runtime, same host module, same catalog —
//! with a recorder in front of `query()`, and reports what came back.
//!
//! Two things about the shape of this file are forced rather than chosen:
//!
//! * **It ends the process.** A GPUI loop that has opened a window does not
//!   unwind when there is nothing to show, and quitting it can end the native
//!   process before `run` returns. gpui-kit's own `check` command reports and
//!   exits from inside the loop for the same reason, and this does the same.
//! * **The queries settle asynchronously.** `init()` is called at mount and
//!   spawns the work; the statements arrive through `smol::unblock` on another
//!   thread. There is no event to wait for, so the wait is a quiet period: the
//!   panel has stopped asking for this long, or the deadline has passed. A
//!   statement is recorded when it finishes, so "quiet" also requires nothing
//!   in flight — a slow query is not idleness.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use duckdb::Connection;
use gpui_kit::{
    div, px, size, App, AppContext as _, Bounds, Context, IntoElement, Render, Window,
    WindowBounds, WindowOptions,
};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context as LayerContext, Layer, SubscriberExt as _};

use crate::analysis::host;
use crate::dash::capture;

/// How often the settle loop looks for new statements.
const POLL: Duration = Duration::from_millis(60);

/// How long the panel must go without asking anything before it is done.
///
/// Longer than one query on a local file, and short enough that a panel that
/// has finished does not keep the export waiting: the cost of being wrong is a
/// statement missing from the report, which is why there is a deadline as well.
const QUIET: Duration = Duration::from_millis(400);

/// The longest a panel may take. A panel that polls the database forever must
/// not make `dash export` a command that never returns.
const DEADLINE: Duration = Duration::from_secs(15);

/// The slot the window-open closure fills with the mounted panel, so the code
/// that waits on it can see whether the mount worked.
type MountSlot = Rc<RefCell<Option<Result<gpui_kit::Entity<gpui_shell::ScriptView>, String>>>>;

/// The window's root: the panel, when it mounted, and nothing when it did not.
///
/// A window needs a root view, and the root has to be one type whether the
/// panel loaded or not — so this holds either.
struct PanelHost(Option<gpui_kit::Entity<gpui_shell::ScriptView>>);

impl Render for PanelHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        match &self.0 {
            Some(view) => view.clone().into_any_element(),
            None => div().into_any_element(),
        }
    }
}

pub struct Job {
    /// The panel's folder; `entry` is the file inside it.
    pub panel: PathBuf,
    /// The connection the panel's `query()` runs against, already opened with
    /// the access mode the caller asked for.
    pub connection: Connection,
}

/// What the run found.
pub enum Outcome {
    /// The panel ran. `panel_error` is why it stopped, when it stopped early;
    /// `reported` is what it logged as an error while running. `stop_reason`
    /// is "settled" when the panel went quiet, "deadline" when the time limit
    /// cut the capture short.
    Captured {
        captures: Vec<capture::Capture>,
        panel_error: Option<String>,
        reported: Vec<String>,
        elapsed_ms: u128,
        stop_reason: &'static str,
    },
    /// It never ran, so there is nothing to write.
    Failed(String),
}

/// Run `job`'s panel and hand the outcome to `finish`, which ends the process.
///
/// Returns only when the platform loop returns without having finished, which
/// is why the call after it is unreachable.
pub fn capture(job: Job, finish: impl FnOnce(Outcome) -> std::convert::Infallible + 'static) -> ! {
    /// Report and end the path here.
    ///
    /// `finish` cannot be typed as returning `!` without an unstable feature,
    /// so the divergence is spelled out where it is used: every arm that
    /// reports an outcome has nothing after it.
    macro_rules! stop {
        ($outcome:expr) => {{
            finish($outcome);
            unreachable!("the export finishes inside the application loop")
        }};
    }
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            let started = Instant::now();
            let reported = listen_for_errors();
            if let Err(error) = crate::db::install(job.connection) {
                stop!(Outcome::Failed(format!("{error:#}")));
            }
            // The same runtime the workspace builds, which installs the
            // component catalog and exports the `ducklocal` module. The module
            // has to be exported before the panel is loaded: an import is
            // resolved while the script's module graph is linked.
            let runtime = match crate::analysis::runtime::create(cx) {
                Ok(runtime) => runtime,
                Err(error) => stop!(Outcome::Failed(format!("{error:#}"))),
            };
            let loaded = match runtime.load_application(&job.panel, crate::analysis::host::ENTRY) {
                Ok(loaded) => loaded,
                Err(error) => stop!(Outcome::Failed(format!("{error:#}"))),
            };

            // Before the mount, not after: `init` is where a panel asks.
            capture::start();

            let mounted: MountSlot = Rc::new(RefCell::new(None));
            let slot = mounted.clone();
            let panel = job.panel.clone();
            let options = hidden_window(cx);
            let window = match cx.open_window(options, move |window, cx| {
                // The panel's folder answers `panelDir()` for this call and for
                // every host call `init` makes inside it.
                let view = host::with_panel_directory(&panel, || {
                    runtime.mount_application(&loaded, window, cx)
                });
                match view {
                    Ok(view) => {
                        *slot.borrow_mut() = Some(Ok(view.clone()));
                        cx.new(|_| PanelHost(Some(view)))
                    }
                    Err(error) => {
                        *slot.borrow_mut() = Some(Err(format!("{error:#}")));
                        cx.new(|_| PanelHost(None))
                    }
                }
            }) {
                Ok(window) => window,
                Err(error) => stop!(Outcome::Failed(format!("{error:#}"))),
            };
            // Opening the window drew one frame, so a panel that renders
            // before it asks has already been through `render`.
            let view = match mounted.borrow_mut().take() {
                Some(Ok(view)) => view,
                Some(Err(message)) => stop!(Outcome::Failed(message)),
                None => stop!(Outcome::Failed("The panel did not mount".to_string())),
            };

            cx.spawn(async move |cx| {
                // The window outlives this task: without a window there is
                // nothing to draw into, and a panel that renders its state
                // after a query would stop being rendered at all.
                let _window = window;
                let mut revision = capture::revision();
                let mut quiet_since = Instant::now();
                let stop_reason = loop {
                    smol::Timer::after(POLL).await;
                    let current = capture::revision();
                    if current != revision {
                        revision = current;
                        quiet_since = Instant::now();
                    }
                    // A statement is recorded when it finishes, so a slow
                    // query looks exactly like idleness on the revision
                    // counter alone; the in-flight count tells them apart.
                    let settled = revision > 0
                        && capture::in_flight() == 0
                        && quiet_since.elapsed() >= QUIET;
                    if settled {
                        break "settled";
                    }
                    if started.elapsed() >= DEADLINE {
                        break "deadline";
                    }
                };
                // A panel that threw said why in its own view, which is the
                // only place the reason exists.
                let panel_error = cx.update(|cx| view.read(cx).build_error().map(str::to_string));
                stop!(Outcome::Captured {
                    captures: capture::take(),
                    panel_error,
                    reported: reported
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone(),
                    elapsed_ms: started.elapsed().as_millis(),
                    stop_reason,
                });
            })
            .detach();
        });

    unreachable!("the export exits from inside the application loop")
}

/// Collect what the shell logs as an error while the panel runs.
///
/// A promise the panel leaves unhandled is logged and dropped: the shell has no
/// view to attach it to, so this is the only place that reason exists. In the
/// window it reaches the log; here it reaches the report, which is the same
/// thing for a file someone else will open.
///
/// A subscriber can only be installed once per process, so an existing one —
/// there is none in a CLI run, and one in a test that initialized tracing — is
/// left alone.
fn listen_for_errors() -> Arc<Mutex<Vec<String>>> {
    let reported: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let _ = tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(ErrorLog(reported.clone())),
    );
    reported
}

struct ErrorLog(Arc<Mutex<Vec<String>>>);

impl<S: Subscriber> Layer<S> for ErrorLog {
    fn on_event(&self, event: &Event<'_>, _: LayerContext<'_, S>) {
        if *event.metadata().level() != Level::ERROR {
            return;
        }
        let mut message = String::new();
        event.record(&mut Message(&mut message));
        if message.is_empty() {
            return;
        }
        // A report does not need the same complaint twice.
        let mut reported = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if !reported.contains(&message) {
            reported.push(message);
        }
    }
}

struct Message<'a>(&'a mut String);

impl Visit for Message<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0.push_str(&format!("{value:?}"));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        }
    }
}

/// A window is needed to render a panel, and nothing should appear on screen
/// while it is being captured: this runs from a script, an agent, or an editor.
///
/// It is laid out wide because a panel is written for a window: a panel that
/// decides what to query from the space it has would otherwise ask a different
/// question than it asks on screen.
fn hidden_window(cx: &mut App) -> WindowOptions {
    WindowOptions {
        show: false,
        focus: false,
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1280.), px(900.)),
            cx,
        ))),
        ..Default::default()
    }
}
