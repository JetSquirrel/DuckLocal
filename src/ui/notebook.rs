//! A notebook as a workspace tab: a column of cells, each SQL with its own
//! results under it, or Markdown rendered in place.
//!
//! A SQL cell is a query tab in miniature — the same editor, completion,
//! error marks and results panel (table, chart, overview, export) — and runs
//! the same way, on the window's shared connection. So a table one cell
//! creates is there for the next, as in a DuckDB shell.
//!
//! Cells run one at a time, in the order asked: running a cell while another
//! is under way queues it, and Run all queues every SQL cell top to bottom,
//! stopping at the first failure as a script does.
//!
//! The file format is `crate::notebook`'s. What is on disk is the cells'
//! text; results live only in the window.

use std::collections::VecDeque;
use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{
    Editor, EditorState, InputEvent, RangeDecorationCollection, RopeExt, TabSize,
    TextDecorationCollection,
};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable, IconName, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use gpui_kit::assets::IconName as AssetIcon;

use crate::i18n::{tr, trf};
use crate::notebook::{Cell, CellKind, Document};
use crate::state::AppState;
use crate::ui::completion;
use crate::ui::results::ResultsPanel;

/// How tall a cell's results stand. A notebook is read top to bottom, so a
/// result is a window onto its rows rather than the whole screen.
const RESULTS_HEIGHT: f32 = 300.;
/// The fewest and most lines a SQL cell's editor shows before it scrolls.
const EDITOR_MIN_LINES: usize = 1;
const EDITOR_MAX_LINES: usize = 30;
/// The editor's rows, as a multiple of its code font size, and its insets
/// above and below together (with a pixel each for the box's border): what
/// `gpui_kit`'s `Editor` draws at its default size.
const EDITOR_LINE_HEIGHT: f32 = 1.5;
const EDITOR_INSETS: f32 = 18.;
/// The column left of each cell holding its run button and run number.
const GUTTER_WIDTH: f32 = 40.;
/// The space between cells, where the `+` appears.
const GAP_HEIGHT: f32 = 14.;

/// Where a cell is in the run queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunState {
    Idle,
    Queued,
    Running,
}

struct NotebookCell {
    id: u64,
    kind: CellKind,
    editor: Entity<EditorState>,
    /// A SQL cell's results; created on its first run.
    results: Option<Entity<ResultsPanel>>,
    squiggles: TextDecorationCollection,
    fill: RangeDecorationCollection,
    run: RunState,
    /// Which run of the notebook this cell's results came from, `[3]`.
    run_number: Option<u64>,
    /// The last run failed; its marker says so.
    failed: bool,
    /// A Markdown cell shows its editor rather than its rendering.
    editing: bool,
}

pub enum NotebookEvent {
    /// The notebook was written to a new path: the tab's title and the
    /// remembered set follow it.
    SavedAs(PathBuf),
}

impl EventEmitter<NotebookEvent> for Notebook {}

pub struct Notebook {
    state: Entity<AppState>,
    /// `None` until an untitled notebook is first saved.
    path: Option<PathBuf>,
    plain: bool,
    /// The text as last read from or written to the file, for `is_dirty`.
    saved: String,
    cells: Vec<NotebookCell>,
    next_cell_id: u64,
    /// The cell the toolbar's commands and ⌘↵ outside an editor act on.
    selected: Option<u64>,
    run_counter: u64,
    queue: VecDeque<u64>,
    /// The cell whose SQL is on the connection now.
    running: Option<u64>,
    /// A Run all stops at the first failure; a run of one cell does not
    /// clear what was queued after it.
    stop_on_failure: bool,
    scroll: ScrollHandle,
    /// The reason a file could not be read, shown in place of the cells.
    load_error: Option<String>,
    _worker: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Notebook {
    /// Open `path`, or start an untitled notebook of one empty SQL cell.
    pub fn new(
        state: Entity<AppState>,
        path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            state,
            path: path.clone(),
            plain: false,
            saved: String::new(),
            cells: Vec::new(),
            next_cell_id: 1,
            selected: None,
            run_counter: 0,
            queue: VecDeque::new(),
            running: None,
            stop_on_failure: false,
            scroll: ScrollHandle::new(),
            load_error: None,
            _worker: None,
            _subscriptions: Vec::new(),
        };
        let doc = match &path {
            Some(path) => match std::fs::read_to_string(path) {
                Ok(text) => {
                    let doc = crate::notebook::parse(&text);
                    this.saved = crate::notebook::serialize(&doc);
                    // A file whose own spelling differs from ours (CRLF line
                    // ends, spacing) is not dirty on open: compare against
                    // what saving it unchanged would write.
                    doc
                }
                Err(error) => {
                    this.load_error = Some(trf(
                        "notebook.open_failed",
                        &[&path.to_string_lossy(), &error.to_string()],
                    ));
                    Document {
                        cells: Vec::new(),
                        plain: true,
                    }
                }
            },
            None => {
                let doc = Document {
                    cells: vec![Cell::sql("")],
                    plain: false,
                };
                this.saved = crate::notebook::serialize(&doc);
                doc
            }
        };
        this.plain = doc.plain;
        for cell in doc.cells {
            let cell = this.new_cell(cell.kind, &cell.source, window, cx);
            this.cells.push(cell);
        }
        // Markdown opens rendered; only a new, empty one opens for editing.
        for cell in &mut this.cells {
            cell.editing = false;
        }
        this.selected = this.cells.first().map(|cell| cell.id);
        this
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some() || !self.queue.is_empty()
    }

