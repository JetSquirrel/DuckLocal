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

use super::presence::{self, Presence};
use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::base::{ResizeHandleContext, ResizeHandleState};
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::chart::AreaChart;
use gpui_kit::component::input::{CompletionProvider, Editor, EditorState, Rope, RopeExt, TabSize};
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::resizable::{resizable_panel, resize_handle_appearance, v_resizable};
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

use super::format::{full_text, short_text, NumberFormat};
use crate::i18n::{tr, trf};
use crate::query::{ColumnKind, QueryOutcome, QueryResult};
use crate::spec::comment_card::CommentCard;
use crate::spec::comments::{self, Author, Comments};
use crate::spec::complete::{self, CompletionKind};
use crate::spec::filter::Pick;
use crate::spec::model::{self, Spec};
use crate::spec::plot::{x_label_count, GroupedBars, SeriesPlot};
use crate::spec::prepare::{keep_series_order, prepare, Mood, PlotPoint, PreparedPlot};
use crate::spec::watch::{Debounce, FileStamp, POLL_INTERVAL};
use crate::ui::chart::{legend_row, map_notes, picking_legend_row, pie_parts, PieLook};
use crate::ui::completion::starts_with_ignore_case;
use crate::ui::geo::GeoPlot;
use crate::ui::results::{fit_column_width, sorted_rows};

