//! The view half of a `.dash` spec: the file as a workspace tab.
//!
//! `mod.rs` checks a spec without a window; this module draws one. A
//! `Dashboard` parses and validates its file, runs every query on the window's
//! connection, and renders each plot as one panel of a vertical resizable
//! stack — the plot heights are the user's to drag, and a stack taller than
//! the tab scrolls. The data a plot draws is
//! `prepare`'s business; the view only renders what it prepares. A plot whose
//! query failed, or whose columns do not resolve, shows the reason in its own
//! panel: one bad plot never takes the dashboard down. And a reload follows
//! the app rule: a spec that no longer validates never replaces a working
//! dashboard — the previous one stays up and the reason appears above it.
//!
//! One chart note: only `AreaChart` in the catalog takes more than one series,
//! so a multi-series `line` reads as a faintly filled area and a multi-series
//! `scatter` as unmarked lines, while a multi-series `bar` is one small chart
//! per series, stacked and scrolled.
//!
//! The toolbar's source toggle swaps the whole body for the spec's text in an
//! editor — not a read-only one: the source is where a dashboard is fixed.
//! Edits are written back with the save button or ⌘S, and a save reloads; a
//! save whose spec no longer validates keeps the last working dashboard up
//! and says why above it. While the buffer holds unsaved edits the tab shows
//! a dirty dot, and an external change to the file is a conflict banner
//! rather than a silent overwrite; with a clean buffer it just reloads.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::component::alert::Alert;
use gpui_kit::component::chart::{AreaChart, BarChart, LineChart};
use gpui_kit::component::input::{
    CompletionProvider, Editor, EditorState, Rope, RopeExt, TabSize,
};
use gpui_kit::component::label::Label;
use gpui_kit::component::resizable::{resizable_panel, v_resizable};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    TextEdit,
};

use crate::analysis::watch::{Debounce, FileStamp, POLL_INTERVAL};
use crate::i18n::{tr, trf};
use crate::query::{ColumnKind, QueryOutcome, QueryResult};
use crate::spec::complete::{self, CompletionKind};
use crate::spec::model::{self, Spec};
use crate::spec::prepare::{prepare, PlotPoint, PreparedPlot};
use crate::ui::chart::format_value;
use crate::ui::completion::starts_with_ignore_case;

/// The plot panels' default and drag bounds, in the spirit of the workspace's
/// own results split.
const PLOT_DEFAULT: f32 = 300.;
const PLOT_MIN: f32 = 160.;
const PLOT_MAX: f32 = 800.;

/// A `.dash` file open as a tab: its plots, and the run that produced them.
pub struct Dashboard {
    /// The tab's id, baked into the resizable group's element id so two
    /// dashboards never share panel sizes.
    id: u64,
    path: PathBuf,
    /// Why nothing can be drawn: the spec itself did not read, parse or
    /// validate, and no earlier spec is still drawable. Per-plot failures live
    /// on the plot instead.
    spec_error: Option<String>,
    /// Why what is shown is out of date, while an earlier run still draws.
    stale_reason: Option<String>,
    plots: Vec<PreparedPlot>,
    /// The successful query results by query name, for the `table` plots that
    /// render the grid directly.
    results: HashMap<String, Arc<QueryResult>>,
    /// Table entities, one per table plot. They need a window to create, so
    /// they are built lazily at render and dropped on every landed run.
    tables: Vec<Option<Entity<TableState<SpecTableDelegate>>>>,
    running: bool,
    /// Whether the body shows the spec's source instead of the plots.
    showing_source: bool,
    /// The spec text as last read from (or written to) disk, or why it could
    /// not be read: the baseline the editor's dirty state is measured against.
    source: Option<Result<String, String>>,
    /// The editor the source view draws in, built lazily like the tables;
    /// `source_version`/`editor_version` pair so a re-read pushes to the
    /// editor once, and no render resets its scroll or cursor.
    source_editor: Option<Entity<EditorState>>,
    source_version: u64,
    editor_version: u64,
    /// The file's stamp as last written by us or reloaded by the watcher, so
    /// our own save does not come back as an "external" change.
    known_stamp: FileStamp,
    /// The file changed on disk while the buffer held unsaved edits. Shown as
    /// a banner; the buffer is never overwritten to resolve it — saving is
    /// the resolution.
    conflict: bool,
    /// Polls the spec file for external changes. Dropping it ends the
    /// watcher, which is how closing the tab stops watching.
    watcher: Option<Task<()>>,
}

