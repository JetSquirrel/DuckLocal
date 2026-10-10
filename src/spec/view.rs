//! The view half of a `.dash` spec: the file as a workspace tab.
//!
//! `mod.rs` checks a spec without a window; this module draws one. A
//! `Dashboard` parses and validates its file, runs every query on the window's
//! connection, and lays the plots out on a twelve-column grid: each plot
//! spans its `width`, plots fill a row left to right and wrap, and each row is
//! one panel of a vertical resizable stack — the row heights are the user's
//! to drag, and a stack taller than the tab scrolls. The data a plot draws is
//! `prepare`'s business; the view only renders what it prepares. A plot whose
//! query failed, or whose columns do not resolve, shows the reason in its own
//! panel: one bad plot never takes the dashboard down. And a reload follows
//! the app rule: a spec that no longer validates never replaces a working
//! dashboard — the previous one stays up and the reason appears above it.
//!
//! One chart note: the catalog's `BarChart` and `LineChart` draw a single
//! series and its one multi-series chart fills areas, so every `bar`, a
//! pivoted `line` and every `scatter` are drawn by the plots in `plot.rs` —
//! grouped bars, bare lines, unconnected dots — on the same primitives the
//! catalog charts compose.
//!
//! A `filter` block makes a plot pickable: a click on a bar, or on a table's
//! row, picks a value, and every query that reads `$name` re-runs with it —
//! only those, and only when the SQL they would run has changed; the picking
//! plot's own queries never narrow (see `filter.rs`). The picks show as chips
//! above the plots, each clearing its own; a click on the picked bar again
//! clears it too.
//!
//! The toolbar's source toggle swaps the whole body for the spec's text in an
//! editor — not a read-only one: the source is where a dashboard is fixed.
//! Edits are written back with the save button or ⌘S, and a save reloads; a
//! save whose spec no longer validates keeps the last working dashboard up
//! and says why above it. While the buffer holds unsaved edits the tab shows
//! a dirty dot, and an external change to the file is a conflict banner
//! rather than a silent overwrite; with a clean buffer it just reloads.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::chart::{AreaChart, LineChart};
use gpui_kit::component::input::{
    CompletionProvider, Editor, EditorState, Rope, RopeExt, TabSize,
};
use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::component::label::Label;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::resizable::{resizable_panel, v_resizable};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::table::{
    Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState,
};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt};
use gpui_kit::prelude::FluentBuilder;

use super::wheel::WheelLatch;
use gpui_kit::*;

use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    TextEdit,
};

use crate::spec::watch::{Debounce, FileStamp, POLL_INTERVAL};
use crate::i18n::{tr, trf};
use crate::query::{ColumnKind, QueryOutcome, QueryResult};
use crate::spec::comment_card::CommentCard;
use crate::spec::comments::{self, Author, Comments};
use crate::spec::complete::{self, CompletionKind};
use crate::spec::filter::Pick;
use crate::spec::model::{self, Spec};
use crate::spec::plot::{GroupedBars, SeriesPlot, x_label_count};
use crate::ui::chart::{format_value, legend_row, map_notes, parse_number, pie_parts};
use crate::ui::geo::GeoPlot;
use crate::spec::prepare::{prepare, PlotPoint, PreparedPlot};
use crate::ui::completion::starts_with_ignore_case;
use crate::ui::results::fit_column_width;

/// The plot panels' default and drag bounds, in the spirit of the workspace's
/// own results split.
const PLOT_DEFAULT: f32 = 300.;
const PLOT_MIN: f32 = 160.;
const PLOT_MAX: f32 = 800.;
/// A row of nothing but cards holds one number each: it starts short.
const CARD_DEFAULT: f32 = 104.;
const CARD_MIN: f32 = 80.;
const CARD_MAX: f32 = 400.;

/// What a sortable header's arrow takes beside the column name.
const SORT_ICON_WIDTH: f32 = 20.;

/// A table plot's chrome around its rows — title, header row, padding — and
/// one row's height, for sizing a row of tables to its content.
const TABLE_CHROME: f32 = 84.;
const TABLE_ROW: f32 = 30.;
const TABLE_MIN: f32 = 120.;

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
    /// Which scroller the wheel gesture in progress moves: a table plot, or
    /// the stack it sits in (see `wheel`).
    wheel: WheelLatch,
    /// The spec the plots on screen came from: which filters exist and which
    /// plot each picks on.
    spec: Option<Arc<Spec>>,
    /// What each filter has picked, by filter name. Survives reloads for as
    /// long as the filter does.
    picks: HashMap<String, Pick>,
    /// Each query's last outcome and the SQL that produced it: a re-run after
    /// a pick reuses every query whose SQL the pick did not change.
    outcomes: Outcomes,
    /// A pick changed while a run was in flight: run again when it lands.
    refilter_pending: bool,
    /// Where each pickable plot was last laid out, by plot name, for a click
    /// to find its bar.
    plot_bounds: HashMap<String, Rc<Cell<Option<Bounds<Pixels>>>>>,
    /// Row-selection subscriptions of the pickable tables, by table entity;
    /// dropped with them.
    table_subscriptions: Vec<(EntityId, Subscription)>,
    /// The plots turned over to show their SQL, by plot name.
    sql_shown: HashSet<String>,
    /// The review threads on the plots, as last read from the comments file
    /// beside the spec (see `comments`), and why it could not be read.
    comments: Comments,
    comments_error: Option<String>,
    /// The plot whose comment card is open, by plot name.
    comment_open: Option<String>,
    /// Each plot's comment card, by plot name; built lazily at render, since
    /// its text box needs a window, and kept so a half-written comment
    /// survives the card closing.
    comment_cards: HashMap<String, Entity<CommentCard>>,
}

/// What a dashboard asks of the workspace around it.
pub enum DashboardEvent {
    /// Open this SQL in a new query tab, under this title.
    OpenSql { sql: String, title: String },
}

impl EventEmitter<DashboardEvent> for Dashboard {}

