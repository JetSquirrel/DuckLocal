//! Results panel below the editor: 结果 / 图表 tabs, the data table, status
//! messages for affected rows and EXPLAIN output, error alerts, and the CSV /
//! Parquet export dialog.

use std::rc::Rc;

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::label::Label;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::i18n::{tr, trf};
use crate::query::{ColumnKind, ExportFormat, QueryOutcome, QueryResult};
use crate::ui::chart::{ChartData, ChartPanel};
use crate::ui::RUN_QUERY_KEYSTROKE;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResultsTab {
    Table,
    Chart,
}

enum ResultView {
    Empty,
    Running,
    Rows(Rc<QueryResult>),
    Affected {
        count: u64,
        elapsed_ms: u128,
    },
    Failed(String),
    Explain {
        lines: Vec<String>,
        elapsed_ms: u128,
    },
}

/// Row-number column header.
const INDEX_COLUMN_HEADER: &str = "#";
/// How many leading columns sit in front of the data columns.
const LEADING_COLUMNS: usize = 1;
/// Per-cell copy buttons build their ElementId as `row * MAX_ID_COLUMNS +
/// col`, so a result set is assumed to never exceed this many columns.
const MAX_ID_COLUMNS: usize = 10_000;

/// Compact cell padding shared by header and body cells.
fn cell_paddings() -> Edges<Pixels> {
    Edges {
        top: px(2.),
        bottom: px(2.),
        left: px(10.),
        right: px(10.),
    }
}

/// Visible-row index map for `filter` over `rows`: the source index of every
/// row containing the (case-insensitive) filter text in any cell. `None`
/// means unfiltered — callers then use the row index directly, so an empty
/// filter costs nothing.
fn filter_row_indices(rows: &[Vec<String>], filter: &str) -> Option<Vec<usize>> {
    let needle = filter.trim().to_lowercase();
    if needle.is_empty() {
        return None;
    }
    let needle_is_ascii = needle.is_ascii();
    Some(
        rows.iter()
            .enumerate()
            .filter_map(|(ix, row)| {
                row.iter()
                    .any(|cell| cell_matches(cell, &needle, needle_is_ascii))
                    .then_some(ix)
            })
            .collect(),
    )
}

/// Case-insensitive substring test against an already-lowercased needle.
///
/// The ASCII path allocates nothing, which matters because this runs once per
/// cell of the whole result set every time the filter changes. Unicode
/// lowercasing of ASCII is plain ASCII lowercasing, so the two paths agree;
/// anything non-ASCII on either side takes the allocating fallback so that
/// case folding stays correct.
fn cell_matches(cell: &str, needle_lower: &str, needle_is_ascii: bool) -> bool {
    if needle_is_ascii && cell.is_ascii() {
        let haystack = cell.as_bytes();
        let needle = needle_lower.as_bytes();
        haystack.len() >= needle.len()
            && haystack
                .windows(needle.len())
                .any(|window| window.eq_ignore_ascii_case(needle))
    } else {
        cell.to_lowercase().contains(needle_lower)
    }
}

/// Test-only accessor for the perf probe harness.
#[cfg(test)]
pub fn filter_row_indices_probe(rows: &[Vec<String>], filter: &str) -> Option<Vec<usize>> {
    filter_row_indices(rows, filter)
}

pub struct ResultTableDelegate {
    /// All columns the table sees: the leading `#` row-number column plus the
    /// query's data columns. The order is a contract, not a preference —
    /// `data_col` subtracts the leading column back out, so data access
    /// always speaks in source column indices.
    columns: Vec<Column>,
    /// The result set, shared with the owning panel rather than copied — a
    /// full row set can be hundreds of megabytes. Filtering never mutates it.
    result: Option<Rc<QueryResult>>,
    filter: String,
    /// Visible row -> source row. `None` = unfiltered (identity mapping).
    filtered: Option<Vec<usize>>,
}

impl ResultTableDelegate {
    fn new() -> Self {
        Self {
            columns: Vec::new(),
            result: None,
            filter: String::new(),
            filtered: None,
        }
    }

    fn index_column() -> Column {
        let mut spec = Column::new(INDEX_COLUMN_HEADER, INDEX_COLUMN_HEADER)
            .width(56.)
            .text_right()
            .resizable(false);
        spec.paddings = Some(cell_paddings());
        spec
    }