impl Dashboard {
    pub fn new(id: u64, path: PathBuf, cx: &mut Context<Self>) -> Self {
        let known_stamp = FileStamp::capture(&path);
        let mut this = Self {
            id,
            path,
            spec_error: None,
            stale_reason: None,
            plots: Vec::new(),
            results: HashMap::new(),
            tables: Vec::new(),
            running: false,
            showing_source: false,
            source: None,
            source_editor: None,
            source_version: 0,
            editor_version: 0,
            known_stamp,
            conflict: false,
            watcher: None,
        };
        this.watch(cx);
        this.reload(cx);
        this
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn is_showing_source(&self) -> bool {
        self.showing_source
    }

    /// The source editor, once the source view has been opened — for the
    /// workspace to focus when its tab comes forward.
    pub fn source_editor(&self) -> Option<Entity<EditorState>> {
        self.source_editor.clone()
    }

    /// The buffer holds edits the file does not: the editor differs from the
    /// text as last read from (or written to) disk.
    pub fn is_dirty(&self, cx: &App) -> bool {
        match (&self.source_editor, &self.source) {
            (Some(editor), Some(Ok(text))) => editor.read(cx).value().as_str() != text.as_str(),
            _ => false,
        }
    }

    /// Swap the body between the plots and the spec's source. Opening the
    /// source re-reads the file — unless the buffer holds unsaved edits,
    /// which a view switch must never throw away.
    pub fn toggle_source(&mut self, cx: &mut Context<Self>) {
        self.showing_source = !self.showing_source;
        if self.showing_source && !self.is_dirty(cx) {
            self.read_source();
        }
        cx.notify();
    }

    /// Write the editor's text back to the spec file and reload. A save whose
    /// spec no longer validates keeps the last working dashboard up, per the
    /// reload rule. Returns why the write itself failed, for a notification.
    pub fn save(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let (Some(editor), Some(Ok(_))) = (&self.source_editor, &self.source) else {
            return Ok(());
        };
        let text = editor.read(cx).value().to_string();
        std::fs::write(&self.path, &text).map_err(|error| {
            trf(
                "dashboard.save_failed",
                &[&self.path.to_string_lossy(), &error.to_string()],
            )
        })?;
        self.source_version += 1;
        self.source = Some(Ok(text));
        // The editor already holds what was written: mark the versions in
        // sync rather than pushing the text back and resetting the cursor.
        self.editor_version = self.source_version;
        self.known_stamp = FileStamp::capture(&self.path);
        self.conflict = false;
        self.reload(cx);
        Ok(())
    }

    /// Poll the spec file and reload when an external change settles. A dirty
    /// buffer is never overwritten: the change is a conflict banner instead,
    /// and our own saves are filtered out by their stamp.
    fn watch(&mut self, cx: &mut Context<Self>) {
        let watched = self.path.clone();
        let watcher = cx.spawn(async move |this, cx| {
            let mut stamp = FileStamp::capture(&watched);
            let mut debounce = Debounce::new();
            loop {
                smol::Timer::after(POLL_INTERVAL).await;
                let next = FileStamp::capture(&watched);
                let changed = next != stamp;
                stamp = next.clone();
                if !debounce.observe(Instant::now(), changed) {
                    continue;
                }
                if this
                    .update(cx, |this, cx| this.external_changed(next, cx))
                    .is_err()
                {
                    // The tab is gone, so there is nothing to reload into.
                    break;
                }
            }
        });
        self.watcher = Some(watcher);
    }

    /// The file moved on disk. A clean buffer just follows it; a dirty one is
    /// told about the conflict and keeps its edits.
    fn external_changed(&mut self, stamp: FileStamp, cx: &mut Context<Self>) {
        if stamp == self.known_stamp {
            // Our own save, coming back around the poll.
            return;
        }
        self.known_stamp = stamp;
        if self.is_dirty(cx) {
            self.conflict = true;
            cx.notify();
            return;
        }
        self.conflict = false;
        self.read_source();
        self.reload(cx);
    }

    /// Read the spec file for the source view; a failure is kept as the
    /// message the view shows instead of the source.
    fn read_source(&mut self) {
        self.source_version += 1;
        self.source = Some(std::fs::read_to_string(&self.path).map_err(|error| {
            trf(
                "dashboard.source.unreadable",
                &[&self.path.to_string_lossy(), &error.to_string()],
            )
        }));
    }

    /// Re-read the spec and re-run every query on the window's connection: the
    /// toolbar's reload, and what a change of database asks for. A run in
    /// flight wins; a second request while it runs would only redo it.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        if self.showing_source && !self.is_dirty(cx) {
            // The source view shows the file as it is now, not as it was when
            // toggled open — but a dirty buffer is the user's work, and a
            // reload is no reason to lose it.
            self.read_source();
        }
        self.running = true;
        cx.notify();
        let path = self.path.clone();
        cx.spawn(async move |this, cx| {
            let run = smol::unblock(move || load(&path)).await;
            this.update(cx, |this, cx| {
                this.running = false;
                match run.spec_error {
                    // A reload that failed never replaces a working dashboard:
                    // the previous one stays up and the reason appears above
                    // it — the difference between a typo while editing and
                    // losing the dashboard to it.
                    Some(error) if !this.plots.is_empty() => {
                        this.stale_reason = Some(error);
                    }
                    spec_error => {
                        this.spec_error = spec_error;
                        this.stale_reason = None;
                        this.plots = run.plots;
                        this.results = run.results;
                        this.tables = vec![None; this.plots.len()];
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_plot(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, notice, failure, is_table, is_empty) = {
            let plot = &self.plots[ix];
            (
                plot.title.clone(),
                plot.notice.clone(),
                plot.failure.clone(),
                plot.kind == "table",
                plot.points.is_empty(),
            )
        };
        // The axis subtitle the results chart shows: which series, over which
        // x column. Tables and failures have nothing to say.
        let axes = (!is_table && failure.is_none() && !is_empty).then(|| {
            let plot = &self.plots[ix];
            trf(
                "chart.title.by",
                &[&plot.series_names.join(", "), &plot.label_name],
            )
        });

        let body = if let Some(message) = failure {
            div()
                .size_full()
                .p_3()
                .child(
                    Alert::error(("dashboard-plot-error", ix), message)
                        .title(tr("dashboard.plot_failed")),
                )
                .into_any_element()
        } else if is_table {
            self.render_table(ix, window, cx)
        } else if is_empty {
            empty_chart(cx)
        } else {
            div()
                .size_full()
                .px_2()
                .pb_2()
                .child(chart_element(ix, &self.plots[ix], cx))
                .into_any_element()
        };

        v_flex()
            .size_full()
            .child(
                v_flex()
                    .w_full()
                    .flex_none()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_baseline()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .when_some(notice, |this, notice| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(notice),
                                )
                            }),
                    )
                    .when_some(axes, |this, axes| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(axes),
                        )
                    }),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    /// A `table` plot draws its query's grid; the query is guaranteed to have
    /// succeeded, because `prepare` would have made the failure otherwise.
    fn render_table(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let query = self.plots[ix].query.clone();
        let Some(result) = self.results.get(&query).cloned() else {
            return empty_chart(cx);
        };
        if self.tables[ix].is_none() {
            let delegate = SpecTableDelegate::new(result.clone());
            self.tables[ix] = Some(cx.new(|cx| TableState::new(delegate, window, cx)));
        }
        let table = self.tables[ix].clone().expect("built just above");
        let truncated = result.truncated;
        let row_count = result.rows.len();
        v_flex()
            .size_full()
            .when(truncated, |this| {
                this.child(
                    Alert::warning(
                        ("dashboard-truncated", ix),
                        trf("results.truncated", &[&row_count.to_string()]),
                    )
                    .banner()
                    .small(),
                )
            })
            .child(
                div().flex_1().min_h_0().child(
                    DataTable::new(&table)
                        .small()
                        .stripe(true)
                        .scrollbar_visible(true, true),
                ),
            )
            .into_any_element()
    }

    /// The spec itself, in the same editor the query tabs use — editable,
    /// with completion, and never reset under the user's hands: a re-read
    /// pushes to it only when the text actually differs. A file that cannot
    /// be read shows its error here rather than replacing the tab.
    fn render_source(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let body = match &self.source {
            Some(Ok(_)) => {
                if self.source_editor.is_none() {
                    self.source_editor = Some(cx.new(|cx| {
                        let mut state = EditorState::new(window, cx)
                            .language(crate::spec::highlight::LANGUAGE)
                            .line_number(true)
                            .tab_size(TabSize {
                                tab_size: 2,
                                hard_tabs: false,
                            });
                        state.lsp_mut().completion_provider =
                            Some(Rc::new(SpecCompletionProvider));
                        // Installed before the editor first renders, so the
                        // component's tree-sitter factory — which has no
                        // `.dash` grammar — never takes the slot.
                        state.set_highlighter_factory(crate::spec::highlight::factory(), cx);
                        state
                    }));
                }
                let editor = self.source_editor.clone().expect("built just above");
                if self.editor_version != self.source_version {
                    let text = match &self.source {
                        Some(Ok(text)) => text.clone(),
                        _ => unreachable!("matched above"),
                    };
                    // Pushing back what the editor already shows would only
                    // reset its cursor — after a save, that is exactly the
                    // text on disk.
                    if editor.read(cx).value().as_str() != text.as_str() {
                        editor.update(cx, |state, cx| state.set_value(text, window, cx));
                    }
                    self.editor_version = self.source_version;
                }
                div()
                    .size_full()
                    .child(Editor::new(&editor).h(relative(1.)).bordered(false))
                    .into_any_element()
            }
            Some(Err(message)) => div()
                .size_full()
                .p_3()
                .child(
                    Alert::error("dashboard-source-error", message.clone())
                        .title(tr("dashboard.spec_failed")),
                )
                .into_any_element(),
            // toggle_source reads before flipping, so this is one frame at most.
            None => self.render_loading(cx),
        };
        v_flex().size_full().child(body).into_any_element()
    }

    /// The spec would not read, parse or validate: the whole tab is the error,
    /// the way an app that has never loaded shows its failure.
    fn render_spec_failure(&self, error: String, cx: &App) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .p_6()
            .child(
                Icon::new(IconName::CircleX)
                    .large()
                    .text_color(cx.theme().danger),
            )
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(tr("dashboard.spec_failed")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .max_w_96()
                    .text_center()
                    .child(error),
            )
            .into_any_element()
    }

    fn render_empty(&self, cx: &App) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .p_6()
            .child(
                Icon::new(IconName::LayoutDashboard)
                    .large()
                    // Decoration, not data: deliberately faded.
                    .text_color(cx.theme().muted_foreground.alpha(0.5)),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("dashboard.empty")),
            )
            .into_any_element()
    }

    fn render_loading(&self, cx: &App) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(Spinner::new().large().color(cx.theme().muted_foreground))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("results.running")),
            )
            .into_any_element()
    }

    /// The file changed on disk while the buffer holds unsaved edits: said
    /// out loud, above whichever view is up, until a save resolves it.
    fn render_conflict_warning(&self, cx: &App) -> Option<impl IntoElement> {
        if !self.conflict {
            return None;
        }
        Some(
            v_flex()
                .flex_none()
                .gap_1()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().warning.alpha(0.12))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(tr("dashboard.conflict")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr("dashboard.conflict.hint")),
                ),
        )
    }

    fn render_stale_warning(&self, cx: &App) -> Option<impl IntoElement> {
        let reason = self.stale_reason.clone()?;
        Some(
            v_flex()
                .flex_none()
                .gap_1()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().danger.alpha(0.12))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(tr("analysis.not_updated")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(reason),
                ),
        )
    }
}