    pub fn has_load_error(&self) -> bool {
        self.load_error.is_some()
    }

    fn document(&self, cx: &App) -> Document {
        Document {
            cells: self
                .cells
                .iter()
                .map(|cell| Cell {
                    kind: cell.kind,
                    source: cell.editor.read(cx).value().to_string(),
                })
                .collect(),
            plain: self.plain,
        }
    }

    /// The cells hold edits the file does not. An untitled notebook is dirty
    /// once anything is typed into it.
    pub fn is_dirty(&self, cx: &App) -> bool {
        self.load_error.is_none() && crate::notebook::serialize(&self.document(cx)) != self.saved
    }

    /// The editor ⌘-shortcuts and focus land in: the selected cell's.
    pub fn focused_editor(&self) -> Option<Entity<EditorState>> {
        let id = self.selected?;
        self.cells
            .iter()
            .find(|cell| cell.id == id)
            .or(self.cells.first())
            .map(|cell| cell.editor.clone())
    }

    fn new_cell(
        &mut self,
        kind: CellKind,
        source: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> NotebookCell {
        let id = self.next_cell_id;
        self.next_cell_id += 1;
        let editor = cx.new(|cx| {
            let state = EditorState::new(window, cx).tab_size(TabSize {
                tab_size: 2,
                hard_tabs: false,
            });
            match kind {
                CellKind::Sql => state.language("sql").line_number(true),
                CellKind::Markdown => state.line_number(false),
            }
        });
        let (squiggles, fill) = editor.update(cx, |state, cx| {
            if kind == CellKind::Sql {
                state.lsp_mut().completion_provider =
                    Some(completion::SqlCompletionProvider::new(self.state.clone()));
            }
            state.set_value(source.to_string(), window, cx);
            (
                state.create_decorations_collection(Vec::new(), cx),
                state.create_range_decorations_collection(Vec::new(), cx),
            )
        });
        let subscription =
            cx.subscribe_in(&editor, window, move |this, editor, event, window, cx| {
                match event {
                    InputEvent::Focus => {
                        this.selected = Some(id);
                        cx.notify();
                    }
                    InputEvent::Change => cx.notify(),
                    // ⌘↵ runs the cell; ⇧↵ runs it and moves on, as in Jupyter.
                    // Both reach the editor as an Enter that has already put a
                    // newline in, so that comes out first.
                    InputEvent::PressEnter { secondary, shift } if *secondary || *shift => {
                        crate::ui::run::remove_enter_newline(editor, window, cx);
                        this.run_cell(id, window, cx);
                        if *shift {
                            this.select_next(id, window, cx);
                        }
                    }
                    InputEvent::Blur => {
                        // Clicking away from a Markdown cell shows what it says,
                        // unless there is nothing to show yet.
                        if let Some(cell) = this.cells.iter_mut().find(|cell| cell.id == id) {
                            if cell.kind == CellKind::Markdown
                                && cell.editing
                                && !editor.read(cx).value().trim().is_empty()
                            {
                                cell.editing = false;
                                cx.notify();
                            }
                        }
                    }
                    _ => {}
                }
            });
        self._subscriptions.push(subscription);
        NotebookCell {
            id,
            kind,
            editor,
            results: None,
            squiggles,
            fill,
            run: RunState::Idle,
            run_number: None,
            failed: false,
            editing: kind == CellKind::Markdown,
        }
    }

    fn index_of(&self, id: u64) -> Option<usize> {
        self.cells.iter().position(|cell| cell.id == id)
    }

    /// Put a new cell after `after` (or at the end), selected and focused.
    pub fn insert_cell(
        &mut self,
        kind: CellKind,
        after: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let at = after
            .and_then(|id| self.index_of(id))
            .map_or(self.cells.len(), |ix| ix + 1);
        let cell = self.new_cell(kind, "", window, cx);
        let id = cell.id;
        self.cells.insert(at, cell);
        self.focus_cell(id, window, cx);
        cx.notify();
    }

    fn delete_cell(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        // A notebook always has a cell to type in.
        if self.cells.len() <= 1 || self.running == Some(id) {
            return;
        }
        let Some(ix) = self.index_of(id) else {
            return;
        };
        self.queue.retain(|queued| *queued != id);
        self.cells.remove(ix);
        // The cell that took its place is selected; a SQL one takes the
        // cursor, a Markdown one stays rendered.
        let next = &self.cells[ix.min(self.cells.len() - 1)];
        self.selected = Some(next.id);
        if next.kind == CellKind::Sql {
            next.editor.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    fn move_cell(&mut self, id: u64, up: bool, cx: &mut Context<Self>) {
        let Some(ix) = self.index_of(id) else {
            return;
        };
        let to = if up {
            ix.checked_sub(1)
        } else {
            (ix + 1 < self.cells.len()).then_some(ix + 1)
        };
        if let Some(to) = to {
            self.cells.swap(ix, to);
            cx.notify();
        }
    }

    fn focus_cell(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(id);
        if let Some(ix) = self.index_of(id) {
            let cell = &mut self.cells[ix];
            if cell.kind == CellKind::Markdown {
                cell.editing = true;
            }
            cell.editor.read(cx).focus_handle(cx).focus(window, cx);
            self.scroll.scroll_to_item(ix);
        }
        cx.notify();
    }

    /// ⇧↵: the cell after `id`, or a new SQL cell when it was the last.
    fn select_next(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index_of(id) else {
            return;
        };
        match self.cells.get(ix + 1).map(|cell| cell.id) {
            Some(next) => {
                self.selected = Some(next);
                let cell = &self.cells[ix + 1];
                // Moving onto Markdown selects it, rendered; moving onto SQL
                // puts the cursor in it.
                if cell.kind == CellKind::Sql {
                    cell.editor.read(cx).focus_handle(cx).focus(window, cx);
                }
                self.scroll.scroll_to_item(ix + 1);
                cx.notify();
            }
            None => self.insert_cell(CellKind::Sql, Some(id), window, cx),
        }
    }

    /// ⌘↵ with no editor focused, and the toolbar's Run: the selected cell.
    pub fn run_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.selected.or(self.cells.first().map(|cell| cell.id)) {
            self.run_cell(id, window, cx);
        }
    }

    /// Run one cell, after whatever is already queued. A Markdown cell's
    /// "run" is showing it rendered.
    pub fn run_cell(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index_of(id) else {
            return;
        };
        if self.cells[ix].kind == CellKind::Markdown {
            if !self.cells[ix].editor.read(cx).value().trim().is_empty() {
                self.cells[ix].editing = false;
            }
            cx.notify();
            return;
        }
        if !self.is_running() {
            self.stop_on_failure = false;
        }
        self.enqueue(id, cx);
        self.start_worker(window, cx);
    }

    /// Run every SQL cell, top to bottom, stopping at the first that fails.
    pub fn run_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_running() {
            return;
        }
        self.stop_on_failure = true;
        let ids: Vec<u64> = self
            .cells
            .iter()
            .filter(|cell| cell.kind == CellKind::Sql)
            .map(|cell| cell.id)
            .collect();
        for id in ids {
            self.enqueue(id, cx);
        }
        self.start_worker(window, cx);
    }

    fn enqueue(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.running == Some(id) || self.queue.contains(&id) {
            return;
        }
        let Some(ix) = self.index_of(id) else {
            return;
        };
        // A blank cell has nothing to run; queueing it would show an empty
        // result as if something had.
        if self.cells[ix].editor.read(cx).value().trim().is_empty() {
            return;
        }
        self.cells[ix].run = RunState::Queued;
        self.queue.push_back(id);
        cx.notify();
    }

    /// Stop the statement on the connection and drop what was queued.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        for id in std::mem::take(&mut self.queue) {
            if let Some(ix) = self.index_of(id) {
                self.cells[ix].run = RunState::Idle;
            }
        }
        if self.running.is_some() {
            crate::db::interrupt();
        }
        cx.notify();
    }

    fn start_worker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running.is_some() || self.queue.is_empty() {
            return;
        }
        let state = self.state.clone();
        self._worker = Some(cx.spawn_in(window, async move |this, cx| loop {
            // The next cell's SQL is read when its turn comes, not when it
            // was queued: an edit made while it waited is what runs.
            let Ok(Some((id, sql, folder))) = this.update(cx, |this, cx| {
                let next = this.take_next(cx)?;
                // Read per cell: a Save As between cells moves the folder.
                let folder = this.path.as_deref().and_then(|path| {
                    crate::spec::source::search_path(path.parent().unwrap_or(path))
                });
                Some((next.0, next.1, folder))
            }) else {
                return;
            };
            let run_sql = sql.clone();
            let executed = smol::unblock(move || {
                crate::ui::run::execute_searching(folder.as_deref(), &run_sql)
            })
            .await;
            let state = state.clone();
            let delivered = this.update_in(cx, |this, window, cx| {
                this.deliver(id, sql, executed, &state, window, cx);
            });
            if delivered.is_err() {
                return;
            }
        }));
    }

    /// Mark the queue's next cell running and hand back its SQL; `None` when
    /// the queue is empty, which ends the worker.
    fn take_next(&mut self, cx: &mut Context<Self>) -> Option<(u64, String)> {
        self.running = None;
        while let Some(id) = self.queue.pop_front() {
            let Some(ix) = self.index_of(id) else {
                continue;
            };
            let sql = self.cells[ix].editor.read(cx).value().to_string();
            if sql.trim().is_empty() {
                self.cells[ix].run = RunState::Idle;
                continue;
            }
            self.run_counter += 1;
            let cell = &mut self.cells[ix];
            cell.run = RunState::Running;
            cell.run_number = Some(self.run_counter);
            // The last run's mark names a range in SQL about to be judged
            // again; stale red is worse than none.
            cell.squiggles.clear(cx);
            cell.fill.clear(cx);
            if let Some(results) = &cell.results {
                results.update(cx, |results, cx| results.set_running(cx));
            }
            self.running = Some(id);
            cx.notify();
            return Some((id, sql));
        }
        cx.notify();
        None
    }

    fn deliver(
        &mut self,
        id: u64,
        sql: String,
        executed: crate::ui::run::Executed,
        state: &Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let failed = matches!(executed.primary(), Some((_, Err(_))));
        let index = self.index_of(id);
        if let (Some(ix), Some((step, Err(error)))) = (index, executed.primary()) {
            let cell = &self.cells[ix];
            let range = executed.script.steps[step].piece.range.clone();
            crate::ui::run::mark_sql_error(&sql, 0, &cell.squiggles, &cell.fill, range, error, cx);
        }
        let steps = executed.publish(state, &sql, cx);
        // The cell may have been deleted while it ran: the history and the
        // catalog still took the run, but there is nowhere to show it.
        let Some(ix) = index else {
            return;
        };
        let results = match &self.cells[ix].results {
            Some(results) => results.clone(),
            None => {
                let results = cx.new(|cx| ResultsPanel::new(window, cx));
                self.cells[ix].results = Some(results.clone());
                results
            }
        };
        results.update(cx, |results, cx| results.set_script(steps, sql, window, cx));
        let cell = &mut self.cells[ix];
        cell.run = RunState::Idle;
        cell.failed = failed;
        if failed && self.stop_on_failure {
            for queued in std::mem::take(&mut self.queue) {
                if let Some(ix) = self.index_of(queued) {
                    self.cells[ix].run = RunState::Idle;
                }
            }
        }
        cx.notify();
    }

    /// Write the cells to the file. An untitled notebook asks where first;
    /// `then` runs once the text is on disk, and not at all when the save is
    /// cancelled or fails (which says why).
    pub fn save(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Window, &mut App) + 'static,
    ) {
        if self.load_error.is_some() {
            return;
        }
        match self.path.clone() {
            Some(path) => {
                // Deferred: the caller is usually mid-update itself (the
                // workspace saving before a close), and `then` reaches back
                // into it.
                if self.write_to(path, window, cx) {
                    cx.defer_in(window, move |_, window, cx| then(window, cx));
                }
            }
            None => {
                let rx = cx.prompt_for_new_path(
                    &start_directory(),
                    Some(tr("notebook.save.suggested_name")),
                );
                cx.spawn_in(window, async move |this, cx| {
                    let Ok(Ok(Some(path))) = rx.await else {
                        return;
                    };
                    let path = with_extension(path);
                    this.update_in(cx, move |this, window, cx| {
                        if this.write_to(path.clone(), window, cx) {
                            cx.emit(NotebookEvent::SavedAs(path));
                            then(window, cx);
                        }
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    fn write_to(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let text = crate::notebook::serialize(&self.document(cx));
        match std::fs::write(&path, &text) {
            Ok(()) => {
                self.saved = text;
                self.path = Some(path);
                cx.notify();
                true
            }
            Err(error) => {
                window.push_notification(
                    Notification::error(trf(
                        "notebook.save_failed",
                        &[&path.to_string_lossy(), &error.to_string()],
                    )),
                    cx,
                );
                false
            }
        }
    }

    /// What `ducklocal open --state` reports of the notebook: each cell's
    /// kind and text, and a SQL cell's result as a query tab's is reported.
    pub fn summary(&self, cx: &App) -> serde_json::Value {
        use serde_json::json;
        let cells: Vec<_> = self
            .cells
            .iter()
            .map(|cell| {
                let mut entry = json!({
                    "kind": match cell.kind {
                        CellKind::Sql => "sql",
                        CellKind::Markdown => "markdown",
                    },
                    "source": cell.editor.read(cx).value().to_string(),
                    "selected": self.selected == Some(cell.id),
                });
                if let Some(results) = &cell.results {
                    entry["result"] = results.read(cx).summary();
                }
                entry
            })
            .collect();
        json!({"running": self.is_running(), "cells": cells})
    }

    /// How tall an editor stands for what it holds: the editor's own rows —
    /// its code font at its line height — and its insets, so a one-line cell
    /// is one line tall and a long one stops at `EDITOR_MAX_LINES`.
    fn editor_height(editor: &Entity<EditorState>, cx: &App) -> Pixels {
        let lines = editor
            .read(cx)
            .text()
            .lines_len()
            .clamp(EDITOR_MIN_LINES, EDITOR_MAX_LINES);
        cx.theme().mono_font_size * EDITOR_LINE_HEIGHT * lines as f32 + px(EDITOR_INSETS)
    }

    /// A cell: a gutter with its run button and run number, the editor or
    /// the rendered Markdown, its results, and — on hover or while selected —
    /// move and delete at its top right. The selected cell is marked by a bar
    /// down its left edge rather than a box around it, so a column of cells
    /// reads as one document.
    fn render_cell(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let cell = &self.cells[ix];
        let id = cell.id;
        let selected = self.selected == Some(id);
        let count = self.cells.len();
        let theme = cx.theme();
        let group: SharedString = format!("notebook-cell-{id}").into();
        // Controls that stay out of the way until the pointer is on the
        // cell, unless the cell is selected or they show something going on.
        let hover_only = |element: Div, keep: bool| {
            if keep {
                element
            } else {
                element
                    .opacity(0.)
                    .group_hover(group.clone(), |style| style.opacity(1.))
            }
        };

        let marker: Option<SharedString> = match (cell.kind, cell.run, cell.run_number) {
            (CellKind::Markdown, _, _) => None,
            (_, RunState::Running, _) => Some("[*]".into()),
            (_, RunState::Queued, _) => Some("[…]".into()),
            (_, RunState::Idle, Some(n)) => Some(format!("[{n}]").into()),
            (_, RunState::Idle, None) => None,
        };
        let busy = cell.run != RunState::Idle;
        let gutter = v_flex()
            .w(crate::ui::scale::design(GUTTER_WIDTH))
            .flex_none()
            .items_center()
            .gap_0p5()
            .pt_1()
            .when(cell.kind == CellKind::Sql, |this| {
                this.child(
                    hover_only(div(), selected || busy).child(
                        Button::new(("cell-run", id as usize))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Play)
                            .loading(cell.run == RunState::Running)
                            .tooltip(tr("notebook.cell.run.tooltip"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.run_cell(id, window, cx)
                            })),
                    ),
                )
                .children(marker.map(|marker| {
                    div()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(if cell.failed {
                            theme.danger
                        } else {
                            theme.muted_foreground
                        })
                        .child(marker)
                }))
            });

        // Code sits in a quiet box, the way a code block does in prose.
        let code_box = |editor: &Entity<EditorState>, height: Pixels| {
            div()
                .w_full()
                .h(height)
                .rounded(theme.radius)
                .border_1()
                .border_color(if selected {
                    theme.border
                } else {
                    theme.border.opacity(0.6)
                })
                .overflow_hidden()
                .child(Editor::new(editor).h(relative(1.)).bordered(false))
        };
        let body = match cell.kind {
            CellKind::Sql => v_flex()
                .w_full()
                .gap_2()
                .child(code_box(
                    &cell.editor,
                    Self::editor_height(&cell.editor, cx),
                ))
                .children(cell.results.clone().map(|results| {
                    div()
                        .w_full()
                        .h(crate::ui::scale::design(RESULTS_HEIGHT))
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border.opacity(0.6))
                        .overflow_hidden()
                        .child(results)
                }))
                .into_any_element(),
            CellKind::Markdown if cell.editing => {
                code_box(&cell.editor, Self::editor_height(&cell.editor, cx)).into_any_element()
            }
            CellKind::Markdown => {
                let text = cell.editor.read(cx).value();
                let empty = text.trim().is_empty();
                div()
                    .id(("cell-markdown", id as usize))
                    .w_full()
                    .px_1()
                    .min_h(crate::ui::scale::design(28.))
                    .cursor_text()
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.click_count() == 2 {
                            this.focus_cell(id, window, cx);
                        }
                    }))
                    .map(|this| {
                        if empty {
                            this.text_sm()
                                .text_color(theme.muted_foreground)
                                .child(tr("notebook.cell.markdown.empty"))
                        } else {
                            this.child(
                                TextView::markdown(("cell-md", id as usize), text).selectable(true),
                            )
                        }
                    })
                    .into_any_element()
            }
        };

        let actions = hover_only(
            h_flex()
                .absolute()
                .top_1()
                .right_1()
                .gap_0p5()
                .p_0p5()
                .rounded(theme.radius)
                .bg(theme.background)
                .border_1()
                .border_color(theme.border.opacity(0.6)),
            selected,
        )
        .child(
            Button::new(("cell-up", id as usize))
                .ghost()
                .xsmall()
                .icon(IconName::ArrowUp)
                .tooltip(tr("notebook.cell.move_up"))
                .disabled(ix == 0)
                .on_click(cx.listener(move |this, _, _, cx| this.move_cell(id, true, cx))),
        )
        .child(
            Button::new(("cell-down", id as usize))
                .ghost()
                .xsmall()
                .icon(IconName::ArrowDown)
                .tooltip(tr("notebook.cell.move_down"))
                .disabled(ix + 1 >= count)
                .on_click(cx.listener(move |this, _, _, cx| this.move_cell(id, false, cx))),
        )
        .child(
            Button::new(("cell-delete", id as usize))
                .ghost()
                .xsmall()
                .icon(AssetIcon::Trash)
                .tooltip(tr("notebook.cell.delete"))
                .disabled(count <= 1 || cell.run == RunState::Running)
                .on_click(cx.listener(move |this, _, window, cx| this.delete_cell(id, window, cx))),
        );

        h_flex()
            .id(("notebook-cell", id as usize))
            .group(group.clone())
            .relative()
            .w_full()
            .items_start()
            .gap_1()
            .py_1p5()
            .pr_2()
            .rounded(theme.radius)
            .border_l_2()
            .border_color(if selected {
                theme.primary
            } else {
                gpui_kit::transparent_black()
            })
            .when(!selected, |this| {
                this.hover(|style| style.bg(theme.muted.opacity(0.35)))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    if this.selected != Some(id) {
                        this.selected = Some(id);
                        cx.notify();
                    }
                }),
            )
            .child(gutter)
            .child(div().flex_1().min_w_0().child(body))
            .child(actions)
            .into_any_element()
    }

    /// The space between two cells, where a new one goes: a hairline and a
    /// `+` that appear only under the pointer, so the column stays quiet.
    /// `always` keeps the `+` showing — at the end of the notebook, where it
    /// is the obvious way to keep going.
    fn render_gap(&self, after: Option<u64>, always: bool, cx: &mut Context<Self>) -> AnyElement {
        let key = after.unwrap_or(0) as usize;
        let group: SharedString = format!("notebook-gap-{key}").into();
        let theme = cx.theme();
        let reveal = |element: Div| {
            if always {
                element
            } else {
                element
                    .opacity(0.)
                    .group_hover(group.clone(), |style| style.opacity(1.))
            }
        };
        let notebook = cx.entity().downgrade();
        div()
            .id(("notebook-gap", key))
            .group(group.clone())
            .relative()
            .w_full()
            .h(crate::ui::scale::design(GAP_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .when(!always, |this| {
                this.child(reveal(
                    div()
                        .absolute()
                        .left(crate::ui::scale::design(GUTTER_WIDTH))
                        .right_0()
                        .top(crate::ui::scale::design(GAP_HEIGHT / 2.))
                        .h(px(1.))
                        .bg(theme.border),
                ))
            })
            .child(
                reveal(div().rounded_full().bg(theme.background)).child(
                    Button::new(("notebook-add", key))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .tooltip(tr("notebook.add.tooltip"))
                        .dropdown_menu(move |menu, _, _| {
                            add_cell_menu(menu, notebook.clone(), after)
                        }),
                ),
            )
            .into_any_element()
    }
}

/// The two kinds of cell a `+` can add. `after` is the cell the new one
/// follows; `None` is the selected cell, which is what the toolbar's `+` means.
pub fn add_cell_menu(
    menu: PopupMenu,
    notebook: WeakEntity<Notebook>,
    after: Option<u64>,
) -> PopupMenu {
    let item = |label: &'static str, icon: AssetIcon, kind: CellKind| {
        let notebook = notebook.clone();
        PopupMenuItem::new(tr(label))
            .icon(icon)
            .on_click(move |_, window, cx| {
                notebook
                    .update(cx, |notebook, cx| {
                        let after = after.or(notebook.selected);
                        notebook.insert_cell(kind, after, window, cx);
                    })
                    .ok();
            })
    };
    menu.item(item(
        "notebook.add.sql_cell",
        AssetIcon::SquareTerminal,
        CellKind::Sql,
    ))
    .item(item(
        "notebook.add.markdown_cell",
        AssetIcon::Type,
        CellKind::Markdown,
    ))
}