/// The plot panels' default and drag bounds, in the spirit of the workspace's
/// own results split.
const PLOT_DEFAULT: f32 = 300.;
/// A pie ring's outer radius, as a share of its box's shorter side: room
/// left for the slice that lifts on hover.
const PIE_RADIUS: f32 = 0.44;
const PLOT_MIN: f32 = 160.;
const PLOT_MAX: f32 = 800.;
/// A row of nothing but cards holds one number each: it starts short.
const CARD_DEFAULT: f32 = 104.;
/// A card with a note under its value needs a line more.
const CARD_NOTED: f32 = 124.;
const CARD_MIN: f32 = 80.;
const CARD_MAX: f32 = 400.;
/// A row of nothing but text panels heads what follows: a title and a line.
const TEXT_DEFAULT: f32 = 76.;
/// A text panel's measures at the default interface size, for sizing its row
/// to its words: padding and gaps, the title's line, one line of words, and
/// one character's average width at the words' size.
const TEXT_PADDING: f32 = 30.;
const TEXT_TITLE: f32 = 34.;
const TEXT_LINE: f32 = 24.;
const TEXT_CHAR: f32 = 8.5;
/// The width a text panel is taken to have before the first layout.
const TEXT_ROOM_GUESS: f32 = 900.;
const TEXT_MIN: f32 = 40.;
/// The space between two plots' cards, and between the cards and the tab's
/// edges.
const PAGE_GAP: f32 = 8.;
/// A plot card's corner radius.
const PANEL_RADIUS: f32 = 8.;

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
    /// The slice each pie's key has picked out, by plot name. A view of the
    /// pie, not a filter: no query runs again.
    pie_picked: HashMap<String, SharedString>,
    /// Row-selection subscriptions of the pickable tables, by table entity;
    /// dropped with them.
    table_subscriptions: Vec<(EntityId, Subscription)>,
    /// The plots turned over to show their SQL, by plot name.
    sql_shown: HashSet<String>,
    /// The plot blown up to fill the dashboard, by plot name; Esc or its
    /// button puts it back on the grid.
    maximized: Option<String>,
    /// Takes Esc while a plot is blown up.
    focus: FocusHandle,
    /// Each card's value size for this frame, by plot index: the smallest
    /// any card in its grid row needs, so a row of numbers reads as one.
    card_sizes: HashMap<usize, f32>,
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
    /// Agents at work on this dashboard and what their edits changed (see
    /// `presence`).
    presence: Presence,
    /// Whether the timer that ends quiet sessions and counts how long the
    /// changed plots have been in view is running.
    presence_ticking: bool,
    _presence_tick: Option<Task<()>>,
    /// Where each plot with a change mark was last drawn, to tell whether
    /// the person can see it.
    card_bounds: HashMap<String, Rc<Cell<Option<Bounds<Pixels>>>>>,
    /// When the plots were last painted, and whether the window had focus
    /// then: a hidden tab or an unfocused window shows nobody anything.
    painted: Rc<Cell<Option<Instant>>>,
    window_active: Rc<Cell<bool>>,
    /// The run in flight was started by an edit on disk, whose changed plots
    /// are marked when it lands.
    external_reload: bool,
    /// Each plot's definition as last drawn — its block and its query's SQL —
    /// to tell which plots an edit on disk changed.
    plot_prints: HashMap<String, String>,
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
            pie_picked: HashMap::new(),
            table_subscriptions: Vec::new(),
            sql_shown: HashSet::new(),
            maximized: None,
            focus: cx.focus_handle(),
            card_sizes: HashMap::new(),
            comments: Comments::default(),
            comments_error: None,
            comment_open: None,
            comment_cards: HashMap::new(),
            presence: Presence::default(),
            presence_ticking: false,
            _presence_tick: None,
            card_bounds: HashMap::new(),
            painted: Rc::new(Cell::new(None)),
            window_active: Rc::new(Cell::new(false)),
            external_reload: false,
            plot_prints: HashMap::new(),
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
        self.external_reload = true;
        self.reload(cx);
    }

    /// An agent's command said what it is doing to this dashboard.
    pub fn apply_presence(&mut self, update: &presence::Update, cx: &mut Context<Self>) {
        self.presence.apply(update, Instant::now());
        self.keep_presence_fresh(cx);
        cx.notify();
    }

    /// Whether an agent is at work on this dashboard, for the tab strip.
    pub fn has_presence(&self) -> bool {
        self.presence.is_working()
    }

    /// Threads an agent answered that are still open: a question back to the
    /// person, or a request it could not carry out.
    fn waiting_threads(&self) -> usize {
        self.comments
            .threads
            .iter()
            .filter(|thread| {
                !thread.resolved
                    && thread
                        .comments
                        .last()
                        .is_some_and(|comment| comment.author == comments::Author::Agent)
            })
            .count()
    }

    /// End quiet sessions and count the changed plots' time in view, for as
    /// long as there is anything to show.
    fn keep_presence_fresh(&mut self, cx: &mut Context<Self>) {
        if self.presence_ticking || self.presence.is_empty() {
            return;
        }
        self.presence_ticking = true;
        self._presence_tick = Some(cx.spawn(async move |this, cx| loop {
            smol::Timer::after(PRESENCE_TICK).await;
            let more = this.update(cx, |this, cx| {
                let waiting = this.waiting_threads();
                let mut moved = this.presence.tick(Instant::now(), waiting);
                if this.presence.has_changes() {
                    let seen = this.plots_in_view();
                    moved |= this.presence.viewed(&seen, PRESENCE_TICK);
                    // A frame each tick while marks wait to be seen: what is
                    // in view is read off the last one painted.
                    cx.notify();
                }
                if moved {
                    cx.notify();
                }
                this.presence_ticking = !this.presence.is_empty();
                this.presence_ticking
            });
            if !matches!(more, Ok(true)) {
                break;
            }
        }));
    }

    /// The changed plots the person can see now: drawn in the last moment,
    /// in a focused window, and fully inside the scrolled view — or, for a
    /// plot taller than the view, filling it.
    fn plots_in_view(&self) -> Vec<String> {
        let fresh = self
            .painted
            .get()
            .is_some_and(|at| at.elapsed() < PRESENCE_TICK * 2);
        if !fresh || !self.window_active.get() || self.showing_source {
            return Vec::new();
        }
        let Some(view) = self.wheel.viewport().get() else {
            return Vec::new();
        };
        self.card_bounds
            .iter()
            .filter(|(name, _)| self.presence.change(name).is_some())
            .filter(|(_, cell)| {
                cell.get().is_some_and(|card| {
                    let top = card.top().max(view.top());
                    let bottom = card.bottom().min(view.bottom());
                    let shown = bottom - top;
                    shown >= card.size.height.min(view.size.height) * 0.95
                })
            })
            .map(|(name, _)| name.clone())
            .collect()
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
                                same.then(|| this.tables.get(old).cloned().flatten())
                                    .flatten()
                            })
                            .collect::<Vec<_>>();
                        this.table_subscriptions.retain(|(id, _)| {
                            tables
                                .iter()
                                .flatten()
                                .any(|table| table.entity_id() == *id)
                        });
                        this.tables = tables;
                        let titles: HashMap<String, String> = this
                            .plots
                            .iter()
                            .map(|plot| {
                                let title = if plot.title.is_empty() {
                                    plot.name.clone()
                                } else {
                                    plot.title.clone()
                                };
                                (plot.name.clone(), title)
                            })
                            .collect();
                        let mut plots = run.plots;
                        for plot in &mut plots {
                            if let Some(before) = this.plots.iter().find(|p| p.name == plot.name) {
                                keep_series_order(plot, &before.series_names);
                            }
                        }
                        this.plots = plots;
                        this.results = run.results;
                        this.outcomes = run.outcomes;
                        this.spec = run.spec;
                        // An edit on disk: the plots it changed, added and
                        // took out are an agent's, when one has been at work.
                        let prints = run.prints;
                        if std::mem::take(&mut this.external_reload) {
                            let before = &this.plot_prints;
                            let mut changed = Vec::new();
                            let mut added = Vec::new();
                            for (name, print) in &prints {
                                match before.get(name) {
                                    None => added.push(name.clone()),
                                    Some(old) if old != print => changed.push(name.clone()),
                                    Some(_) => {}
                                }
                            }
                            let mut removed: Vec<(String, String)> = before
                                .keys()
                                .filter(|name| !prints.contains_key(*name))
                                .map(|name| {
                                    let title =
                                        titles.get(name).cloned().unwrap_or_else(|| name.clone());
                                    (name.clone(), title)
                                })
                                .collect();
                            changed.sort();
                            added.sort();
                            removed.sort();
                            this.presence
                                .edited(&changed, &added, &removed, Instant::now());
                            this.keep_presence_fresh(cx);
                        }
                        this.plot_prints = prints;
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
        let Some(row) = result.rows.iter().position(|row| {
            row.get(x)
                .is_some_and(|cell| cell.as_str() == band.as_ref())
        }) else {
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
                    .on_click(cx.listener(move |this, _, _, cx| this.clear_picks(Some(&name), cx)))
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
            // The glyph a query tab wears: this plot's query. Back is the
            // chart's own glyph, as the spec view's button has it.
            .icon(if shown {
                AssetIcon::ChartNoAxesColumn
            } else {
                AssetIcon::SquareTerminal
            })
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
        let is_max = self.maximized.as_deref() == Some(name.as_str());
        let maximize = Button::new(("dashboard-maximize", ix))
            .xsmall()
            .ghost()
            .icon(Icon::new(if is_max {
                AssetIcon::Minimize2
            } else {
                AssetIcon::Maximize2
            }))
            .tooltip(if is_max {
                tr("dashboard.restore_plot.tooltip")
            } else {
                tr("dashboard.maximize.tooltip")
            })
            .on_click(cx.listener({
                let name = name.clone();
                move |this, _, window, cx| {
                    this.toggle_maximized(&name, window, cx);
                }
            }));
        // A text panel runs no query: there is no SQL to show.
        let has_sql = self.plots[ix].kind != "text";
        let comments = self.render_comment_button(ix, window, cx);
        let open = self.comments.open_on(&name);
        let commenting = self.comment_open.as_deref() == Some(name.as_str());
        let hover_only = |this: Div, keep: bool| {
            if keep {
                this
            } else {
                this.opacity(0.)
                    .group_hover(group.clone(), |style| style.opacity(1.))
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
                    .when(has_sql, |this| {
                        this.child(
                            hover_only(div(), shown)
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().background)
                                .child(toggle),
                        )
                    })
                    .child(
                        hover_only(div(), is_max)
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().background)
                            .child(maximize),
                    ),
            )
            .into_any_element()
    }

    /// Blow plot `name` up to fill the dashboard, or put it back.
    fn toggle_maximized(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.maximized.as_deref() == Some(name) {
            self.maximized = None;
        } else {
            self.maximized = Some(name.to_string());
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// The blown-up plot's index; `None` once a reload drops it.
    fn maximized_ix(&self) -> Option<usize> {
        let name = self.maximized.as_deref()?;
        self.plots.iter().position(|plot| plot.name == name)
    }

    /// The blown-up plot alone, as a card filling the dashboard.
    fn render_maximized(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.plots[ix].kind == "card" {
            let room = self
                .wheel
                .viewport()
                .get()
                .map(|viewport| viewport.size.width.as_f32() - CARD_PADDING - PAGE_GAP);
            let size = card_value_size(self.plots[ix].card.as_deref().unwrap_or_default(), room);
            self.card_sizes.insert(ix, size);
        }
        let boxed = self.plots[ix].kind != "text";
        div()
            .size_full()
            .p(px(PAGE_GAP))
            .child(
                div()
                    .size_full()
                    .relative()
                    .overflow_hidden()
                    .when(boxed, |this| {
                        this.bg(crate::ui::palette::panel(cx))
                            .border_1()
                            .border_color(cx.theme().border)
                            .rounded(px(PANEL_RADIUS))
                    })
                    .child(self.render_plot(ix, window, cx))
                    .children(self.render_plot_presence(ix, cx)),
            )
            .into_any_element()
    }

    /// The font size at which card `ix`'s value fits its share of the
    /// dashboard's last laid-out width, less its padding.
    fn card_value_size(&self, ix: usize) -> f32 {
        let plot = &self.plots[ix];
        let room = self.wheel.viewport().get().map(|viewport| {
            viewport.size.width.as_f32() * plot.width as f32 / model::GRID_COLUMNS as f32
                - CARD_PADDING
        });
        card_value_size(plot.card.as_deref().unwrap_or_default(), room)
    }

    /// The height a row of text panels needs for its title and words at the
    /// width each has and the interface size in use: a fixed height clipped
    /// a second line, and every line at a larger size.
    fn text_row_height(&self, row: &[usize]) -> f32 {
        let scale = f32::from(crate::ui::scale::design(1.));
        let viewport = self.wheel.viewport().get().map(|v| v.size.width.as_f32());
        row.iter()
            .map(|&ix| {
                let plot = &self.plots[ix];
                let room = viewport.map_or(TEXT_ROOM_GUESS, |width| {
                    width * plot.width as f32 / model::GRID_COLUMNS as f32
                        - 2. * (CARD_INSET + 1.)
                        - PAGE_GAP
                });
                let per_line = (room / (TEXT_CHAR * scale)).max(8.);
                let lines: f32 = plot
                    .text
                    .as_deref()
                    .unwrap_or_default()
                    .lines()
                    .map(|line| {
                        let width = unicode_width::UnicodeWidthStr::width(line) as f32;
                        (width / per_line).ceil().max(1.)
                    })
                    .sum();
                let title = if plot.title.is_empty() {
                    0.
                } else {
                    TEXT_TITLE
                };
                (TEXT_PADDING + title + lines * TEXT_LINE) * scale
            })
            .fold(TEXT_DEFAULT, f32::max)
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
            .map(|this| {
                if open > 0 {
                    this.outline()
                } else {
                    this.ghost()
                }
            })
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
        let styles = super::highlight::sql_styles(&sql, &*cx.theme().highlight_theme);
        let copied = sql.clone();
        let opened = sql.clone();
        let tab_title = title.clone();
        // Headed as the plot's face is, so flipping to the SQL does not move
        // the title.
        v_flex()
            .size_full()
            .px(px(CARD_INSET))
            .pt_4()
            .pb(px(CARD_INSET))
            .gap_2()
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::MEDIUM)
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
                    .child(StyledText::new(sql).with_highlights(styles)),
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
            let size = self
                .card_sizes
                .get(&ix)
                .copied()
                .unwrap_or_else(|| self.card_value_size(ix));
            return render_card(&self.plots[ix], size, cx);
        }
        if self.plots[ix].kind == "text" {
            return render_text(&self.plots[ix], cx);
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
                Some(notice) => crate::i18n::join(&notice, &hint),
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
                            (Some(a), Some(b)) => Some(crate::i18n::join(&a, &b)),
                            (a, b) => a.or(b),
                        };
                        legend = key;
                    }
                    plot.label_name.clone()
                }
                "pie" => {
                    legend = pie_parts(
                        &plot.pie,
                        series.clone().into(),
                        plot.format,
                        PieLook::default(),
                        "dashboard-pie-key",
                        cx,
                    )
                    .1;
                    trf("chart.pie.title", &[&series, &plot.label_name])
                }
                _ => {
                    // Several series need a key; one is named in the title.
                    if plot.series_names.len() > 1 {
                        legend = plot
                            .series_names
                            .iter()
                            .enumerate()
                            .map(|(s, name)| {
                                (
                                    crate::ui::palette::color(s, cx),
                                    SharedString::from(name.clone()),
                                )
                            })
                            .collect();
                    }
                    trf("chart.title.by", &[&series, &plot.label_name])
                }
            }
        });
        // One muted line under the title: what is plotted, then what a click
        // does. A key that already names the series says the first half
        // better, so a keyed line or bar plot leaves it out.
        let keyed_xy = !legend.is_empty() && !matches!(self.plots[ix].kind.as_str(), "pie" | "map");
        let subtitle = match (axes.filter(|_| !keyed_xy), notice) {
            (Some(axes), Some(notice)) => Some(crate::i18n::join(&axes, &notice)),
            (axes, notice) => axes.or(notice),
        };

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
                (
                    ElementId::from("dashboard-chart"),
                    self.plots[ix].name.clone(),
                ),
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
        } else if self.plots[ix].kind == "pie" {
            self.render_pie(ix, cx)
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
                    .px(px(CARD_INSET))
                    .pt_4()
                    .pb_2()
                    .gap_1()
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::MEDIUM)
                            .text_ellipsis()
                            // Clear of the SQL and comment buttons.
                            .pr_16()
                            .child(title),
                    )
                    .when_some(subtitle, |this, subtitle| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .text_ellipsis()
                                .child(subtitle),
                        )
                    })
                    .when(!legend.is_empty(), |this| {
                        let plot = &self.plots[ix];
                        if plot.kind != "pie" {
                            return this.child(legend_row(legend, cx));
                        }
                        // A pie's key picks a slice out; the same entry again
                        // puts the whole back.
                        let name = plot.name.clone();
                        let keyed = legend
                            .into_iter()
                            .zip(&plot.pie)
                            .map(|((color, label), slice)| (color, label, slice.name().clone()))
                            .collect();
                        let view = cx.entity().downgrade();
                        this.child(picking_legend_row(
                            ("dashboard-pie-key", ix),
                            keyed,
                            self.pie_picked.get(&name),
                            move |key, _, cx| {
                                let name = name.clone();
                                view.update(cx, |this, cx| {
                                    if this.pie_picked.get(&name) == Some(&key) {
                                        this.pie_picked.remove(&name);
                                    } else {
                                        this.pie_picked.insert(name, key);
                                    }
                                    cx.notify();
                                })
                                .ok();
                            },
                            cx,
                        ))
                    }),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    /// A pie as a ring sized to its panel, with the total — or the slice its
    /// key picked out — in the middle. The panel's size is known only once it
    /// is laid out, so the ring takes the last frame's, and a frame that finds
    /// it changed asks for one more.
    fn render_pie(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let plot = &self.plots[ix];
        let bounds = self
            .plot_bounds
            .entry(plot.name.clone())
            .or_default()
            .clone();
        let picked = self.pie_picked.get(&plot.name).cloned();
        let outer = bounds
            .get()
            .map(|b| b.size.width.min(b.size.height).as_f32() * PIE_RADIUS);
        let series = plot.series_names.join(", ");
        let (chart, _, total) = pie_parts(
            &plot.pie,
            series.into(),
            plot.format,
            PieLook {
                outer_radius: outer,
                isolated: picked.clone(),
            },
            (ElementId::from("dashboard-chart"), plot.name.clone()),
            cx,
        );
        let slice = picked.and_then(|name| plot.pie.iter().find(|s| *s.name() == name));
        let (value, caption) = match slice {
            Some(slice) => (
                short_text(plot.format, slice.amount()),
                format!("{} · {:.1}%", slice.name(), slice.amount() / total * 100.),
            ),
            None => (
                short_text(plot.format, total),
                tr("chart.pie.center_total").to_string(),
            ),
        };
        let muted = cx.theme().muted_foreground;
        div()
            .size_full()
            .px_2()
            .pb_2()
            .child(
                div()
                    .size_full()
                    .relative()
                    .child(chart)
                    .when(outer.is_some(), |this| {
                        this.child(
                            v_flex()
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(value),
                                )
                                .child(div().text_xs().text_color(muted).child(caption)),
                        )
                    })
                    .child(
                        canvas(
                            move |b, window, _| {
                                if bounds.get() != Some(b) {
                                    bounds.set(Some(b));
                                    window.refresh();
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    ),
            )
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
                self.table_subscriptions
                    .push((table.entity_id(), subscription));
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
            self.table_subscriptions
                .push((table.entity_id(), subscription));
            self.tables[ix] = Some(table);
        }
        let table = self.tables[ix].clone().expect("built just above");
        let truncated = result.truncated;
        let row_count = result.rows.len();
        let rows = table
            .read(cx)
            .vertical_scroll_handle
            .0
            .borrow()
            .base_handle
            .clone();
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
                            .scrollbar_visible(true, false),
                    )
                    .child(crate::ui::results::horizontal_scrollbar(&table, cx))
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
                        state.lsp_mut().completion_provider = Some(Rc::new(SpecCompletionProvider));
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
    /// An agent's mark on plot `ix`: a ring in the agent colour over the card
    /// — breathing while it says it is about to edit the plot, steady once
    /// its edit has changed or added it, until the person has seen it.
    fn render_plot_presence(&self, ix: usize, cx: &App) -> Option<AnyElement> {
        let name = &self.plots[ix].name;
        let color = crate::ui::palette::presence(cx);
        let ring = div()
            .absolute()
            .inset_0()
            .rounded(px(PANEL_RADIUS))
            .border_2()
            .border_color(color);
        if self.presence.change(name).is_some() {
            return Some(ring.bg(color.opacity(0.05)).into_any_element());
        }
        self.presence.editing(name)?;
        Some(
            ring.with_animation(
                SharedString::from(format!("presence-ring-{}-{name}", self.id)),
                pulse(true),
                |ring, delta| ring.opacity(delta),
            )
            .into_any_element(),
        )
    }

    /// The name tag of an agent's mark: on the card's top edge, half over the
    /// gap above it, so it never covers the plot's own title.
    fn render_presence_tag(&self, ix: usize, cx: &App) -> Option<AnyElement> {
        let name = &self.plots[ix].name;
        let (text, ongoing) = match self.presence.change(name) {
            Some(mark) => {
                let key = match mark.change {
                    presence::Change::Changed => "presence.changed",
                    presence::Change::Added => "presence.added",
                };
                (trf(key, &[&mark.agent]), false)
            }
            None => (
                trf("presence.editing", &[self.presence.editing(name)?]),
                true,
            ),
        };
        Some(
            presence_tag(crate::ui::palette::presence(cx))
                .absolute()
                .top(px(-PRESENCE_TAG_RISE))
                .left(px(PAGE_GAP / 2. + 10.))
                .child(presence_dot(
                    format!("presence-dot-{}-{name}-{ongoing}", self.id).into(),
                    ongoing,
                ))
                .child(text)
                .into_any_element(),
        )
    }

    /// Records where a marked plot is drawn, for `plots_in_view`.
    fn record_card_bounds(&mut self, ix: usize) -> Option<AnyElement> {
        let name = &self.plots[ix].name;
        self.presence.change(name)?;
        let cell = self.card_bounds.entry(name.clone()).or_default().clone();
        Some(
            canvas(move |bounds, _, _| cell.set(Some(bounds)), |_, _, _, _| {})
                .absolute()
                .inset_0()
                .into_any_element(),
        )
    }

    /// The plots an agent said it is about to add, held a place for below
    /// the last row until its edit lands.
    fn render_adding(&self, cx: &App) -> Option<AnyElement> {
        let names: Vec<String> = self.plots.iter().map(|plot| plot.name.clone()).collect();
        let adding = self.presence.adding(&names);
        if adding.is_empty() {
            return None;
        }
        let color = crate::ui::palette::presence(cx);
        Some(
            v_flex()
                .gap(px(PAGE_GAP))
                .px(px(PAGE_GAP))
                .pb(px(PAGE_GAP))
                .children(adding.into_iter().map(|(plot, agent)| {
                    h_flex()
                        .h(px(ADDING_HEIGHT))
                        .justify_center()
                        .items_center()
                        .gap_1p5()
                        .rounded(px(PANEL_RADIUS))
                        .border_2()
                        .border_color(color.opacity(0.6))
                        .bg(color.opacity(0.04))
                        .text_sm()
                        .text_color(color)
                        .child(presence_dot_in(
                            format!("presence-adding-{}-{plot}", self.id).into(),
                            color,
                            true,
                        ))
                        .child(trf("presence.adding", &[&agent, &plot]))
                }))
                .into_any_element(),
        )
    }

    /// Plots an agent's edit took out: one line above the dashboard until the
    /// person dismisses it.
    fn render_removed(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let removed = self.presence.removed();
        if removed.is_empty() {
            return None;
        }
        let color = crate::ui::palette::presence(cx);
        let mut lines: Vec<String> = Vec::new();
        for agent in removed.iter().map(|r| r.agent.as_str()) {
            if lines.iter().any(|line| line.starts_with(agent)) {
                continue;
            }
            let titles: Vec<String> = removed
                .iter()
                .filter(|r| r.agent == agent)
                .map(|r| format!("\u{201c}{}\u{201d}", r.title))
                .collect();
            lines.push(trf(
                "presence.removed",
                &[agent, &titles.join(tr("presence.list_separator"))],
            ));
        }
        Some(
            h_flex()
                .flex_none()
                .gap_2()
                .px_3()
                .py_1p5()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(color.opacity(0.06))
                .text_sm()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .children(lines.into_iter().map(|line| div().child(line))),
                )
                .child(
                    Button::new(("dashboard-removed-dismiss", self.id as usize))
                        .xsmall()
                        .ghost()
                        .label(tr("presence.dismiss"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.presence.dismiss_removed();
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    /// Who is at work on the dashboard: one name each in the top corner,
    /// breathing while it works, steady once done, red while the dashboard
    /// shows an error. A click opens what it did.
    fn render_presence_names(&self, cx: &App) -> Option<AnyElement> {
        let sessions = self.presence.sessions();
        if sessions.is_empty() {
            return None;
        }
        let failing =
            self.spec_error.is_some() || self.plots.iter().any(|plot| plot.failure.is_some());
        let waiting = self.waiting_threads();
        let titles: HashMap<String, String> = self
            .plots
            .iter()
            .map(|plot| {
                let title = if plot.title.is_empty() {
                    plot.name.clone()
                } else {
                    plot.title.clone()
                };
                (plot.name.clone(), title)
            })
            .collect();
        let last = sessions.len() - 1;
        let pills = sessions.iter().enumerate().map(|(ix, session)| {
            let working = session.is_working();
            let color = if failing {
                cx.theme().danger
            } else if working {
                crate::ui::palette::presence(cx)
            } else {
                cx.theme().success
            };
            let state = if failing {
                tr("presence.has_errors")
            } else if working {
                tr("presence.working")
            } else {
                tr("presence.done")
            };
            let lines: Vec<(String, String)> = session
                .log
                .iter()
                .map(|entry| {
                    (
                        entry.at.format("%H:%M").to_string(),
                        log_line(&entry.did, &titles),
                    )
                })
                .collect();
            let header = trf("presence.session", &[&session.agent]);
            let shows_waiting = ix == last && waiting > 0;
            let trigger = Button::new(SharedString::from(format!(
                "presence-name-{}-{}",
                self.id, session.agent
            )))
            .xsmall()
            .outline()
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(crate::ui::agent_avatar::agent_avatar(
                        &session.agent,
                        px(16.),
                        cx,
                    ))
                    .child(session.agent.clone())
                    .child(presence_dot_in(
                        format!("presence-name-dot-{}-{}-{working}", self.id, session.agent).into(),
                        color,
                        working && !failing,
                    )),
            );
            let popover = Popover::new(SharedString::from(format!(
                "presence-log-{}-{}",
                self.id, session.agent
            )))
            .anchor(Anchor::TopRight)
            .trigger(trigger)
            .content(move |_, _, cx| {
                v_flex()
                    .w(px(280.))
                    .gap_1()
                    .text_sm()
                    .child(
                        h_flex()
                            .justify_between()
                            .font_weight(FontWeight::MEDIUM)
                            .child(header.clone())
                            .child(div().text_color(color).child(state)),
                    )
                    .children(lines.iter().map(|(at, text)| {
                        h_flex()
                            .gap_2()
                            .items_start()
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(40.))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(at.clone()),
                            )
                            .child(div().min_w_0().child(text.clone()))
                    }))
                    .when(shows_waiting, |this| {
                        this.child(
                            div()
                                .pt_1()
                                .text_color(cx.theme().muted_foreground)
                                .child(trf("presence.waiting", &[&waiting.to_string()])),
                        )
                    })
            });
            div().relative().child(popover).when(shows_waiting, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(-5.))
                        .right(px(-5.))
                        .min_w(px(15.))
                        .h(px(15.))
                        .px(px(3.))
                        .rounded_full()
                        .bg(crate::ui::palette::presence(cx))
                        .text_color(gpui_kit::white())
                        .text_size(px(10.))
                        .font_weight(FontWeight::BOLD)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(waiting.to_string()),
                )
            })
        });
        Some(
            h_flex()
                .absolute()
                .top_2()
                .right_3()
                .gap_2()
                .children(pills)
                .into_any_element(),
        )
    }

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
                Icon::new(AssetIcon::ChartNoAxesColumn)
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
        self.window_active.set(window.is_window_active());
        let body = if self.showing_source {
            // The source view stands on its own, spec error or not: the text
            // of a broken spec is exactly what one opens the source to fix.
            self.render_source(window, cx)
        } else if let Some(error) = self.spec_error.clone() {
            self.render_spec_failure(error, cx)
        } else if let Some(ix) = self.maximized_ix() {
            self.render_maximized(ix, window, cx)
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
            // Cards side by side share one value size: one shrunk alone left
            // its number smaller and its label lower than its neighbours'.
            self.card_sizes.clear();
            for row in &rows {
                let cards: Vec<usize> = row
                    .iter()
                    .copied()
                    .filter(|&ix| self.plots[ix].kind == "card")
                    .collect();
                let size = cards
                    .iter()
                    .map(|&ix| self.card_value_size(ix))
                    .fold(CARD_VALUE_MAX, f32::min);
                self.card_sizes
                    .extend(cards.into_iter().map(|ix| (ix, size)));
            }
            let mut stack = 0.;
            let panels = rows
                .iter()
                .map(|row| {
                    let all = |kind: &str| row.iter().all(|&ix| self.plots[ix].kind == kind);
                    // A heading row with a card beside it is a row of cards.
                    let cards = row
                        .iter()
                        .all(|&ix| matches!(self.plots[ix].kind.as_str(), "card" | "text"));
                    let (default, min, max) = if all("text") {
                        (self.text_row_height(row), TEXT_MIN, CARD_MAX)
                    } else if cards {
                        let noted = row.iter().any(|&ix| {
                            self.plots[ix].note.is_some() || self.plots[ix].delta.is_some()
                        });
                        let default = if noted { CARD_NOTED } else { CARD_DEFAULT };
                        (default, CARD_MIN, CARD_MAX)
                    } else if let Some(height) = self.table_row_height(row) {
                        // A row of short tables is as tall as its rows, not
                        // a chart's height of empty space under them.
                        (height, TABLE_MIN.min(height), PLOT_MAX)
                    } else {
                        (PLOT_DEFAULT, PLOT_MIN, PLOT_MAX)
                    };
                    stack += default;
                    // A row short of twelve columns leaves the rest empty,
                    // so a plot's width means the same on every row. The
                    // gutter is padding inside each share, not a gap
                    // between them, so the shares still sum to the row.
                    let text_row = all("text");
                    let cells = row.iter().map(|&ix| {
                        let share = self.plots[ix].width as f32 / model::GRID_COLUMNS as f32;
                        // A text panel heads the page, on the page itself;
                        // every other plot is a card of its own.
                        let boxed = self.plots[ix].kind != "text";
                        div()
                            .h_full()
                            .w(relative(share))
                            .min_w_0()
                            .relative()
                            .px(px(PAGE_GAP / 2.))
                            .child(
                                div()
                                    .size_full()
                                    .relative()
                                    .overflow_hidden()
                                    .when(boxed, |this| {
                                        this.bg(crate::ui::palette::panel(cx))
                                            .border_1()
                                            .border_color(cx.theme().border)
                                            .rounded(px(PANEL_RADIUS))
                                    })
                                    .child(self.render_plot(ix, window, cx))
                                    .children(self.render_plot_presence(ix, cx))
                                    .children(self.record_card_bounds(ix)),
                            )
                            .children(self.render_presence_tag(ix, cx))
                    });
                    let cells = cells.collect::<Vec<_>>();
                    let default = default + PAGE_GAP;
                    stack += PAGE_GAP;
                    resizable_panel()
                        .size(px(default))
                        .size_range(px(min + PAGE_GAP)..px(max + PAGE_GAP))
                        .child(
                            h_flex()
                                .size_full()
                                .px(px(PAGE_GAP / 2.))
                                .py(px(PAGE_GAP / 2.))
                                .when(text_row, |this| this.pb_0())
                                .children(cells),
                        )
                })
                .collect::<Vec<_>>();
            // Every row keeps its default height and the tab scrolls, rather
            // than a dozen plots squeezed into one screen and the rest clipped
            // out of reach. Dragging a divider still trades height between
            // neighbours. A tab taller than the stack is not stretched to fill:
            // the group scales every row by the same factor, so a row of cards
            // would grow as much as a chart, all of it empty space. The rest
            // of the tab stays below the last row, as on a page.
            let stack = px(stack);
            let wheel = self.wheel.clone();
            let viewport = self.wheel.viewport();
            let painted = self.painted.clone();
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
                            v_flex()
                                .w_full()
                                .child(
                                    div().w_full().h(stack).child(
                                        v_resizable(format!("dashboard-{}", self.id))
                                            // The page gap already parts the rows: a
                                            // line across it at rest would cut the page
                                            // in strips. Under the pointer the handle
                                            // shows itself as everywhere else.
                                            .with_handle_appearance({
                                                let shown = resize_handle_appearance();
                                                Rc::new(
                                            move |handle: &ResizeHandleContext,
                                                  window: &mut Window,
                                                  cx: &mut App| {
                                                if handle.state() == ResizeHandleState::Idle {
                                                    Some(div().into_any_element())
                                                } else {
                                                    shown(handle, window, cx)
                                                }
                                            },
                                        )
                                            })
                                            .children(panels),
                                    ),
                                )
                                .children(self.render_adding(cx)),
                        ),
                )
                .child(
                    canvas(
                        move |b, _, _| viewport.set(Some(b)),
                        move |_, _, _, _| painted.set(Some(Instant::now())),
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
                )
                .into_any_element()
        };

        v_flex()
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && this.maximized.take().is_some() {
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .bg(crate::ui::palette::page(cx))
            .text_color(cx.theme().foreground)
            .children(self.render_conflict_warning(cx))
            .children(self.render_stale_warning(cx))
            .when(!self.showing_source, |this| {
                this.children(self.render_removed(cx))
            })
            .when(!self.showing_source, |this| {
                this.children(self.render_picks(cx))
            })
            .child(body)
            .children(self.render_presence_names(cx))
            .into_any_element()
    }
}