impl Render for Dashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = if self.showing_source {
            // The source view stands on its own, spec error or not: the text
            // of a broken spec is exactly what one opens the source to fix.
            self.render_source(window, cx)
        } else if let Some(error) = self.spec_error.clone() {
            self.render_spec_failure(error, cx)
        } else if self.plots.is_empty() {
            // The first run has not landed yet: loading, not empty. A spec
            // that ran and has no plots is the empty state.
            if self.running {
                self.render_loading(cx)
            } else {
                self.render_empty(cx)
            }
        } else {
            let panels = (0..self.plots.len())
                .map(|ix| {
                    resizable_panel()
                        .size(px(PLOT_DEFAULT))
                        .size_range(px(PLOT_MIN)..px(PLOT_MAX))
                        .child(self.render_plot(ix, window, cx))
                })
                .collect::<Vec<_>>();
            // Every plot keeps its default height and the tab scrolls, rather
            // than a dozen plots squeezed into one screen and the rest clipped
            // out of reach. Dragging a divider still trades height between
            // neighbours; a tab taller than the stack is filled, as before.
            let stack = px(PLOT_DEFAULT * self.plots.len() as f32);
            div()
                .id(format!("dashboard-scroll-{}", self.id))
                .size_full()
                .overflow_y_scrollbar()
                .child(
                    div()
                        .w_full()
                        .h(stack)
                        .min_h_full()
                        .child(v_resizable(format!("dashboard-{}", self.id)).children(panels)),
                )
                .into_any_element()
        };

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .children(self.render_conflict_warning(cx))
            .children(self.render_stale_warning(cx))
            .child(body)
    }
}