impl Render for Notebook {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(error) = &self.load_error {
            return div()
                .size_full()
                .p_4()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(error.clone())
                .into_any_element();
        }
        let last = self.cells.len().saturating_sub(1);
        let mut column: Vec<AnyElement> = Vec::with_capacity(self.cells.len() * 2);
        for ix in 0..self.cells.len() {
            column.push(self.render_cell(ix, cx));
            let id = self.cells[ix].id;
            column.push(self.render_gap(Some(id), ix == last, cx));
        }
        div()
            .id("notebook-cells")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .bg(cx.theme().background)
            .child(
                v_flex()
                    .w_full()
                    .max_w(crate::ui::scale::design(1100.))
                    .mx_auto()
                    .px_4()
                    .py_3()
                    .children(column),
            )
            .into_any_element()
    }
}

/// Where Save asks to put an untitled notebook first: the documents folder.
fn start_directory() -> PathBuf {
    directories::UserDirs::new()
        .map(|dirs| {
            dirs.document_dir()
                .unwrap_or_else(|| dirs.home_dir())
                .to_path_buf()
        })
        .unwrap_or_else(|| PathBuf::from("."))
}

/// A name typed without `.sql` gets it, so the file opens as a notebook
/// again.
fn with_extension(path: PathBuf) -> PathBuf {
    let has = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case(crate::notebook::EXTENSION));
    if has {
        path
    } else {
        let mut name = path.into_os_string();
        name.push(".");
        name.push(crate::notebook::EXTENSION);
        PathBuf::from(name)
    }
}