    fn set_result(&mut self, result: Rc<QueryResult>) {
        let mut columns = Vec::with_capacity(result.columns.len() + LEADING_COLUMNS);
        columns.push(Self::index_column());
        columns.extend(result.columns.iter().enumerate().map(|(ix, column)| {
            let mut spec = Column::new(format!("c{ix}"), column.name.clone());
            if column.kind == ColumnKind::Numeric {
                spec = spec.text_right();
            }
            spec.paddings = Some(cell_paddings());
            spec
        }));
        self.columns = columns;
        self.result = Some(result);
        // A new result set clears any previous filter.
        self.set_filter(String::new());
    }

    fn rows(&self) -> &[Vec<String>] {
        self.result
            .as_ref()
            .map(|r| r.rows.as_slice())
            .unwrap_or(&[])
    }

    fn set_filter(&mut self, filter: String) {
        self.filtered = filter_row_indices(self.rows(), &filter);
        self.filter = filter;
    }

    /// Delegate column index -> source data column index (see `columns`).
    fn data_col(&self, col_ix: usize) -> usize {
        col_ix.saturating_sub(LEADING_COLUMNS)
    }

    /// Visible row index -> source row index.
    fn source_row(&self, row_ix: usize) -> usize {
        self.filtered
            .as_ref()
            .map(|indices| indices.get(row_ix).copied().unwrap_or(row_ix))
            .unwrap_or(row_ix)
    }

    fn visible_count(&self) -> usize {
        self.filtered
            .as_ref()
            .map(|indices| indices.len())
            .unwrap_or_else(|| self.rows().len())
    }

    fn total_count(&self) -> usize {
        self.rows().len()
    }

    fn is_filtered(&self) -> bool {
        self.filtered.is_some()
    }
}

impl TableDelegate for ResultTableDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.visible_count()
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
        let is_index = col_ix == 0;
        h_flex()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings))
            .child(
                Label::new(column.name.clone())
                    .text_align(column.align)
                    .text_sm()
                    .font_family(cx.theme().mono_font_family.clone())
                    .font_weight(FontWeight::BOLD)
                    .when(is_index, |this| {
                        this.text_color(cx.theme().muted_foreground)
                    })
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
        let base = h_flex()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings));

        // Row-number column: the visible position (1-based), muted.
        if col_ix == 0 {
            return base
                .child(
                    Label::new((row_ix + 1).to_string())
                        .text_align(column.align)
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(cx.theme().muted_foreground)
                        .w_full(),
                )
                .into_any_element();
        }

        // One owned copy per cell, shared with the label and the copy handler;
        // `SharedString::clone` is a refcount bump, not another copy.
        let text: SharedString = self
            .rows()
            .get(self.source_row(row_ix))
            .and_then(|row| row.get(self.data_col(col_ix)))
            .cloned()
            .unwrap_or_default()
            .into();
        let is_null = text == "NULL";
        let group_name: SharedString = format!("td-{row_ix}-{col_ix}").into();

        base.group(group_name.clone())
            .overflow_hidden()
            .child(
                Label::new(text.clone())
                    .text_align(column.align)
                    .text_ellipsis()
                    .font_family(cx.theme().mono_font_family.clone())
                    .when(is_null, |this| this.text_color(cx.theme().muted_foreground))
                    .flex_1()
                    .min_w_0(),
            )
            .child(
                div()
                    .id(("copy-wrapper", row_ix * MAX_ID_COLUMNS + col_ix))
                    .opacity(0.)
                    .group_hover(group_name, |style| style.opacity(1.))
                    .flex_none()
                    .on_click(|_, _, cx: &mut App| cx.stop_propagation())
                    .child(
                        Button::new(("copy-cell", row_ix * MAX_ID_COLUMNS + col_ix))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Copy)
                            .tooltip(tr("results.cell.copy"))
                            .on_click(move |_, _, cx: &mut App| {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
                            }),
                    ),
            )
            .into_any_element()
    }
}