/// Everything one run of a dashboard produces, computed off the UI thread.
struct Run {
    spec_error: Option<String>,
    plots: Vec<PreparedPlot>,
    results: HashMap<String, Arc<QueryResult>>,
}

/// Read, parse, validate, run, prepare. Blocking — call it off the UI thread.
fn load(path: &Path) -> Run {
    let display = path.display().to_string();
    let spec = (|| -> Result<Spec, String> {
        let source = std::fs::read_to_string(path).map_err(|e| format!("{display}: {e}"))?;
        let file = model::parse(&source)
            .map_err(|e| format!("{display}:{}: {}", e.line, e.message))?;
        model::validate(&file).map_err(|diagnostics| {
            diagnostics
                .iter()
                .map(|d| format!("{display}:{}: {}", d.line, d.message))
                .collect::<Vec<_>>()
                .join("\n")
        })
    })();
    let spec = match spec {
        Ok(spec) => spec,
        Err(spec_error) => {
            return Run {
                spec_error: Some(spec_error),
                plots: Vec::new(),
                results: HashMap::new(),
            };
        }
    };

    // Every query runs once, however many plots draw from it, and only after
    // it has been shown to read and nothing else: opening a file someone sent
    // must not be what runs its `DROP` or `COPY … TO`.
    let outcomes: Vec<Result<QueryResult, String>> = spec
        .queries
        .iter()
        .map(|query| match super::validate_query_sql(&query.sql)
            .and_then(|()| crate::query::run(&query.sql).map_err(|e| format!("{e:#}")))
        {
            Ok(QueryOutcome::Rows(result)) => Ok(result),
            Ok(QueryOutcome::Affected { .. }) => {
                Err(trf("dashboard.query_not_rows", &[&query.name]))
            }
            Err(e) => Err(e),
        })
        .collect();

    // validate() has already said every plot's query exists.
    let plots = spec
        .plots
        .iter()
        .map(|plot| {
            let result = spec
                .queries
                .iter()
                .position(|q| q.name == plot.query)
                .map(|ix| &outcomes[ix]);
            prepare(plot, result)
        })
        .collect();

    let results = spec
        .queries
        .iter()
        .zip(outcomes)
        .filter_map(|(query, outcome)| {
            outcome
                .ok()
                .map(|result| (query.name.clone(), Arc::new(result)))
        })
        .collect();

    Run {
        spec_error: None,
        plots,
        results,
    }
}