/// Each query's outcome by name, with the SQL that ran.
type Outcomes = HashMap<String, (String, Result<Arc<QueryResult>, String>)>;

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
            wheel: WheelLatch::default(),
            spec: None,
            picks: HashMap::new(),
            outcomes: HashMap::new(),
            refilter_pending: false,
            plot_bounds: HashMap::new(),
            table_subscriptions: Vec::new(),
            sql_shown: HashSet::new(),
            comments: Comments::default(),
            comments_error: None,
            comment_open: None,
            comment_cards: HashMap::new(),
        };
        this.load_comments();
        this.watch(cx);
        this.reload(cx);
        this
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// What the tab shows, for `ducklocal open --state`: each plot and whether it
    /// drew, and every filter with the value it has picked, if any.
    pub fn summary(&self) -> serde_json::Value {
        use serde_json::json;
        let filters = self.spec.as_ref().map_or_else(Vec::new, |spec| {
            spec.filters
                .iter()
                .map(|filter| {
                    let pick = self.picks.get(&filter.name);
                    json!({
                        "name": filter.name,
                        "plot": filter.plot,
                        "column": filter.column,
                        "picked": pick.is_some(),
                        "value": pick.and_then(|pick| pick.value.clone()),
                    })
                })
                .collect()
        });
        json!({
            "running": self.running,
            "error": self.spec_error,
            "stale": self.stale_reason,
            "plots": self.plots.iter().map(|plot| json!({
                "name": plot.name,
                "type": plot.kind,
                "title": plot.title,
                "query": plot.query,
                "failure": plot.failure,
                "rows": self.results.get(&plot.query).map(|r| r.rows.len()),
                "open_comments": self.comments.open_on(&plot.name),
            })).collect::<Vec<_>>(),
            "filters": filters,
            "comments_file": comments::path_for(&self.path),
        })
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
        let comments_path = comments::path_for(&self.path);
        let watcher = cx.spawn(async move |this, cx| {
            let mut stamp = FileStamp::capture(&watched);
            let mut debounce = Debounce::new();
            // The comments file is followed too, so an agent's reply shows
            // on its plot without a reload.
            let mut comments_stamp = FileStamp::capture(&comments_path);
            let mut comments_debounce = Debounce::new();
            loop {
                smol::Timer::after(POLL_INTERVAL).await;
                let next = FileStamp::capture(&comments_path);
                let changed = next != comments_stamp;
                comments_stamp = next;
                if comments_debounce.observe(Instant::now(), changed)
                    && this
                        .update(cx, |this, cx| {
                            this.load_comments();
                            cx.notify();
                        })
                        .is_err()
                {
                    break;
                }
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

    /// Re-read the comments file. A file that does not read keeps the threads
    /// last shown, and says why on the comments faces.
    fn load_comments(&mut self) {
        match comments::load(&self.path) {
            Ok(loaded) => {
                self.comments = loaded;
                self.comments_error = None;
            }
            Err(error) => self.comments_error = Some(error),
        }
    }

    /// The threads on the plots, for the comment cards.
    pub fn comments(&self) -> &Comments {
        &self.comments
    }

    pub fn comments_error(&self) -> Option<String> {
        self.comments_error.clone()
    }

    /// A plot's title, for the comment card to quote; its name when the plot
    /// is gone.
    pub fn plot_title(&self, plot: &str) -> SharedString {
        self.plots
            .iter()
            .find(|p| p.name == plot)
            .map(|p| SharedString::from(p.title.to_string()))
            .unwrap_or_else(|| SharedString::from(plot.to_string()))
    }

    pub fn close_comments(&mut self, cx: &mut Context<Self>) {
        self.comment_open = None;
        cx.notify();
    }

    /// Post a comment on `plot`: a reply to `replying`, or a new thread.
    /// Written straight to the comments file; `false` when it could not be,
    /// with the reason kept for the card to show.
    pub fn post_comment(
        &mut self,
        plot: &str,
        replying: Option<&str>,
        text: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let result = comments::update(&self.path, |c| match replying {
            Some(id) => c.reply(id, Author::User, text),
            None => {
                c.add(plot, Author::User, text);
                Ok(())
            }
        });
        let posted = self.comments_written(result);
        cx.notify();
        posted
    }

    pub fn set_resolved(&mut self, id: &str, resolved: bool, cx: &mut Context<Self>) {
        let result = comments::update(&self.path, |c| c.set_resolved(id, resolved));
        self.comments_written(result);
        cx.notify();
    }

    fn comments_written(&mut self, result: Result<((), Comments), String>) -> bool {
        match result {
            Ok((_, loaded)) => {
                self.comments = loaded;
                self.comments_error = None;
                true
            }
            Err(error) => {
                self.comments_error = Some(error);
                false
            }
        }
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
        // A reload re-reads the data as well as the spec: nothing is reused.
        self.start_run(HashMap::new(), cx);
    }

    /// Re-run after a pick changed: every query whose SQL the picks leave as
    /// it was keeps its result, so only the narrowed plots redraw.
    fn refilter(&mut self, cx: &mut Context<Self>) {
        if self.running {
            self.refilter_pending = true;
            return;
        }
        self.start_run(self.outcomes.clone(), cx);
    }

    fn start_run(&mut self, reuse: Outcomes, cx: &mut Context<Self>) {
        self.running = true;
        cx.notify();
        let path = self.path.clone();
        let picks = self.picks.clone();
        cx.spawn(async move |this, cx| {
            let run = smol::unblock(move || load(&path, &picks, &reuse)).await;
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
                        // A table whose query came back as the very same
                        // result keeps its entity: its scroll position and
                        // its selected row — the pick — are the user's.
                        let tables = run
                            .plots
                            .iter()
                            .map(|plot| {
                                let old = this
                                    .plots
                                    .iter()
                                    .position(|p| p.name == plot.name && p.kind == "table")?;
                                let same = match (
                                    this.results.get(&plot.query),
                                    run.results.get(&plot.query),
                                ) {
                                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                                    _ => false,
                                };
                                same.then(|| this.tables.get(old).cloned().flatten()).flatten()
                            })
                            .collect::<Vec<_>>();
                        this.table_subscriptions.retain(|(id, _)| {
                            tables.iter().flatten().any(|table| table.entity_id() == *id)
                        });
                        this.tables = tables;
                        this.plots = run.plots;
                        this.results = run.results;
                        this.outcomes = run.outcomes;
                        this.spec = run.spec;
                        // A pick whose filter is gone from the spec goes too.
                        let spec = this.spec.clone();
                        this.picks.retain(|name, _| {
                            spec.as_ref()
                                .is_some_and(|spec| spec.filters.iter().any(|f| &f.name == name))
                        });
                    }
                }
                if std::mem::take(&mut this.refilter_pending) {
                    this.refilter(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Whether a click on this plot picks a value: some filter names it.
    fn is_pickable(&self, plot: &str) -> bool {
        self.spec
            .as_ref()
            .is_some_and(|spec| spec.filters.iter().any(|f| f.plot == plot))
    }

    /// The band a bar plot shows as picked: the value of a filter that picks
    /// the plot's own x.
    fn picked_band(&self, plot: &PreparedPlot) -> Option<SharedString> {
        let spec = self.spec.as_ref()?;
        spec.filters
            .iter()
            .filter(|f| f.plot == plot.name && f.column.eq_ignore_ascii_case(&plot.label_name))
            .find_map(|f| self.picks.get(&f.name))
            .map(|pick| SharedString::from(pick.value.clone().unwrap_or_else(|| "NULL".into())))
    }

    /// A click on a pickable bar plot: the band under it becomes the pick —
    /// or, when it is the band already picked, the pick is cleared.
    fn bar_clicked(&mut self, ix: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        let plot = &self.plots[ix];
        let Some(bounds) = self.plot_bounds.get(&plot.name).and_then(|cell| cell.get()) else {
            return;
        };
        let local = point(position.x - bounds.origin.x, position.y - bounds.origin.y);
        let Some(band) = GroupedBars::new("dashboard-hit", plot).band_at(local, bounds) else {
            return;
        };
        let Some(result) = self.results.get(&plot.query).cloned() else {
            return;
        };
        // The row the bar came from: the first whose x is the band.
        let Some(x) = result
            .columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(&plot.label_name))
        else {
            return;
        };
        let Some(row) = result
            .rows
            .iter()
            .position(|row| row.get(x).is_some_and(|cell| cell.as_str() == band.as_ref()))
        else {
            return;
        };
        let name = plot.name.clone();
        self.pick_row(&name, &result, row, true, cx);
    }

    /// Pick, for every filter on `plot`, its column's value in `row`. With
    /// `toggle`, a pick that is already the one made clears instead.
    fn pick_row(
        &mut self,
        plot: &str,
        result: &QueryResult,
        row: usize,
        toggle: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(spec) = self.spec.clone() else {
            return;
        };
        let mut changed = false;
        for filter in spec.filters.iter().filter(|f| f.plot == plot) {
            let Some(column) = result
                .columns
                .iter()
                .position(|c| c.name.eq_ignore_ascii_case(&filter.column))
            else {
                continue;
            };
            let Some(cell) = result.rows.get(row).and_then(|r| r.get(column)) else {
                continue;
            };
            let pick = Pick {
                column: filter.column.clone(),
                // A cell showing NULL: SQL NULL, or the text 'NULL'.
                value: (cell != "NULL").then(|| cell.clone()),
            };
            if self.picks.get(&filter.name) == Some(&pick) {
                if toggle {
                    self.picks.remove(&filter.name);
                    changed = true;
                }
            } else {
                self.picks.insert(filter.name.clone(), pick);
                changed = true;
            }
        }
        if changed {
            self.refilter(cx);
            cx.notify();
        }
    }

    /// Clear one filter's pick, or every pick; a table it was picked on lets
    /// go of its selected row.
    fn clear_picks(&mut self, only: Option<&str>, cx: &mut Context<Self>) {
        let cleared: Vec<String> = self
            .picks
            .keys()
            .filter(|name| only.is_none_or(|only| only == name.as_str()))
            .cloned()
            .collect();
        if cleared.is_empty() {
            return;
        }
        for name in &cleared {
            self.picks.remove(name);
        }
        if let Some(spec) = self.spec.clone() {
            for filter in spec.filters.iter().filter(|f| cleared.contains(&f.name)) {
                for (ix, plot) in self.plots.iter().enumerate() {
                    if plot.name == filter.plot {
                        if let Some(Some(table)) = self.tables.get(ix) {
                            table.update(cx, |table, cx| table.clear_selection(cx));
                        }
                    }
                }
            }
        }
        self.refilter(cx);
        cx.notify();
    }

    /// The picks in force, as chips above the plots: each says what it
    /// narrows to and clears on its ✕.
    fn render_picks(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.picks.is_empty() {
            return None;
        }
        let spec = self.spec.clone()?;
        let chips = spec
            .filters
            .iter()
            .filter_map(|filter| Some((filter, self.picks.get(&filter.name)?)))
            .enumerate()
            .map(|(ix, (filter, pick))| {
                let name = filter.name.clone();
                let value = pick.value.clone().unwrap_or_else(|| "NULL".into());
                Button::new(("dashboard-pick", ix))
                    .outline()
                    .xsmall()
                    .label(format!("{} = {value}", filter.column))
                    .icon(IconName::Close)
                    .tooltip(tr("dashboard.filter.clear"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.clear_picks(Some(&name), cx)
                    }))
            })
            .collect::<Vec<_>>();
        let several = chips.len() > 1;
        Some(
            h_flex()
                .flex_none()
                .flex_wrap()
                .gap_2()
                .px_3()
                .py_1p5()
                .items_center()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr("dashboard.filter.label")),
                )
                .children(chips)
                .when(several, |this| {
                    this.child(
                        Button::new("dashboard-picks-clear")
                            .ghost()
                            .xsmall()
                            .label(tr("dashboard.filter.clear_all"))
                            .on_click(cx.listener(|this, _, _, cx| this.clear_picks(None, cx))),
                    )
                })
                .into_any_element(),
        )
    }

    /// One panel of the grid: the plot, or — turned over by the SQL button
    /// in its top-right corner — the SQL behind it. Beside that button, the
    /// comment button opens the plot's comment card over the dashboard.
    /// Both stay out of the way until the pointer is over the plot, except
    /// that a plot with open threads keeps their count in sight.
    fn render_plot(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let group = SharedString::from(format!("dashboard-plot-{ix}"));
        let shown = self.sql_shown.contains(&self.plots[ix].name);
        let face = if shown {
            self.render_sql(ix, cx)
        } else {
            self.render_plot_face(ix, window, cx)
        };
        let name = self.plots[ix].name.clone();
        let toggle = Button::new(("dashboard-sql", ix))
            .xsmall()
            .ghost()
            .label(if shown { tr("dashboard.sql.hide") } else { "SQL" })
            .tooltip(if shown {
                tr("dashboard.sql.hide_tooltip")
            } else {
                tr("dashboard.sql.show_tooltip")
            })
            .on_click(cx.listener({
                let name = name.clone();
                move |this, _, _, cx| {
                    if !this.sql_shown.remove(&name) {
                        this.sql_shown.insert(name.clone());
                    }
                    cx.notify();
                }
            }));
        let comments = self.render_comment_button(ix, window, cx);
        let open = self.comments.open_on(&name);
        let commenting = self.comment_open.as_deref() == Some(name.as_str());
        let hover_only = |this: Div, keep: bool| {
            if keep {
                this
            } else {
                this.opacity(0.).group_hover(group.clone(), |style| style.opacity(1.))
            }
        };
        div()
            .size_full()
            .relative()
            .group(group.clone())
            .child(face)
            .child(
                h_flex()
                    .absolute()
                    .top_1p5()
                    .right_2()
                    .gap_0p5()
                    .child(hover_only(div(), open > 0 || commenting).child(comments))
                    .child(
                        hover_only(div(), shown)
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().background)
                            .child(toggle),
                    ),
            )
            .into_any_element()
    }

    /// The height a grid row of nothing but drawn tables needs to show every
    /// row of the longest, up to a chart's default height; `None` for any
    /// other row.
    fn table_row_height(&self, row: &[usize]) -> Option<f32> {
        let rows = row
            .iter()
            .map(|&ix| {
                let plot = &self.plots[ix];
                if plot.kind != "table" || plot.failure.is_some() {
                    return None;
                }
                let result = self.results.get(&plot.query)?;
                // A truncated result carries a banner above the grid.
                Some(result.rows.len() + usize::from(result.truncated))
            })
            .collect::<Option<Vec<_>>>()?;
        let longest = rows.into_iter().max()? as f32;
        Some((TABLE_CHROME + longest * TABLE_ROW).min(PLOT_DEFAULT))
    }

    /// The comment button and the card it opens: a speech bubble, with the
    /// count of open threads beside it once there are any.
    fn render_comment_button(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = self.plots[ix].name.clone();
        let open = self.comments.open_on(&name);
        let is_open = self.comment_open.as_deref() == Some(name.as_str());
        let card = match self.comment_cards.get(&name) {
            Some(card) => card.clone(),
            None => {
                let dashboard = cx.entity().downgrade();
                let plot = name.clone();
                let card = cx.new(|cx| CommentCard::new(dashboard, plot, window, cx));
                self.comment_cards.insert(name.clone(), card.clone());
                card
            }
        };
        let button = Button::new(("dashboard-comments", ix))
            .xsmall()
            .map(|this| if open > 0 { this.outline() } else { this.ghost() })
            .icon(Icon::new(AssetIcon::MessageSquare))
            .when(open > 0, |this| this.label(open.to_string()))
            .tooltip(tr("dashboard.comments.show_tooltip"));
        let focus_card = card.clone();
        Popover::new(("dashboard-comment-card", ix))
            .anchor(Anchor::TopRight)
            .open(is_open)
            .on_open_change(cx.listener(move |this, open: &bool, window, cx| {
                this.comment_open = open.then(|| name.clone());
                if *open {
                    CommentCard::focus(&focus_card, window, cx);
                }
                cx.notify();
            }))
            .trigger(button)
            .content(move |_, _, _| card.clone())
            .into_any_element()
    }

    /// The SQL a plot's query last ran — sources and the current picks
    /// included, so it runs as it stands in a query tab — or, before it has
    /// run, the query as written.
    fn render_sql(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let plot = &self.plots[ix];
        let title = plot.title.clone();
        let sql = self
            .outcomes
            .get(&plot.query)
            .map(|(sql, _)| sql.clone())
            .or_else(|| {
                self.spec.as_ref().and_then(|spec| {
                    spec.queries
                        .iter()
                        .find(|q| q.name == plot.query)
                        .map(|q| q.sql.clone())
                })
            })
            .unwrap_or_default();
        let sql = sql.trim().to_string();
        let copied = sql.clone();
        let opened = sql.clone();
        let tab_title = title.clone();
        v_flex()
            .size_full()
            .px_3()
            .py_2()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_ellipsis()
                    .pr_16()
                    .child(title),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new(("dashboard-sql-copy", ix))
                            .xsmall()
                            .ghost()
                            .icon(IconName::Copy)
                            .label(tr("dashboard.sql.copy"))
                            .on_click(move |_, _, cx: &mut App| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))
                            }),
                    )
                    .child(
                        Button::new(("dashboard-sql-open", ix))
                            .xsmall()
                            .ghost()
                            .label(tr("dashboard.sql.open"))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(DashboardEvent::OpenSql {
                                    sql: opened.clone(),
                                    title: tab_title.clone(),
                                })
                            })),
                    ),
            )
            .child(
                div()
                    .id(("dashboard-sql-text", ix))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_xs()
                    .child(sql),
            )
            .into_any_element()
    }

    fn render_plot_face(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.plots[ix].kind == "card" && self.plots[ix].failure.is_none() {
            // The card's share of the dashboard's last laid-out width, less
            // its padding: what its value has to fit in.
            let room = self.wheel.viewport().get().map(|viewport| {
                viewport.size.width.as_f32() * self.plots[ix].width as f32
                    / model::GRID_COLUMNS as f32
                    - CARD_PADDING
            });
            return render_card(&self.plots[ix], room, cx);
        }
        let (title, mut notice, failure, is_table, is_empty) = {
            let plot = &self.plots[ix];
            let is_empty = match plot.kind.as_str() {
                "map" => plot.geo.as_ref().is_none_or(|geo| geo.point_count() == 0),
                "pie" => plot.pie.is_empty(),
                _ => plot.points.is_empty(),
            };
            (
                plot.title.clone(),
                plot.notice.clone(),
                plot.failure.clone(),
                plot.kind == "table",
                is_empty,
            )
        };
        // The axis subtitle the results chart shows: which series, over which
        // x column — for a pie, what the shares are of; for a map, which
        // columns place the points. Tables and failures have nothing to say.
        let drawn = !is_table && failure.is_none() && !is_empty;
        if failure.is_none() && self.is_pickable(&self.plots[ix].name) {
            let hint = tr("dashboard.filter.hint").to_string();
            notice = Some(match notice.take() {
                Some(notice) => format!("{notice} · {hint}"),
                None => hint,
            });
        }
        let mut legend = Vec::new();
        let axes = drawn.then(|| {
            let plot = &self.plots[ix];
            let series = plot.series_names.join(", ");
            match plot.kind.as_str() {
                "map" => {
                    if let Some(geo) = &plot.geo {
                        let (notes, key) = map_notes(geo, cx);
                        notice = match (notice.take(), notes) {
                            (Some(a), Some(b)) => Some(format!("{a} · {b}")),
                            (a, b) => a.or(b),
                        };
                        legend = key;
                    }
                    plot.label_name.clone()
                }
                "pie" => {
                    legend = pie_parts(&plot.pie, series.clone().into(), "dashboard-pie-key", cx).1;
                    trf("chart.pie.title", &[&series, &plot.label_name])
                }
                _ => trf("chart.title.by", &[&series, &plot.label_name]),
            }
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
        } else if self.plots[ix].kind == "bar" && self.is_pickable(&self.plots[ix].name) {
            let picked = self.picked_band(&self.plots[ix]);
            let chart = GroupedBars::new(
                (ElementId::from("dashboard-chart"), self.plots[ix].name.clone()),
                &self.plots[ix],
            )
            .selected(picked);
            // The chart fills this box, so the box's bounds are the chart's:
            // the canvas records them for the click to hit-test against.
            let bounds = self
                .plot_bounds
                .entry(self.plots[ix].name.clone())
                .or_default()
                .clone();
            div()
                .size_full()
                .px_2()
                .pb_2()
                .child(
                    div()
                        .size_full()
                        .relative()
                        .cursor_pointer()
                        .child(chart)
                        .child(
                            canvas(move |b, _, _| bounds.set(Some(b)), |_, _, _, _| {})
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full(),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                this.bar_clicked(ix, event.position, cx)
                            }),
                        ),
                )
                .into_any_element()
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
                    })
                    .when(!legend.is_empty(), |this| this.child(legend_row(legend, cx))),
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
            let table = cx.new(|cx| TableState::new(delegate, window, cx));
            let plot = self.plots[ix].name.clone();
            if self.is_pickable(&plot) {
                // A row selected — by click or by the arrow keys — is a pick;
                // the table's own highlight is what shows it. The row is a
                // position in the shown order, which a sort may have changed.
                let picked_from = result.clone();
                let subscription =
                    cx.subscribe(&table, move |this, table, event: &TableEvent, cx| {
                        if let TableEvent::SelectRow(row) = event {
                            let row = table.read(cx).delegate().source_row(*row);
                            this.pick_row(&plot, &picked_from, row, false, cx);
                        }
                    });
                self.table_subscriptions.push((table.entity_id(), subscription));
            }
            // After a sort, the selected row moves to where its row went.
            let subscription = cx.observe(&table, |_, table, cx| {
                table.update(cx, |table, cx| {
                    let Some(before) = table.delegate_mut().resorted.take() else {
                        return;
                    };
                    let Some(selected) = table.selected_row() else {
                        return;
                    };
                    let row = before.get(selected).copied().unwrap_or(selected);
                    if let Some(shown) = table.delegate().order.iter().position(|&r| r == row) {
                        table.set_selected_row(shown, cx);
                    }
                });
            });
            self.table_subscriptions.push((table.entity_id(), subscription));
            self.tables[ix] = Some(table);
        }
        let table = self.tables[ix].clone().expect("built just above");
        let truncated = result.truncated;
        let row_count = result.rows.len();
        let rows = table.read(cx).vertical_scroll_handle.0.borrow().base_handle.clone();
        // Stripes only once the rows fill the panel: striping pads a short
        // table with empty rows down to the panel's foot, which reads as
        // rows of nothing rather than as the end of the data.
        let overflows = rows.max_offset().y > px(0.);
        let wheel = self.wheel.clone();
        // Where the table sits, for the wheel to tell whether it is on screen.
        let bounds: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::default();
        let record = bounds.clone();
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
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    // Runs after the table's own scrolling and before the
                    // stack's, so it decides whether the stack moves too.
                    .on_scroll_wheel(move |event, window, cx| {
                        if wheel.table_scrolled(ix, &rows, bounds.get(), event, window) {
                            cx.stop_propagation();
                        }
                    })
                    .child(
                        DataTable::new(&table)
                            .small()
                            .stripe(overflows)
                            .scrollbar_visible(true, true),
                    )
                    .child(
                        canvas(move |b, _, _| record.set(Some(b)), |_, _, _, _| {})
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full(),
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
                        .child(tr("dashboard.not_updated")),
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
            let rows = grid_rows(self.plots.iter().map(|plot| plot.width));
            let mut stack = 0.;
            let panels = rows
                .iter()
                .map(|row| {
                    let cards = row.iter().all(|&ix| self.plots[ix].kind == "card");
                    let (default, min, max) = if cards {
                        (CARD_DEFAULT, CARD_MIN, CARD_MAX)
                    } else if let Some(height) = self.table_row_height(row) {
                        // A row of short tables is as tall as its rows, not
                        // a chart's height of empty space under them.
                        (height, TABLE_MIN.min(height), PLOT_MAX)
                    } else {
                        (PLOT_DEFAULT, PLOT_MIN, PLOT_MAX)
                    };
                    stack += default;
                    // A row short of twelve columns leaves the rest empty,
                    // so a plot's width means the same on every row.
                    let cells = row.iter().enumerate().map(|(position, &ix)| {
                        let share = self.plots[ix].width as f32 / model::GRID_COLUMNS as f32;
                        div()
                            .h_full()
                            .w(relative(share))
                            .min_w_0()
                            .when(position > 0, |this| {
                                this.border_l_1().border_color(cx.theme().border)
                            })
                            .child(self.render_plot(ix, window, cx))
                    });
                    let cells = cells.collect::<Vec<_>>();
                    resizable_panel()
                        .size(px(default))
                        .size_range(px(min)..px(max))
                        .child(h_flex().size_full().children(cells))
                })
                .collect::<Vec<_>>();
            // Every row keeps its default height and the tab scrolls, rather
            // than a dozen plots squeezed into one screen and the rest clipped
            // out of reach. Dragging a divider still trades height between
            // neighbours; a tab taller than the stack is filled, as before.
            let stack = px(stack);
            let wheel = self.wheel.clone();
            let viewport = self.wheel.viewport();
            // The canvas sits outside the scrolling div, so its bounds are
            // the viewport's, not the scrolled content's.
            div()
                .size_full()
                .relative()
                .child(
                    div()
                        .id(format!("dashboard-scroll-{}", self.id))
                        .size_full()
                        .on_scroll_wheel(move |event, window, _| {
                            wheel.stack_scrolled(event, window)
                        })
                        .overflow_y_scrollbar()
                        .child(
                            div().w_full().h(stack).min_h_full().child(
                                v_resizable(format!("dashboard-{}", self.id)).children(panels),
                            ),
                        ),
                )
                .child(
                    canvas(move |b, _, _| viewport.set(Some(b)), |_, _, _, _| {})
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                )
                .into_any_element()
        };

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .children(self.render_conflict_warning(cx))
            .children(self.render_stale_warning(cx))
            .when(!self.showing_source, |this| this.children(self.render_picks(cx)))
            .child(body)
            .into_any_element()
    }
}