pub struct ResultsPanel {
    view: ResultView,
    tab: ResultsTab,
    table: Entity<TableState<ResultTableDelegate>>,
    /// SQL that produced the current row set, kept for export.
    rows_sql: Option<String>,
    /// Chart rows derived from the current result. Built on first use of the
    /// chart tab and reused across frames; `None` means not yet derived.
    chart_data: Option<Rc<ChartData>>,
    export_input: Option<Entity<InputState>>,
    filter_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl ResultsPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let table = cx.new(|cx| TableState::new(ResultTableDelegate::new(), window, cx));
        let filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr("results.filter.placeholder")));
        let subscription =
            cx.subscribe_in(&filter_input, window, |this, state, event, window, cx| {
                match event {
                    InputEvent::PressEnter { .. } => this.apply_filter(cx),
                    // The cleanable ✕ emits Change with an empty value — treat
                    // that as clearing the filter so the button always works.
                    InputEvent::Change => {
                        if state.read(cx).value().trim().is_empty() {
                            this.apply_filter(cx);
                        }
                    }
                    _ => {}
                }
                let _ = window;
            });
        Self {
            view: ResultView::Empty,
            tab: ResultsTab::Table,
            table,
            rows_sql: None,
            chart_data: None,
            export_input: None,
            filter_input,
            _subscriptions: vec![subscription],
        }
    }

    /// Push the filter text down to the delegate and re-render the table.
    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let filter = self.filter_input.read(cx).value().to_string();
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_filter(filter);
            table.refresh(cx);
        });
        cx.notify();
    }

    pub fn set_running(&mut self, cx: &mut Context<Self>) {
        self.view = ResultView::Running;
        cx.notify();
    }

    pub fn set_outcome(
        &mut self,
        outcome: anyhow::Result<QueryOutcome>,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            Ok(QueryOutcome::Rows(result)) => {
                let result = Rc::new(result);
                self.table.update(cx, |table, cx| {
                    table.delegate_mut().set_result(result.clone());
                    table.refresh(cx);
                });
                self.filter_input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                });
                self.rows_sql = Some(sql);
                self.chart_data = None;
                self.view = ResultView::Rows(result);
            }
            Ok(QueryOutcome::Affected { count, elapsed_ms }) => {
                self.chart_data = None;
                self.view = ResultView::Affected { count, elapsed_ms };
            }
            Err(e) => {
                self.chart_data = None;
                self.view = ResultView::Failed(e.to_string());
            }
        }
        cx.notify();
    }

    pub fn set_explain(
        &mut self,
        result: anyhow::Result<(Vec<String>, u128)>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.chart_data = None;
        self.view = match result {
            Ok((lines, elapsed_ms)) => ResultView::Explain { lines, elapsed_ms },
            Err(e) => ResultView::Failed(e.to_string()),
        };
        self.tab = ResultsTab::Table;
        cx.notify();
    }

    fn open_export_dialog(
        &mut self,
        format: ExportFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sql) = self.rows_sql.clone() else {
            return;
        };
        let default_path = match format {
            ExportFormat::Csv => "~/Desktop/export.csv",
            ExportFormat::Parquet => "~/Desktop/export.parquet",
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(default_path));
        self.export_input = Some(input.clone());
        let view = cx.entity().downgrade();
        let format_label = match format {
            ExportFormat::Csv => "CSV",
            ExportFormat::Parquet => "Parquet",
        };

        window.open_dialog(cx, move |dialog, _, cx| {
            let confirm_view = view.clone();
            let confirm_sql = sql.clone();
            dialog
                .title(trf("dialog.export.title", &[format_label]))
                .w(px(440.))
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(trf("dialog.export.description", &[format_label])),
                        )
                        .child(Input::new(&input)),
                )
                .footer(
                    DialogFooter::new()
                        .gap_2()
                        .child(
                            Button::new("cancel")
                                .outline()
                                .label(tr("common.cancel"))
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("confirm-export")
                                .primary()
                                .label(tr("dialog.export.confirm"))
                                .on_click({
                                    let input = input.clone();
                                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                        let path = input.read(cx).value().to_string();
                                        window.close_dialog(cx);
                                        if let Some(view) = confirm_view.upgrade() {
                                            view.update(cx, |this, cx| {
                                                this.run_export(
                                                    confirm_sql.clone(),
                                                    path.clone(),
                                                    format,
                                                    window,
                                                    cx,
                                                );
                                            });
                                        }
                                    }
                                }),
                        ),
                )
        });
    }

    fn run_export(
        &mut self,
        sql: String,
        path: String,
        format: ExportFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if path.trim().is_empty() {
            window.push_notification(Notification::error(tr("notify.export.empty_path")), cx);
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let export_sql = sql.clone();
            let export_path = path.clone();
            let result =
                smol::unblock(move || crate::query::export(&export_sql, &export_path, format))
                    .await;
            this.update_in(cx, move |_, window, cx| match result {
                Ok(()) => {
                    window.push_notification(
                        Notification::info(trf("notify.export.success", &[&path])),
                        cx,
                    );
                }
                Err(e) => {
                    window.push_notification(
                        Notification::error(trf("notify.export.failed", &[&e.to_string()])),
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    fn rows_count(&self) -> usize {
        match &self.view {
            ResultView::Rows(result) => result.row_count(),
            _ => 0,
        }
    }

    fn columns_count(&self) -> usize {
        match &self.view {
            ResultView::Rows(result) => result.columns.len(),
            _ => 0,
        }
    }

    fn render_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_rows = matches!(self.view, ResultView::Rows(_));
        let summary = has_rows.then(|| {
            trf(
                "results.summary",
                &[
                    &self.rows_count().to_string(),
                    &self.columns_count().to_string(),
                ],
            )
        });

        h_flex()
            .w_full()
            .px_3()
            .gap_3()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                TabBar::new("results-tabs")
                    .underline()
                    .small()
                    .selected_index(match self.tab {
                        ResultsTab::Table => 0,
                        ResultsTab::Chart => 1,
                    })
                    .on_click(cx.listener(|this, ix, _, cx| {
                        this.tab = match ix {
                            0 => ResultsTab::Table,
                            _ => ResultsTab::Chart,
                        };
                        cx.notify();
                    }))
                    .child(Tab::new().label(tr("results.tab.table")))
                    .child(Tab::new().label(tr("results.tab.chart"))),
            )
            .child(div().flex_1())
            .when_some(summary, |this, summary| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(summary),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("export-csv")
                            .outline()
                            .xsmall()
                            .label(tr("results.export_csv"))
                            .disabled(!has_rows)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_export_dialog(ExportFormat::Csv, window, cx);
                            })),
                    )
                    .child(
                        Button::new("export-parquet")
                            .outline()
                            .xsmall()
                            .label(tr("results.export_parquet"))
                            .disabled(!has_rows)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_export_dialog(ExportFormat::Parquet, window, cx);
                            })),
                    ),
            )
    }

    fn render_table_content(&mut self, cx: &mut Context<Self>) -> AnyElement {
        match &self.view {
            ResultView::Empty => empty_state(
                tr("results.empty.title"),
                Some(tr("results.empty.hint_prefix")),
                cx,
            ),
            ResultView::Running => running_state(cx),
            ResultView::Rows(result) => {
                let truncated = result.truncated;
                v_flex()
                    .size_full()
                    .when(truncated, |this| {
                        this.child(
                            Alert::warning(
                                "truncated-notice",
                                // The real count: a wide result is cut by the
                                // cell budget well before the row cap.
                                trf(
                                    "results.truncated",
                                    &[&format_thousands(result.row_count())],
                                ),
                            )
                            .banner()
                            .small(),
                        )
                    })
                    .child(
                        div().flex_1().min_h_0().child(
                            DataTable::new(&self.table)
                                .small()
                                .stripe(true)
                                .scrollbar_visible(true, true),
                        ),
                    )
                    .child(self.render_filter_bar(cx))
                    .into_any_element()
            }
            ResultView::Affected { count, elapsed_ms } => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .child(
                    Icon::new(IconName::CircleCheck)
                        .large()
                        .text_color(cx.theme().success),
                )
                .child(div().text_sm().child(trf(
                    "results.affected",
                    &[
                        &count.to_string(),
                        &crate::state::format_duration(*elapsed_ms as i64),
                    ],
                )))
                .into_any_element(),
            ResultView::Failed(message) => div()
                .size_full()
                .p_3()
                .child(
                    Alert::error("query-error", message.clone()).title(tr("results.failed.title")),
                )
                .into_any_element(),
            ResultView::Explain { lines, elapsed_ms } => v_flex()
                .size_full()
                .child(
                    h_flex().px_3().py_2().child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(trf(
                                "results.explain.elapsed",
                                &[&crate::state::format_duration(*elapsed_ms as i64)],
                            )),
                    ),
                )
                .child(
                    v_flex()
                        .id("explain-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px_3()
                        .pb_3()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_xs()
                        .children(lines.iter().map(|line| div().child(line.clone())))
                        .into_any_element(),
                )
                .into_any_element(),
        }
    }

    fn render_chart_content(&mut self, cx: &mut Context<Self>) -> AnyElement {
        match &self.view {
            ResultView::Rows(result) => {
                // Derived once per result set: walking every row on each frame
                // is what made a large result set unusable in the chart tab.
                let data = match &self.chart_data {
                    Some(data) => data.clone(),
                    None => {
                        let data = Rc::new(ChartData::prepare(result));
                        self.chart_data = Some(data.clone());
                        data
                    }
                };
                ChartPanel::new(data).into_any_element()
            }
            ResultView::Running => running_state(cx),
            _ => empty_state(
                tr("results.empty_chart.title"),
                Some(tr("results.empty.hint_prefix")),
                cx,
            ),
        }
    }

    /// Bottom bar of the table page: client-side filter input + match counts.
    fn render_filter_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let (visible, total, is_filtered) = {
            let delegate = self.table.read(cx).delegate();
            (
                delegate.visible_count(),
                delegate.total_count(),
                delegate.is_filtered(),
            )
        };

        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                Input::new(&self.filter_input)
                    .w(px(200.))
                    .cleanable(true)
                    .small(),
            )
            .when(is_filtered, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(trf(
                            "results.filter.counts",
                            &[&visible.to_string(), &total.to_string()],
                        )),
                )
            })
    }
}