/// One plot's chart element, built from the prepared data on each frame;
/// cloning the `Arc`s is a refcount bump, not a copy of the row set.
fn chart_element(plot_ix: usize, plot: &PreparedPlot, cx: &App) -> AnyElement {
    let palette = [
        cx.theme().chart_1,
        cx.theme().chart_2,
        cx.theme().chart_3,
        cx.theme().chart_4,
        cx.theme().chart_5,
    ];
    let id = ("dashboard-chart", plot_ix);
    let tick_margin = (plot.points.len() / 10).max(1);
    let single = plot.series_names.len() <= 1;
    let name = plot
        .series_names
        .first()
        .cloned()
        .unwrap_or_else(|| plot.title.clone());

    match plot.kind.as_str() {
        "bar" if single => BarChart::new(plot.points.clone())
            .band(|d: &Arc<PlotPoint>| d.band.clone())
            .value(|d: &Arc<PlotPoint>| d.values[0])
            .fill(move |d: &Arc<PlotPoint>, _, _, _| palette[d.ix % palette.len()])
            .label(|d: &Arc<PlotPoint>| d.label.clone())
            .id(id)
            .name(name)
            .into_any_element(),
        // A band scale cannot group bars side by side, so each series draws
        // its own chart, one under the next.
        "bar" => {
            let mut stack = v_flex()
                .id(format!("dashboard-bars-{}", plot.name))
                .size_full()
                .overflow_y_scroll()
                .gap_4()
                .p_2();
            for (s, series_name) in plot.series_names.iter().enumerate() {
                let color = palette[s % palette.len()];
                let data: Vec<Arc<PlotPoint>> = plot
                    .points
                    .iter()
                    .filter(|d| d.present[s])
                    .cloned()
                    .collect();
                stack = stack.child(
                    v_flex()
                        .flex_none()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(series_name.clone()),
                        )
                        .child(
                            div().h(px(240.)).flex_none().child(
                                BarChart::new(data)
                                    .band(|d: &Arc<PlotPoint>| d.band.clone())
                                    .value(move |d: &Arc<PlotPoint>| d.values[s])
                                    .fill(move |_: &Arc<PlotPoint>, _, _, _| color)
                                    .label(move |d: &Arc<PlotPoint>| {
                                        SharedString::from(format_value(d.values[s]))
                                    })
                                    .id(format!("dashboard-chart-{}-{s}", plot.name))
                                    .name(series_name.clone()),
                            ),
                        ),
                );
            }
            stack.into_any_element()
        }
        "area" if single => {
            let color = palette[0];
            AreaChart::new(plot.points.clone())
                .x(|d: &Arc<PlotPoint>| d.band.clone())
                .id(id)
                .y(|d: &Arc<PlotPoint>| d.values[0])
                .stroke(color)
                .fill(linear_gradient(
                    0.,
                    linear_color_stop(color.opacity(0.4), 1.),
                    linear_color_stop(color.opacity(0.05), 0.),
                ))
                .name(name)
                .tick_margin(tick_margin)
                .into_any_element()
        }
        "scatter" if single => LineChart::new(plot.points.clone())
            .x(|d: &Arc<PlotPoint>| d.band.clone())
            .y(|d: &Arc<PlotPoint>| d.values[0])
            .dot()
            .stroke(palette[0])
            .name(name)
            .id(id)
            .tick_margin(tick_margin)
            .into_any_element(),
        _ if single => LineChart::new(plot.points.clone())
            .x(|d: &Arc<PlotPoint>| d.band.clone())
            .y(|d: &Arc<PlotPoint>| d.values[0])
            .stroke(palette[0])
            .name(name)
            .id(id)
            .tick_margin(tick_margin)
            .into_any_element(),
        // Pivoted line/area/scatter all draw on the catalog's one multi-series
        // chart; how much fill distinguishes them.
        _ => {
            let mut chart = AreaChart::new(plot.points.clone())
                .x(|d: &Arc<PlotPoint>| d.band.clone())
                .id(id)
                .tick_margin(tick_margin);
            for (s, series_name) in plot.series_names.iter().enumerate() {
                let color = palette[s % palette.len()];
                let (top, bottom) = match plot.kind.as_str() {
                    "area" => (0.4, 0.05),
                    "line" => (0.10, 0.02),
                    // A scatter's fill would claim a density the points lack.
                    _ => (0.0, 0.0),
                };
                chart = chart
                    .y(move |d: &Arc<PlotPoint>| d.values[s])
                    .stroke(color)
                    .fill(linear_gradient(
                        0.,
                        linear_color_stop(color.opacity(top), 1.),
                        linear_color_stop(color.opacity(bottom), 0.),
                    ))
                    .name(series_name.clone());
            }
            chart.into_any_element()
        }
    }
}