/// Everything one run of a dashboard produces, computed off the UI thread.
struct Run {
    spec_error: Option<String>,
    spec: Option<Arc<Spec>>,
    plots: Vec<PreparedPlot>,
    results: HashMap<String, Arc<QueryResult>>,
    outcomes: Outcomes,
}

/// Read, parse, validate, run, prepare. Blocking — call it off the UI thread.
/// `picks` narrow the queries that read their filters; a query whose SQL
/// comes out as `reuse` last ran it keeps that outcome instead of running.
fn load(path: &Path, picks: &HashMap<String, Pick>, reuse: &Outcomes) -> Run {
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
                spec: None,
                plots: Vec::new(),
                results: HashMap::new(),
                outcomes: HashMap::new(),
            };
        }
    };

    // Every query runs once, however many plots draw from it, and only after
    // it has been shown to read and nothing else: opening a file someone sent
    // must not be what runs its `DROP` or `COPY … TO`.
    // The check is on the SQL as written; what runs has the sources it names
    // in front of it, which only read.
    // Picks go in before the sources, which only read.
    let base = super::source::base_of(path);
    let mut outcomes: Outcomes = HashMap::new();
    for query in &spec.queries {
        let sql = super::validate_query_sql(&query.sql).map(|()| {
            let narrowed = super::filter::apply(&query.sql, &picks_for(&spec, query, picks));
            super::source::expand(&narrowed, &spec.sources, &base)
        });
        let entry = match sql {
            Err(error) => (String::new(), Err(error)),
            Ok(sql) => match reuse.get(&query.name) {
                Some((ran, outcome)) if *ran == sql => (sql, outcome.clone()),
                _ => {
                    let outcome = match crate::query::run(&sql) {
                        Ok(QueryOutcome::Rows(result)) => Ok(Arc::new(result)),
                        Ok(QueryOutcome::Affected { .. }) => {
                            Err(trf("dashboard.query_not_rows", &[&query.name]))
                        }
                        Err(e) => Err(format!("{e:#}")),
                    };
                    (sql, outcome)
                }
            },
        };
        outcomes.insert(query.name.clone(), entry);
    }

    // validate() has already said every plot's query exists.
    let plots = spec
        .plots
        .iter()
        .map(|plot| prepare(plot, outcomes.get(&plot.query).map(|(_, outcome)| outcome)))
        .collect();

    let results = outcomes
        .iter()
        .filter_map(|(name, (_, outcome))| {
            outcome.as_ref().ok().map(|result| (name.clone(), result.clone()))
        })
        .collect();

    Run {
        spec_error: None,
        spec: Some(Arc::new(spec)),
        plots,
        results,
        outcomes,
    }
}