impl Render for ResultsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(self.render_header(cx))
            .child(div().flex_1().min_h_0().child(match self.tab {
                ResultsTab::Table => self.render_table_content(cx),
                ResultsTab::Chart => self.render_chart_content(cx),
            }))
    }
}

/// Shared "query running" placeholder, used by both the table and chart tabs.
fn running_state(cx: &mut Context<ResultsPanel>) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            Icon::new(IconName::LoaderCircle)
                .large()
                .text_color(cx.theme().muted_foreground),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr("results.running")),
        )
        .into_any_element()
}

fn empty_state(title: &str, subtitle: Option<&str>, cx: &mut Context<ResultsPanel>) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            Icon::new(IconName::Inbox)
                .large()
                // Deliberately faded: the icon is decoration, not information.
                .text_color(cx.theme().muted_foreground.alpha(0.5)),
        )
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .child(title.to_string()),
        )
        .when_some(subtitle, |this, subtitle| {
            this.child(
                h_flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(subtitle.to_string()),
                    )
                    .children(
                        Keystroke::parse(RUN_QUERY_KEYSTROKE)
                            .ok()
                            .map(|k| Kbd::new(k).into_any_element()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr("results.empty.hint_suffix")),
                    ),
            )
        })
        .into_any_element()
}

fn format_thousands(n: usize) -> String {
    let mut s = n.to_string();
    let mut out = String::new();
    while s.len() > 3 {
        let split = s.len() - 3;
        out = format!(",{}{}", &s[split..], out);
        s.truncate(split);
    }
    format!("{s}{out}")
}