/// The plot body when nothing parsed: the same medium-over-muted message the
/// results chart's empty state uses.
fn empty_chart(cx: &App) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .p_4()
        .gap_2()
        .child(
            Icon::new(IconName::ChartPie)
                .large()
                .text_color(cx.theme().muted_foreground.alpha(0.5)),
        )
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .text_center()
                .child(tr("chart.empty.no_rows")),
        )
        .into_any_element()
}

/// Compact cell padding shared by header and body cells.
fn cell_paddings() -> Edges<Pixels> {
    Edges {
        top: px(2.),
        bottom: px(2.),
        left: px(10.),
        right: px(10.),
    }
}

/// The table a `table` plot draws: the query's own columns, numeric ones
/// right-aligned, no row numbers or per-cell actions — the results panel's
/// table minus everything a dashboard does not need.
struct SpecTableDelegate {
    columns: Vec<Column>,
    result: Arc<QueryResult>,
}

impl SpecTableDelegate {
    fn new(result: Arc<QueryResult>) -> Self {
        let columns = result
            .columns
            .iter()
            .enumerate()
            .map(|(ix, column)| {
                let mut spec = Column::new(format!("c{ix}"), column.name.clone());
                if column.kind == ColumnKind::Numeric {
                    spec = spec.text_right();
                }
                spec.paddings = Some(cell_paddings());
                spec
            })
            .collect();
        Self { columns, result }
    }
}