/// The picks a query sees: each filter it reads that has one — except a
/// filter picked on a plot this very query draws, which would otherwise
/// narrow the plot down to the one value just clicked.
fn picks_for(
    spec: &Spec,
    query: &model::Query,
    picks: &HashMap<String, Pick>,
) -> HashMap<String, Pick> {
    spec.filters_of(query)
        .into_iter()
        .filter(|filter| {
            spec.plots
                .iter()
                .find(|plot| plot.name == filter.plot)
                .is_none_or(|plot| plot.query != query.name)
        })
        .filter_map(|filter| {
            let pick = picks.get(&filter.name)?;
            // The column is the spec's as it stands, not as it was clicked.
            Some((
                filter.name.clone(),
                Pick {
                    column: filter.column.clone(),
                    value: pick.value.clone(),
                },
            ))
        })
        .collect()
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
    // The hand-built plots key their hover state and path caches on the
    // plot's name — unique after validation — which survives reordering the
    // spec's blocks where the panel index would not.
    let named_id = || (ElementId::from("dashboard-chart"), plot.name.clone());
    // A label count the axis can fit, not a stride: wide labels (dates) get
    // few ticks, and the first and last value are always among them.
    let x_labels = x_label_count(&plot.points);
    let single = plot.series_names.len() <= 1;
    let name = plot
        .series_names
        .first()
        .cloned()
        .unwrap_or_else(|| plot.title.clone());

    match plot.kind.as_str() {
        "pie" => pie_parts(&plot.pie, name.into(), named_id(), cx).0,
        "map" => match &plot.geo {
            Some(geo) => GeoPlot::new(named_id(), geo.clone()).into_any_element(),
            None => empty_chart(cx),
        },
        // Every bar draws on the hand-built chart, one series or several: it
        // colours by series, and lays a category axis on its side rather than
        // drop the labels that do not fit.
        "bar" => GroupedBars::new(named_id(), plot).into_any_element(),
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
                .x_tick_count(x_labels)
                .tooltip_value(|_: &Arc<PlotPoint>, _: usize, v: f64| format_value(v).into())
                .into_any_element()
        }
        // A scatter's points are not connected, and its x is the band axis
        // `prepare` built — the hand-built plot draws it that way, one series
        // or many.
        "scatter" => SeriesPlot::scatter(named_id(), plot).into_any_element(),
        _ if single => LineChart::new(plot.points.clone())
            .x(|d: &Arc<PlotPoint>| d.band.clone())
            .y(|d: &Arc<PlotPoint>| d.values[0])
            .stroke(palette[0])
            .name(name)
            .id(id)
            .x_tick_count(x_labels)
            .tooltip_value(|_: &Arc<PlotPoint>, v: f64| format_value(v).into())
            .into_any_element(),
        // The catalog's one multi-series chart fills; filling is what `area`
        // asks for.
        "area" => {
            let mut chart = AreaChart::new(plot.points.clone())
                .x(|d: &Arc<PlotPoint>| d.band.clone())
                .id(id)
                .x_tick_count(x_labels)
                .tooltip_value(|_: &Arc<PlotPoint>, _: usize, v: f64| format_value(v).into());
            for (s, series_name) in plot.series_names.iter().enumerate() {
                let color = palette[s % palette.len()];
                chart = chart
                    .y(move |d: &Arc<PlotPoint>| d.values[s])
                    .stroke(color)
                    .fill(linear_gradient(
                        0.,
                        linear_color_stop(color.opacity(0.4), 1.),
                        linear_color_stop(color.opacity(0.05), 0.),
                    ))
                    .name(series_name.clone());
            }
            chart.into_any_element()
        }
        // A pivoted line draws lines, no fill.
        _ => SeriesPlot::lines(named_id(), plot).into_any_element(),
    }
}