/// Each plot's definition as text — its block as written and its query's
/// SQL — so two runs can tell which plots an edit changed. Where a block sits
/// in the file is no part of it: an edit above it moves it, not changes it.
fn plot_prints(spec: &Spec, source: &str) -> HashMap<String, String> {
    // A block runs from its keyword to the next block's: the spans the model
    // keeps mark only where each starts.
    let mut starts: Vec<usize> = spec
        .sources
        .iter()
        // A source keeps only its name's place, a word into the block:
        // near enough to end the block before it.
        .map(|block| block.name_span.0)
        .chain(spec.queries.iter().map(|block| block.span.0))
        .chain(spec.plots.iter().map(|block| block.span.0))
        .chain(spec.filters.iter().map(|block| block.span.0))
        .collect();
    starts.sort_unstable();
    spec.plots
        .iter()
        .map(|plot| {
            let start = plot.span.0;
            let end = starts
                .iter()
                .copied()
                .find(|&next| next > start)
                .unwrap_or(source.len());
            let block = source.get(start..end).unwrap_or_default().trim_end();
            let sql = spec
                .queries
                .iter()
                .find(|query| query.name == plot.query)
                .map(|query| query.sql.as_str())
                .unwrap_or_default();
            // Whitespace aside: an editor that rewrites line endings or
            // re-indents the file has changed no plot.
            let text = format!("{block}\n{sql}");
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            (plot.name.clone(), text)
        })
        .collect()
}