impl TableDelegate for SpecTableDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.result.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        h_flex()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings))
            .child(
                Label::new(column.name.clone())
                    .text_align(column.align)
                    .text_sm()
                    .font_family(cx.theme().mono_font_family.clone())
                    .font_weight(FontWeight::BOLD)
                    .flex_1(),
            )
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        let text = self
            .result
            .rows
            .get(row_ix)
            .and_then(|row| row.get(col_ix))
            .cloned()
            .unwrap_or_default();
        let is_null = text == "NULL";
        h_flex()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings))
            .overflow_hidden()
            .child(
                Label::new(text)
                    .text_align(column.align)
                    .text_ellipsis()
                    .font_family(cx.theme().mono_font_family.clone())
                    .when(is_null, |this| this.text_color(cx.theme().muted_foreground))
                    .flex_1()
                    .min_w_0(),
            )
    }
}

/// Spec completion for the source editor: a thin gpui shell over
/// [`complete::complete`], wired exactly like the SQL provider — the rope
/// gives the prefix and the replace range, the pure function gives the
/// candidates.
struct SpecCompletionProvider;

/// Don't pop the menu up before an identifier character or a `.` exists.
const MAX_PREFIX_SCAN: usize = 64;

impl CompletionProvider for SpecCompletionProvider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        // Identifier prefix immediately before the cursor, as in the SQL
        // provider; after `query.` it is empty and every name matches.
        let mut prefix_len = 0usize;
        while prefix_len < MAX_PREFIX_SCAN {
            let pos = offset.saturating_sub(prefix_len + 1);
            match text.get_char(pos).ok() {
                Some(c) if c.is_alphanumeric() || c == '_' => prefix_len += 1,
                _ => break,
            }
            if pos == 0 {
                break;
            }
        }
        let start = offset - prefix_len;
        let prefix_lower = text.slice(start..offset).to_string().to_lowercase();

        // Columns count characters here and in `complete` alike: the rope's
        // own position conversion keeps the two sides agreeing.
        let position = text.offset_to_position(offset);
        let candidates = complete::complete(
            &text.to_string(),
            position.line as usize + 1,
            position.character as usize + 1,
        );

        let replace_range = lsp_types::Range {
            start: text.offset_to_position(start),
            end: position,
        };
        let items: Vec<CompletionItem> = candidates
            .into_iter()
            .filter(|candidate| starts_with_ignore_case(&candidate.label, &prefix_lower))
            .enumerate()
            .map(|(index, candidate)| {
                let kind = match candidate.kind {
                    CompletionKind::Block => CompletionItemKind::KEYWORD,
                    CompletionKind::Attribute => CompletionItemKind::PROPERTY,
                    CompletionKind::Value => CompletionItemKind::ENUM_MEMBER,
                    CompletionKind::Reference => CompletionItemKind::REFERENCE,
                };
                CompletionItem {
                    label: candidate.label,
                    kind: Some(kind),
                    detail: candidate.detail,
                    sort_text: Some(format!("{index:02}")),
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                        range: replace_range,
                        new_text: candidate.insert_text,
                    })),
                    ..Default::default()
                }
            })
            .collect();

        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        new_text
            .chars()
            .last()
            .map(|c| c.is_alphanumeric() || c == '_' || c == '.')
            .unwrap_or(false)
    }
}