/// Plots in rows of the grid: each row takes plots in order until the next
/// would not fit in the twelve columns, then a new row starts. Indices into
/// the plot list, every plot in exactly one row.
fn grid_rows(widths: impl IntoIterator<Item = u8>) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut used = model::GRID_COLUMNS;
    for (ix, width) in widths.into_iter().enumerate() {
        if used + width > model::GRID_COLUMNS {
            rows.push(Vec::new());
            used = 0;
        }
        used += width;
        rows.last_mut().expect("pushed above").push(ix);
    }
    rows
}

/// A card: its title over its one value, large. What was dropped to get to
/// one value — the rows past the first — is said under it, small.
/// A card's horizontal padding, both sides.
const CARD_PADDING: f32 = 32.;
/// A card value's font size: as large as this, and never smaller than the
/// floor — past that, a value that still does not fit is cut off.
const CARD_VALUE_MAX: f32 = 30.;
const CARD_VALUE_MIN: f32 = 14.;

/// The font size at which `value` fits across `room` pixels. Digits and
/// separators are about 0.6em wide in the UI font; a wide (CJK) character
/// counts double.
fn card_value_size(value: &str, room: Option<f32>) -> f32 {
    use unicode_width::UnicodeWidthStr;
    let Some(room) = room.filter(|room| *room > 0.) else {
        return CARD_VALUE_MAX;
    };
    let ems = value.width().max(1) as f32 * 0.6;
    (room / ems).clamp(CARD_VALUE_MIN, CARD_VALUE_MAX)
}