#[cfg(test)]
mod tests {
    use super::filter_row_indices;

    fn rows() -> Vec<Vec<String>> {
        vec![
            vec!["2026-09-09".into(), "数码配件".into(), "1884201.40".into()],
            vec!["2026-09-09".into(), "生鲜食品".into(), "1402880.05".into()],
            vec!["2026-09-08".into(), "美妆个护".into(), "996447.12".into()],
        ]
    }

    #[test]
    fn empty_filter_is_unfiltered() {
        assert_eq!(filter_row_indices(&rows(), ""), None);
        assert_eq!(filter_row_indices(&rows(), "   "), None);
    }

    #[test]
    fn matches_any_cell_case_insensitively() {
        assert_eq!(filter_row_indices(&rows(), "数码"), Some(vec![0]));
        assert_eq!(filter_row_indices(&rows(), "2026-09-09"), Some(vec![0, 1]));
        assert_eq!(filter_row_indices(&rows(), "NULL"), Some(vec![]));
        // Case-insensitive on latin text.
        let latin = vec![vec!["Alpha".into()], vec!["beta".into()]];
        assert_eq!(filter_row_indices(&latin, "ALPHA"), Some(vec![0]));
        assert_eq!(filter_row_indices(&latin, "Eta"), Some(vec![1]));
    }

    #[test]
    fn preserves_source_order_and_indices() {
        let mapped = filter_row_indices(&rows(), "2026").unwrap();
        assert_eq!(mapped, vec![0, 1, 2]);
    }
}