/// How far a presence name tag rises above its card's top edge, into the
/// gap between rows.
const PRESENCE_TAG_RISE: f32 = 8.;

/// How often presence is brought up to date: quiet sessions ended, and the
/// changed plots' time in view counted.
const PRESENCE_TICK: std::time::Duration = std::time::Duration::from_millis(250);

/// The height of the place held for a plot an agent is about to add.
const ADDING_HEIGHT: f32 = 56.;

/// A presence name tag's pill, in the agent colour.
fn presence_tag(color: Hsla) -> Div {
    h_flex()
        .gap_1()
        .items_center()
        .px_1p5()
        .rounded(px(4.))
        .bg(color)
        .text_color(gpui_kit::white())
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
}

/// One line of a session's log, plots by their titles.
fn log_line(did: &presence::Did, titles: &HashMap<String, String>) -> String {
    let named = |plots: &[String]| {
        plots
            .iter()
            .map(|plot| format!("\u{201c}{}\u{201d}", titles.get(plot).unwrap_or(plot)))
            .collect::<Vec<_>>()
            .join(tr("presence.list_separator"))
    };
    match did {
        presence::Did::Read(plots) => trf("presence.log.read", &[&named(plots)]),
        presence::Did::Checked => tr("presence.log.checked").to_string(),
        presence::Did::Replied(plots) => trf("presence.log.replied", &[&named(plots)]),
        presence::Did::Edited {
            changed,
            added,
            removed,
        } => {
            let parts = [
                ("presence.log.changed", *changed),
                ("presence.log.added", *added),
                ("presence.log.removed", *removed),
            ]
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .map(|(key, count)| trf(key, &[&count.to_string()]))
            .collect::<Vec<_>>()
            .join(tr("presence.list_separator"));
            // The parts read as one sentence, which starts with a capital.
            let mut chars = parts.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}

/// A dot in `color`: breathing while an agent is at work, steady after.
fn presence_dot_in(id: SharedString, color: Hsla, ongoing: bool) -> impl IntoElement {
    div()
        .size_2()
        .rounded_full()
        .bg(color)
        .with_animation(id, pulse(ongoing), |dot, delta| dot.opacity(delta))
}

/// A presence mark's dot: breathing while the work goes on, steady once done.
fn presence_dot(id: SharedString, ongoing: bool) -> impl IntoElement {
    div()
        .size_1p5()
        .rounded_full()
        .bg(gpui_kit::white())
        .with_animation(id, pulse(ongoing), |dot, delta| dot.opacity(delta))
}

/// Opacity between faint and full and back, a breath every 1.2s; or full,
/// held, for a mark of work already done.
fn pulse(ongoing: bool) -> Animation {
    if ongoing {
        Animation::new(std::time::Duration::from_millis(1200))
            .repeat()
            .with_easing(|t| 0.35 + 0.65 * (0.5 - 0.5 * (t * std::f32::consts::TAU).cos()))
    } else {
        Animation::new(std::time::Duration::from_millis(1)).with_easing(|_| 1.)
    }
}

/// Everything one run of a dashboard produces, computed off the UI thread.
struct Run {
    spec_error: Option<String>,
    spec: Option<Arc<Spec>>,
    /// Each plot's definition as text (see `plot_prints`).
    prints: HashMap<String, String>,
    plots: Vec<PreparedPlot>,
    results: HashMap<String, Arc<QueryResult>>,
    outcomes: Outcomes,
}

/// Read, parse, validate, run, prepare. Blocking — call it off the UI thread.
/// `picks` narrow the queries that read their filters; a query whose SQL
/// comes out as `reuse` last ran it keeps that outcome instead of running.
fn load(path: &Path, picks: &HashMap<String, Pick>, reuse: &Outcomes) -> Run {
    let display = path.display().to_string();
    let spec = (|| -> Result<(Spec, String), String> {
        let source = std::fs::read_to_string(path).map_err(|e| format!("{display}: {e}"))?;
        let file =
            model::parse(&source).map_err(|e| format!("{display}:{}: {}", e.line, e.message))?;
        let spec = model::validate(&file).map_err(|diagnostics| {
            diagnostics
                .iter()
                .map(|d| format!("{display}:{}: {}", d.line, d.message))
                .collect::<Vec<_>>()
                .join("\n")
        })?;
        Ok((spec, source))
    })();
    let (spec, prints) = match spec {
        Ok((spec, source)) => {
            let prints = plot_prints(&spec, &source);
            (spec, prints)
        }
        Err(spec_error) => {
            return Run {
                spec_error: Some(spec_error),
                spec: None,
                prints: HashMap::new(),
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
    let search_path = super::source::search_path(&base);
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
                    let outcome = match crate::query::run_searching(search_path.as_deref(), &sql) {
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
            outcome
                .as_ref()
                .ok()
                .map(|result| (name.clone(), result.clone()))
        })
        .collect();

    Run {
        spec_error: None,
        spec: Some(Arc::new(spec)),
        prints,
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

/// An area chart's value axis in the plot's `format`, each tick on its own;
/// unformatted, the chart's own labels.
fn with_value_axis<T: 'static>(
    chart: AreaChart<T, SharedString, f64>,
    format: Option<NumberFormat>,
) -> AreaChart<T, SharedString, f64> {
    match format {
        Some(format) => chart.y_tick_format(move |v| format.ticks(&[v]).remove(0)),
        None => chart,
    }
}

/// One plot's chart element, built from the prepared data on each frame;
/// cloning the `Arc`s is a refcount bump, not a copy of the row set.
fn chart_element(plot_ix: usize, plot: &PreparedPlot, cx: &App) -> AnyElement {
    let palette = crate::ui::palette::series(cx);
    let format = plot.format;
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
        "pie" => {
            pie_parts(
                &plot.pie,
                name.into(),
                plot.format,
                PieLook::default(),
                named_id(),
                cx,
            )
            .0
        }
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
            let chart = AreaChart::new(plot.points.clone())
                .x(|d: &Arc<PlotPoint>| d.band.clone())
                .id(id)
                .y(|d: &Arc<PlotPoint>| d.values[0])
                .stroke(color)
                .fill(linear_gradient(
                    0.,
                    linear_color_stop(color.opacity(0.4), 1.),
                    linear_color_stop(color.opacity(0.05), 0.),
                ))
                .linear()
                .name(name)
                .x_tick_count(x_labels)
                .tooltip_value(move |_: &Arc<PlotPoint>, _: usize, v: f64| {
                    full_text(format, v).into()
                });
            with_value_axis(chart, format).into_any_element()
        }
        // A scatter's points are not connected, and its x is the band axis
        // `prepare` built — the hand-built plot draws it that way, one series
        // or many.
        "scatter" => SeriesPlot::scatter(named_id(), plot).into_any_element(),
        // The catalog's one multi-series chart fills; filling is what `area`
        // asks for.
        "area" => {
            let mut chart = AreaChart::new(plot.points.clone())
                .x(|d: &Arc<PlotPoint>| d.band.clone())
                .linear()
                .id(id)
                .x_tick_count(x_labels)
                .tooltip_value(move |_: &Arc<PlotPoint>, _: usize, v: f64| {
                    full_text(format, v).into()
                });
            chart = with_value_axis(chart, format);
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
        // A line, one series or several, draws lines and no fill, straight
        // between points and over a labelled value axis.
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
/// A card's inset from its border, each side.
const CARD_INSET: f32 = 20.;
/// What a card's share of the width loses before its value: the inset and
/// border on both sides, and the page gap around the card.
const CARD_PADDING: f32 = 2. * (CARD_INSET + 1.) + PAGE_GAP;
/// A card value's font size: as large as this, and never smaller than the
/// floor — past that, a value that still does not fit is cut off.
const CARD_VALUE_MAX: f32 = 32.;
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

fn render_card(plot: &PreparedPlot, size: f32, cx: &App) -> AnyElement {
    let value = plot.card.clone().unwrap_or_default();
    let tone = plot
        .tone
        .as_deref()
        .and_then(|name| crate::ui::palette::tone(name, cx));
    v_flex()
        .size_full()
        .px(px(CARD_INSET))
        .py_3()
        .gap_1()
        .justify_center()
        .overflow_hidden()
        // A toned card says so across its top, the way a status strip does;
        // the label underneath says what the colour means. The strip bends
        // with the card's corners — just inside its 1px border — since the
        // card clips its content to a rectangle, not to its rounded shape.
        .when_some(tone, |this, (rule, _)| {
            this.border_t_3()
                .border_color(rule)
                .rounded_t(px(PANEL_RADIUS - 1.))
        })
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
                .line_height(px(size * 1.25))
                // Regular weight: the size already makes it the headline, and
                // a row of bold numbers shouts over the charts below it.
                .font_weight(FontWeight::NORMAL)
                .whitespace_nowrap()
                .text_ellipsis()
                .when_some(tone, |this, (_, ink)| this.text_color(ink))
                .child(value),
        )
        // The change leads its line, the note after it says against what:
        // `▲ 12.3%  vs last week`.
        .when(plot.delta.is_some() || plot.note.is_some(), |this| {
            let muted = cx.theme().muted_foreground;
            let delta = plot.delta.clone().map(|delta| {
                let color = match delta.mood {
                    Mood::Good => crate::ui::palette::tone("good", cx).map(|(_, ink)| ink),
                    Mood::Bad => crate::ui::palette::tone("critical", cx).map(|(_, ink)| ink),
                    Mood::Flat => None,
                };
                div()
                    .flex_none()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color.unwrap_or(muted))
                    .child(delta.text)
            });
            this.child(
                h_flex()
                    .gap_2()
                    .min_w_0()
                    .text_xs()
                    .children(delta)
                    .when_some(plot.note.clone(), |this, note| {
                        this.child(
                            div()
                                .min_w_0()
                                .text_color(muted)
                                .text_ellipsis()
                                .child(note),
                        )
                    }),
            )
        })
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

/// A `text` panel: a heading and the words under it, left-aligned like the
/// cards' labels so a dashboard's sections read down one edge.
fn render_text(plot: &PreparedPlot, cx: &App) -> AnyElement {
    v_flex()
        .size_full()
        // Inset to the cards' content, so the heading and the card labels
        // under it start on one edge.
        .px(px(CARD_INSET + 1.))
        .py_3()
        .gap_1()
        .justify_center()
        .overflow_hidden()
        .when(!plot.title.is_empty(), |this| {
            this.child(
                div()
                    .text_2xl()
                    .font_weight(FontWeight::MEDIUM)
                    .text_ellipsis()
                    .child(plot.title.clone()),
            )
        })
        .when_some(plot.text.clone(), |this, text| {
            this.child(
                div()
                    .text_base()
                    .text_color(cx.theme().muted_foreground)
                    .child(text),
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
    /// The column of the cell last right-clicked, for the menu's copies: the
    /// table tells the menu only the row.
    menu_col: Option<usize>,
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
            menu_col: None,
        }
    }

    /// The cell at `col` of the row shown at `shown`.
    fn cell(&self, shown: usize, col: usize) -> &str {
        self.result
            .rows
            .get(self.source_row(shown))
            .and_then(|row| row.get(col))
            .map_or("", String::as_str)
    }

    /// The row shown at `shown`, its cells tab-separated, as a spreadsheet
    /// pastes a row.
    fn row_text(&self, shown: usize) -> String {
        (0..self.columns.len())
            .map(|col| self.cell(shown, col))
            .collect::<Vec<_>>()
            .join("	")
    }

    /// Column `col`'s cells in the order shown, one per line, as a
    /// spreadsheet pastes a column.
    fn column_text(&self, col: usize) -> String {
        (0..self.order.len())
            .map(|shown| self.cell(shown, col))
            .collect::<Vec<_>>()
            .join(
                "
",
            )
    }

    /// The row of the result shown at `shown`.
    fn source_row(&self, shown: usize) -> usize {
        self.order.get(shown).copied().unwrap_or(shown)
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

    fn context_menu(
        &mut self,
        row_ix: usize,
        menu: PopupMenu,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let copy = |label: &'static str, text: String| {
            PopupMenuItem::new(tr(label))
                .icon(IconName::Copy)
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
                })
        };
        let mut menu = menu;
        if let Some(col) = self.menu_col.filter(|&col| col < self.columns.len()) {
            menu = menu.item(copy(
                "dashboard.table.copy_cell",
                self.cell(row_ix, col).to_string(),
            ));
        }
        menu = menu.item(copy("dashboard.table.copy_row", self.row_text(row_ix)));
        if let Some(col) = self.menu_col.filter(|&col| col < self.columns.len()) {
            menu = menu.item(copy("dashboard.table.copy_column", self.column_text(col)));
        }
        menu
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
        let text = self.cell(row_ix, col_ix).to_string();
        let is_null = text == "NULL";
        h_flex()
            .size_full()
            // Before the row's own right-click, which opens the menu: the
            // menu then knows which cell it is for.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |table, _, _, _| table.delegate_mut().menu_col = Some(col_ix)),
            )
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
        card_value_size, grid_rows, load, sorted_rows, Pick, Run, SpecTableDelegate,
        CARD_VALUE_MAX, CARD_VALUE_MIN,
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
            columns: vec![
                column("name", ColumnKind::Text),
                column("n", ColumnKind::Numeric),
            ],
            rows: [["b", "10"], ["a", "9"], ["c", "NULL"], ["d", "1,200"]]
                .iter()
                .map(|row| row.iter().map(|cell| cell.to_string()).collect())
                .collect(),
            elapsed_ms: 0,
            truncated: false,
        };
        assert_eq!(sorted_rows(&result, 1, ColumnSort::Ascending), [1, 0, 3, 2]);
        assert_eq!(
            sorted_rows(&result, 1, ColumnSort::Descending),
            [3, 0, 1, 2]
        );
        assert_eq!(sorted_rows(&result, 0, ColumnSort::Ascending), [1, 0, 2, 3]);
        // A third click goes back to the query's own order.
        assert_eq!(sorted_rows(&result, 1, ColumnSort::Default), [0, 1, 2, 3]);
    }

    #[test]
    fn a_tables_copies_follow_the_shown_order() {
        let column = |name: &str| ColumnMeta {
            name: name.into(),
            duck_type: String::new(),
            kind: ColumnKind::Text,
        };
        let result = QueryResult {
            columns: vec![column("channel"), column("region")],
            rows: [["Web", "East"], ["App", "North"]]
                .iter()
                .map(|row| row.iter().map(|cell| cell.to_string()).collect())
                .collect(),
            elapsed_ms: 0,
            truncated: false,
        };
        let mut table = SpecTableDelegate::new(Arc::new(result));
        assert_eq!(table.row_text(0), "Web	East");
        assert_eq!(
            table.column_text(1),
            "East
North"
        );
        table.order = sorted_rows(&table.result, 0, ColumnSort::Ascending);
        assert_eq!(table.cell(0, 0), "App");
        assert_eq!(table.row_text(0), "App	North");
        assert_eq!(
            table.column_text(0),
            "App
Web"
        );
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
        std::fs::write(
            dir.join("orders.csv"),
            "channel,amount\nweb,10\nshop,20\nweb,5\n,1\n",
        )
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
        assert!(!Arc::ptr_eq(
            &fresh.results["count"],
            &reloaded.results["count"]
        ));
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
    fn only_the_plots_an_edit_touches_print_differently() {
        let prints = |source: &str| {
            let file = crate::spec::model::parse(source).unwrap();
            let spec = crate::spec::model::validate(&file).unwrap();
            super::plot_prints(&spec, source)
        };
        let spec = |title: &str, sql: &str| {
            format!(
                "query \"q\" {{
  sql = \"{sql}\"
}}
                 plot \"a\" {{
  type = \"card\"
  query = query.q
  title = \"{title}\"
}}
                 plot \"b\" {{
  type = \"card\"
  query = query.q
  title = \"B\"
}}
"
            )
        };
        let before = prints(&spec("A", "SELECT 1 AS n"));
        // A longer title moves plot b down the file without changing it.
        let retitled = prints(&spec("A, renamed at length", "SELECT 1 AS n"));
        assert_ne!(before["a"], retitled["a"]);
        assert_eq!(before["b"], retitled["b"]);
        // Line endings and indentation are not a change.
        let crlf = spec("A", "SELECT 1 AS n").replace('\n', "\r\n    ");
        assert_eq!(prints(&crlf)["b"], before["b"]);
        // A query both read changes both.
        let requeried = prints(&spec("A", "SELECT 2 AS n"));
        assert_ne!(before["b"], requeried["b"]);
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