fn render_card(plot: &PreparedPlot, room: Option<f32>, cx: &App) -> AnyElement {
    let value = plot.card.clone().unwrap_or_default();
    let size = card_value_size(&value, room);
    v_flex()
        .size_full()
        .px_4()
        .py_3()
        .gap_1()
        .justify_center()
        .overflow_hidden()
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .text_ellipsis()
                .child(plot.title.clone()),
        )
        .child(
            // A number is never ellipsized while a smaller size fits it:
            // `11,439,7…` reads as a different number.
            div()
                .text_size(px(size))
                .line_height(px(size * 1.2))
                .font_weight(FontWeight::SEMIBOLD)
                .whitespace_nowrap()
                .text_ellipsis()
                .child(value),
        )
        .when_some(plot.notice.clone(), |this, notice| {
            this.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .text_ellipsis()
                    .child(notice),
            )
        })
        .into_any_element()
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
    /// The result's rows in the order shown: `order[shown]` is the row of
    /// `result`. The query's own order until a header sorts it.
    order: Vec<usize>,
    /// The order before the last sort, until the view has moved the selected
    /// row along with it: the table keeps a selection by position, and a
    /// pick is a row, not a position.
    resorted: Option<Vec<usize>>,
}

impl SpecTableDelegate {
    fn new(result: Arc<QueryResult>) -> Self {
        let columns = result
            .columns
            .iter()
            .enumerate()
            .map(|(ix, column)| {
                // The header also holds the sort arrow beside its name.
                let width = fit_column_width(&column.name, &result.rows, ix, SORT_ICON_WIDTH);
                let mut spec = Column::new(format!("c{ix}"), column.name.clone())
                    .width(width)
                    .sortable();
                if column.kind == ColumnKind::Numeric {
                    spec = spec.text_right();
                }
                spec.paddings = Some(cell_paddings());
                spec
            })
            .collect();
        let order = (0..result.rows.len()).collect();
        Self {
            columns,
            result,
            order,
            resorted: None,
        }
    }

    /// The row of the result shown at `shown`.
    fn source_row(&self, shown: usize) -> usize {
        self.order.get(shown).copied().unwrap_or(shown)
    }
}

/// The rows of `result` ordered by column `col`: numerically for a number
/// column, as text otherwise — ISO dates and times sort right as text. NULL
/// sorts last either way, and equal cells keep the query's order.
fn sorted_rows(result: &QueryResult, col: usize, sort: ColumnSort) -> Vec<usize> {
    let mut order: Vec<usize> = (0..result.rows.len()).collect();
    if sort == ColumnSort::Default {
        return order;
    }
    let cell = |row: usize| {
        result
            .rows
            .get(row)
            .and_then(|r| r.get(col))
            .map(String::as_str)
            .filter(|cell| *cell != "NULL")
    };
    let numeric = result
        .columns
        .get(col)
        .is_some_and(|c| c.kind == ColumnKind::Numeric);
    let compare = |a: &str, b: &str| {
        if numeric {
            if let (Some(a), Some(b)) = (parse_number(a), parse_number(b)) {
                return a.total_cmp(&b);
            }
        }
        a.cmp(b)
    };
    let descending = sort == ColumnSort::Descending;
    order.sort_by(|&a, &b| match (cell(a), cell(b)) {
        (Some(a), Some(b)) if descending => compare(b, a),
        (Some(a), Some(b)) => compare(a, b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    order
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

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) {
        let order = sorted_rows(&self.result, col_ix, sort);
        self.resorted = Some(std::mem::replace(&mut self.order, order));
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
            .get(self.source_row(row_ix))
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

#[cfg(test)]
mod tests {
    // Not `use super::*`: that brings `gpui_kit::*`, whose `test` macro
    // shadows the built-in `#[test]`.
    use super::{
        card_value_size, grid_rows, load, sorted_rows, Pick, Run, CARD_VALUE_MAX, CARD_VALUE_MIN,
    };
    use crate::query::{ColumnKind, ColumnMeta, QueryResult};
    use gpui_kit::component::table::ColumnSort;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;

    #[test]
    fn a_table_sorts_numbers_as_numbers_and_nulls_last() {
        let column = |name: &str, kind| ColumnMeta {
            name: name.into(),
            duck_type: String::new(),
            kind,
        };
        let result = QueryResult {
            columns: vec![column("name", ColumnKind::Text), column("n", ColumnKind::Numeric)],
            rows: [["b", "10"], ["a", "9"], ["c", "NULL"], ["d", "1,200"]]
                .iter()
                .map(|row| row.iter().map(|cell| cell.to_string()).collect())
                .collect(),
            elapsed_ms: 0,
            truncated: false,
        };
        assert_eq!(sorted_rows(&result, 1, ColumnSort::Ascending), [1, 0, 3, 2]);
        assert_eq!(sorted_rows(&result, 1, ColumnSort::Descending), [3, 0, 1, 2]);
        assert_eq!(sorted_rows(&result, 0, ColumnSort::Ascending), [1, 0, 2, 3]);
        // A third click goes back to the query's own order.
        assert_eq!(sorted_rows(&result, 1, ColumnSort::Default), [0, 1, 2, 3]);
    }

    const BOARD: &str = r#"
source "orders" { path = "orders.csv" }
query "by_channel" { sql = "SELECT channel, sum(amount) AS amount FROM orders GROUP BY 1 ORDER BY 1" }
query "total" { sql = "SELECT sum(amount) AS total FROM orders WHERE $channel" }
query "count" { sql = "SELECT count(*) AS n FROM orders" }
plot "channels" { type = "bar" query = query.by_channel x = channel y = amount }
plot "total" { type = "card" query = query.total }
plot "count" { type = "card" query = query.count }
filter "channel" { plot = plot.channels }
"#;

    fn total(run: &Run) -> String {
        run.results["total"].rows[0][0].clone()
    }

    #[test]
    fn a_pick_narrows_the_queries_that_read_it_and_reuses_the_rest() {
        let _guard = crate::db::connection_guard();
        crate::db::open_memory().unwrap();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/view-tests")
            .join(format!("pick-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("orders.csv"), "channel,amount\nweb,10\nshop,20\nweb,5\n,1\n")
            .unwrap();
        let path = dir.join("board.dash");
        std::fs::write(&path, BOARD).unwrap();

        let fresh = load(&path, &HashMap::new(), &HashMap::new());
        assert_eq!(fresh.spec_error, None);
        assert_eq!(total(&fresh), "36");

        let mut picks = HashMap::new();
        picks.insert(
            "channel".to_string(),
            Pick {
                column: "channel".into(),
                value: Some("web".into()),
            },
        );
        let narrowed = load(&path, &picks, &fresh.outcomes);
        assert_eq!(total(&narrowed), "15");
        // The picking plot's own query is not narrowed, and neither it nor a
        // query that reads no filter ran again: the same result comes back.
        for unchanged in ["by_channel", "count"] {
            assert!(
                Arc::ptr_eq(&fresh.results[unchanged], &narrowed.results[unchanged]),
                "{unchanged} ran again"
            );
        }
        assert_eq!(narrowed.results["by_channel"].rows.len(), 3);

        // The grid's NULL picks the rows with no channel.
        picks.get_mut("channel").unwrap().value = None;
        assert_eq!(total(&load(&path, &picks, &narrowed.outcomes)), "1");

        // A reload reuses nothing: every query runs.
        let reloaded = load(&path, &picks, &HashMap::new());
        assert!(!Arc::ptr_eq(&fresh.results["count"], &reloaded.results["count"]));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_card_value_shrinks_to_fit_rather_than_clip() {
        // Room to spare, or no layout yet: full size.
        assert_eq!(card_value_size("42", Some(300.)), CARD_VALUE_MAX);
        assert_eq!(card_value_size("11,439,704.50", None), CARD_VALUE_MAX);
        // Thirteen characters in 150px: smaller, and it fits.
        let size = card_value_size("11,439,704.50", Some(150.));
        assert!(size < CARD_VALUE_MAX && 13. * 0.6 * size <= 150.);
        // Never below the floor, however narrow.
        assert_eq!(card_value_size("11,439,704.50", Some(20.)), CARD_VALUE_MIN);
    }

    #[test]
    fn plots_fill_rows_and_wrap() {
        // Four cards, then two halves, then a full row, then a lone third
        // that no longer fits beside an eight.
        assert_eq!(
            grid_rows([3, 3, 3, 3, 6, 6, 12, 8, 6]),
            vec![vec![0, 1, 2, 3], vec![4, 5], vec![6], vec![7], vec![8]]
        );
        // Existing specs: every plot the full width, one per row.
        assert_eq!(grid_rows([12, 12]), vec![vec![0], vec![1]]);
        assert!(grid_rows([]).is_empty());
    }
}
