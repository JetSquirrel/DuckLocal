//! The results panel's Overview tab: every column of the current result,
//! profiled lazily.
//!
//! Opening the tab costs a `DESCRIBE` and a row count — the shell — and
//! lists every column by name and type in a virtualized list. A column's
//! statistics are computed when it scrolls into view or is clicked, a batch
//! at a time on the window's connection, and kept in the
//! [process cache](crate::overview_cache) so looking again costs nothing.
//! "Profile all" queues the rest, in batches bounded as `ducklocal profile`
//! bounds them, with progress and a Cancel that interrupts the batch running.
//! Clicking a column opens its details beside the list.

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;
use std::time::SystemTime;

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::i18n::{tr, trf};
use crate::overview::{Cancel, Cancelled, ColumnOverview, Distribution, Shell};
use crate::overview_cache::Stamp;
use crate::ui::results::group_digits;

/// Height of one row of the list. Every row is this tall, profiled or not,
/// which is what lets the list draw only the rows in view.
const ROW_HEIGHT: f32 = 52.;
const NAME_WIDTH: f32 = 180.;
const NULLS_WIDTH: f32 = 96.;
const CHART_WIDTH: f32 = 168.;
const CHART_HEIGHT: f32 = 28.;
const DETAIL_WIDTH: f32 = 320.;
const DETAIL_CHART_HEIGHT: f32 = 96.;

/// One column's place in the overview.
#[derive(Clone)]
pub(crate) enum Slot {
    /// Not asked for.
    Pending,
    /// Waiting for a batch.
    Queued,
    /// In the batch running.
    Running,
    Ready(Arc<ColumnOverview>),
    Failed(SharedString),
}

/// What a finished batch brought back.
pub(crate) enum Outcome {
    Profiled(Vec<ColumnOverview>),
    /// The data changed since the shell was read: nothing was profiled.
    Stale,
    Cancelled,
    Failed(String),
}

/// The columns' slots and the queue of those waiting, apart from any window:
/// what to profile next, and what a finished batch changes.
pub(crate) struct Plan {
    pub slots: Vec<Slot>,
    /// Waiting columns, the next first; `true` for one to profile alone,
    /// after a batch it was in failed.
    queue: VecDeque<(usize, bool)>,
    /// The batch running.
    running: Vec<usize>,
    /// Columns queued since the queue was last empty, and how many of them
    /// are done: the progress shown.
    queued: usize,
    done: usize,
}

impl Plan {
    pub fn new(count: usize) -> Self {
        Self {
            slots: vec![Slot::Pending; count],
            queue: VecDeque::new(),
            running: Vec::new(),
            queued: 0,
            done: 0,
        }
    }

    /// Queue `columns` that are neither profiled nor already waiting, ahead
    /// of the rest when `first`. A failed column is asked for again only
    /// explicitly. Whether anything was queued.
    pub fn request(&mut self, columns: impl IntoIterator<Item = usize>, first: bool) -> bool {
        self.request_with(columns, first, false)
    }

    /// [`Plan::request`], counting a failed column as not yet asked for.
    pub fn retry(&mut self, column: usize) -> bool {
        self.request_with([column], true, true)
    }

    fn request_with(
        &mut self,
        columns: impl IntoIterator<Item = usize>,
        first: bool,
        retry: bool,
    ) -> bool {
        let wanted: Vec<usize> = columns
            .into_iter()
            .filter(|&ix| match self.slots.get(ix) {
                Some(Slot::Pending) => true,
                Some(Slot::Failed(_)) => retry,
                _ => false,
            })
            .collect();
        if wanted.is_empty() {
            return false;
        }
        for &ix in &wanted {
            self.slots[ix] = Slot::Queued;
        }
        self.queued += wanted.len();
        if first {
            for &ix in wanted.iter().rev() {
                self.queue.push_front((ix, false));
            }
        } else {
            self.queue.extend(wanted.into_iter().map(|ix| (ix, false)));
        }
        true
    }

    /// Move a waiting column to the front of the queue.
    pub fn hurry(&mut self, column: usize) {
        if let Some(at) = self.queue.iter().position(|(ix, _)| *ix == column) {
            if let Some(entry) = self.queue.remove(at) {
                self.queue.push_front(entry);
            }
        }
    }

    /// The next batch, of at most `size` columns, unless one is running.
    pub fn next_batch(&mut self, size: usize) -> Option<Vec<usize>> {
        if !self.running.is_empty() {
            return None;
        }
        let mut batch = Vec::new();
        while let Some(&(ix, alone)) = self.queue.front() {
            if alone && !batch.is_empty() {
                break;
            }
            self.queue.pop_front();
            batch.push(ix);
            if alone || batch.len() >= size.max(1) {
                break;
            }
        }
        if batch.is_empty() {
            return None;
        }
        for &ix in &batch {
            self.slots[ix] = Slot::Running;
        }
        self.running = batch.clone();
        Some(batch)
    }

    /// What the running batch came back with. Profiled columns come back in
    /// the order they were asked for.
    pub fn finish(&mut self, outcome: Outcome) -> Vec<Arc<ColumnOverview>> {
        let batch = std::mem::take(&mut self.running);
        let mut profiled = Vec::new();
        match outcome {
            Outcome::Profiled(columns) => {
                for (ix, column) in batch.iter().zip(columns) {
                    let column = Arc::new(column);
                    self.slots[*ix] = Slot::Ready(column.clone());
                    profiled.push(column);
                }
                self.done += batch.len();
            }
            Outcome::Failed(_) if batch.len() > 1 => {
                // One column can fail a batch of many: try each alone, so
                // only the one at fault stays failed.
                for &ix in batch.iter().rev() {
                    self.slots[ix] = Slot::Queued;
                    self.queue.push_front((ix, true));
                }
            }
            Outcome::Failed(message) => {
                let message = SharedString::from(message);
                for &ix in &batch {
                    self.slots[ix] = Slot::Failed(message.clone());
                }
                self.done += batch.len();
            }
            Outcome::Stale | Outcome::Cancelled => {
                for &ix in &batch {
                    self.slots[ix] = Slot::Pending;
                }
                self.stop();
            }
        }
        if self.is_idle() {
            self.queued = 0;
            self.done = 0;
        }
        profiled
    }

    /// Empty the queue: everything waiting goes back to not asked for.
    pub fn stop(&mut self) {
        for (ix, _) in self.queue.drain(..) {
            self.slots[ix] = Slot::Pending;
        }
        if self.running.is_empty() {
            self.queued = 0;
            self.done = 0;
        }
    }

    pub fn is_idle(&self) -> bool {
        self.running.is_empty() && self.queue.is_empty()
    }

    /// `(done, of)` since the queue was last empty.
    pub fn progress(&self) -> (usize, usize) {
        (self.done, self.queued)
    }

    pub fn profiled(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| matches!(slot, Slot::Ready(_)))
            .count()
    }
}

/// An overview whose shell is read.
struct Lazy {
    relation: String,
    label: SharedString,
    /// What the shell and every profile here were computed under.
    stamp: Stamp,
    shell: Arc<Shell>,
    plan: Plan,
    /// When the first profile shown was computed, if it came from the cache.
    cached_since: Option<SystemTime>,
    /// The batch running, to cancel.
    cancel: Option<Arc<Cancel>>,
    /// Profile columns as they scroll into view. Off after a Cancel, so the
    /// rows still in view do not start again at once.
    auto: bool,
    /// The data changed since the shell was read.
    stale: bool,
    selected: Option<usize>,
}

enum State {
    Idle,
    Loading(Arc<Cancel>),
    Ready(Box<Lazy>),
    Failed(SharedString),
}

pub struct ColumnOverviewPanel {
    state: State,
    /// Bumped by every load and clear, so an answer for an earlier one is
    /// dropped rather than shown.
    request: u64,
    scroll: UniformListScrollHandle,
}

impl ColumnOverviewPanel {
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            request: 0,
            scroll: UniformListScrollHandle::new(),
        }
    }

    /// Nothing loaded nor loading.
    pub fn is_idle(&self) -> bool {
        matches!(self.state, State::Idle)
    }

    /// Stop whatever is under way and show nothing.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.cancel_all();
        self.request += 1;
        self.state = State::Idle;
        cx.notify();
    }

    /// Show why there is no overview.
    pub fn fail(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.cancel_all();
        self.request += 1;
        self.state = State::Failed(message.into());
        cx.notify();
    }

    fn cancel_all(&mut self) {
        match &self.state {
            State::Loading(cancel) => cancel.cancel(),
            State::Ready(lazy) => {
                if let Some(cancel) = &lazy.cancel {
                    cancel.cancel();
                }
            }
            _ => {}
        }
    }

    /// Read `relation`'s shell — from the cache when it is still valid,
    /// with whatever columns were profiled under it — and list its columns.
    pub fn load(&mut self, relation: String, label: SharedString, cx: &mut Context<Self>) {
        self.cancel_all();
        self.request += 1;
        let request = self.request;
        let cancel = Arc::new(Cancel::default());
        self.state = State::Loading(cancel.clone());
        self.scroll = UniformListScrollHandle::new();
        cx.notify();
        let read = relation.clone();
        cx.spawn(async move |this, cx| {
            let loaded = smol::unblock(move || {
                crate::db::with_connection(|conn| {
                    let stamp = crate::overview_cache::stamp_of(conn, &read);
                    if let Some(cached) = crate::overview_cache::lookup(&read, &stamp) {
                        return Ok((stamp, cached.shell, cached.columns, Some(cached.since)));
                    }
                    let shell = Arc::new(crate::overview::shell_of(conn, &read, &cancel)?);
                    crate::overview_cache::store_shell(&read, &stamp, shell.clone());
                    Ok((stamp, shell, Default::default(), None))
                })
            })
            .await;
            this.update(cx, |this, cx| {
                if this.request != request {
                    return;
                }
                this.state = match loaded {
                    Ok((stamp, shell, columns, since)) => {
                        let mut plan = Plan::new(shell.columns.len());
                        for (ix, (name, _)) in shell.columns.iter().enumerate() {
                            if let Some(column) = columns.get(name) {
                                plan.slots[ix] = Slot::Ready(column.clone());
                            }
                        }
                        let cached_since = since.filter(|_| plan.profiled() > 0);
                        State::Ready(Box::new(Lazy {
                            relation,
                            label,
                            stamp,
                            shell,
                            plan,
                            cached_since,
                            cancel: None,
                            auto: true,
                            stale: false,
                            selected: None,
                        }))
                    }
                    Err(error) if error.is::<Cancelled>() => State::Idle,
                    Err(error) => State::Failed(error.to_string().into()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Profile it all again: forget what the cache holds for this relation.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let State::Ready(lazy) = &self.state else {
            return;
        };
        let (relation, label) = (lazy.relation.clone(), lazy.label.clone());
        crate::overview_cache::forget(&relation);
        self.load(relation, label, cx);
    }

    /// Columns `range` came into view.
    fn on_visible(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        let State::Ready(lazy) = &mut self.state else {
            return;
        };
        if !lazy.auto || lazy.stale {
            return;
        }
        if lazy.plan.request(range, true) {
            self.pump(cx);
            cx.notify();
        }
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let State::Ready(lazy) = &mut self.state else {
            return;
        };
        lazy.selected = Some(ix);
        if !lazy.stale {
            if matches!(lazy.plan.slots.get(ix), Some(Slot::Failed(_))) {
                lazy.plan.retry(ix);
            } else if !lazy.plan.request([ix], true) {
                lazy.plan.hurry(ix);
            }
            self.pump(cx);
        }
        cx.notify();
    }

    fn profile_all(&mut self, cx: &mut Context<Self>) {
        let State::Ready(lazy) = &mut self.state else {
            return;
        };
        lazy.auto = true;
        lazy.plan.request(0..lazy.plan.slots.len(), false);
        self.pump(cx);
        cx.notify();
    }

    fn resume(&mut self, cx: &mut Context<Self>) {
        if let State::Ready(lazy) = &mut self.state {
            lazy.auto = true;
        }
        // The rows in view ask for themselves on the next frame.
        cx.notify();
    }

    /// Stop: the batch running is interrupted, the queue emptied, and
    /// columns scrolling into view wait to be asked for.
    fn cancel(&mut self, cx: &mut Context<Self>) {
        let State::Ready(lazy) = &mut self.state else {
            return;
        };
        if let Some(cancel) = lazy.cancel.take() {
            cancel.cancel();
        }
        lazy.plan.stop();
        lazy.auto = false;
        cx.notify();
    }

    /// Start the next batch, unless one is running.
    fn pump(&mut self, cx: &mut Context<Self>) {
        let State::Ready(lazy) = &mut self.state else {
            return;
        };
        if lazy.stale {
            return;
        }
        let size = crate::overview::batch_size(lazy.shell.row_count);
        let Some(batch) = lazy.plan.next_batch(size) else {
            return;
        };
        let wanted: Vec<(String, String)> = batch
            .iter()
            .map(|&ix| lazy.shell.columns[ix].clone())
            .collect();
        let cancel = Arc::new(Cancel::default());
        lazy.cancel = Some(cancel.clone());
        let (relation, stamp, row_count) = (
            lazy.relation.clone(),
            lazy.stamp.clone(),
            lazy.shell.row_count,
        );
        let request = self.request;
        cx.spawn(async move |this, cx| {
            let outcome = smol::unblock(move || {
                let read = crate::db::with_connection(|conn| {
                    // Under the same lock as the profile, so nothing can
                    // write between the check and the scan.
                    if crate::overview_cache::stamp_of(conn, &relation) != stamp {
                        return Ok(None);
                    }
                    crate::overview::profile_columns(conn, &relation, &wanted, row_count, &cancel)
                        .map(Some)
                });
                match read {
                    Ok(Some(columns)) => Outcome::Profiled(columns),
                    Ok(None) => Outcome::Stale,
                    Err(error) if error.is::<Cancelled>() => Outcome::Cancelled,
                    Err(error) => Outcome::Failed(error.to_string()),
                }
            })
            .await;
            this.update(cx, |this, cx| {
                if this.request != request {
                    return;
                }
                let State::Ready(lazy) = &mut this.state else {
                    return;
                };
                lazy.cancel = None;
                let stale = matches!(outcome, Outcome::Stale);
                let profiled = lazy.plan.finish(outcome);
                if stale {
                    lazy.stale = true;
                }
                crate::overview_cache::store_columns(&lazy.relation, &lazy.stamp, &profiled);
                this.pump(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Show `columns` as profiled, without the database: for tests that
    /// paint an overview.
    #[cfg(test)]
    pub(crate) fn show_profiled(
        &mut self,
        relation: &str,
        shell: Shell,
        columns: Vec<ColumnOverview>,
        cx: &mut Context<Self>,
    ) {
        let mut plan = Plan::new(shell.columns.len());
        for (ix, column) in columns.into_iter().enumerate() {
            plan.slots[ix] = Slot::Ready(Arc::new(column));
        }
        // One left unprofiled, one failed: every kind of row paints.
        if let Some(last) = plan.slots.len().checked_sub(1) {
            plan.slots[last] = Slot::Failed("boom".into());
        }
        self.request += 1;
        self.state = State::Ready(Box::new(Lazy {
            relation: relation.to_string(),
            label: "test".into(),
            stamp: crate::overview_cache::stamp_of(
                &duckdb::Connection::open_in_memory().expect("in-memory database"),
                relation,
            ),
            shell: Arc::new(shell),
            plan,
            cached_since: Some(SystemTime::now()),
            cancel: None,
            auto: false,
            stale: true,
            selected: Some(0),
        }));
        cx.notify();
    }

    fn render_header(&self, lazy: &Lazy, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let total = lazy.shell.columns.len();
        let profiled = lazy.plan.profiled();
        let (done, of) = lazy.plan.progress();
        let busy = !lazy.plan.is_idle();
        let age = lazy.cached_since.map(|since| {
            let ms = since.elapsed().map(|d| d.as_millis()).unwrap_or(0);
            format_age(ms as u64)
        });
        v_flex()
            .px_3()
            .py_2()
            .gap_2()
            .when(lazy.stale, |this| {
                this.child(Alert::warning("overview-stale", tr("results.overview.stale")).small())
            })
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .text_xs()
                    .text_color(muted)
                    .child(trf(
                        "results.overview.summary",
                        &[
                            &lazy.label,
                            &group_digits(lazy.shell.row_count.max(0) as u64),
                            &total.to_string(),
                        ],
                    ))
                    .child(trf(
                        "results.overview.profiled",
                        &[&profiled.to_string(), &total.to_string()],
                    ))
                    .when_some(age, |this, age| {
                        this.child(trf("results.overview.cached", &[&age]))
                    })
                    .when(busy, |this| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .text_color(cx.theme().foreground)
                                .child(Icon::new(IconName::LoaderCircle).xsmall())
                                .child(trf(
                                    "results.overview.progress",
                                    &[&done.to_string(), &of.to_string()],
                                )),
                        )
                    })
                    .when(!busy && !lazy.auto && !lazy.stale, |this| {
                        this.child(tr("results.overview.paused"))
                    })
                    .child(div().flex_1())
                    .when(busy, |this| {
                        this.child(
                            Button::new("overview-cancel")
                                .ghost()
                                .xsmall()
                                .label(tr("common.cancel"))
                                .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                        )
                    })
                    .when(!busy && !lazy.auto && !lazy.stale, |this| {
                        this.child(
                            Button::new("overview-resume")
                                .ghost()
                                .xsmall()
                                .label(tr("results.overview.resume"))
                                .on_click(cx.listener(|this, _, _, cx| this.resume(cx))),
                        )
                    })
                    .when(!lazy.stale && profiled < total, |this| {
                        this.child(
                            Button::new("overview-profile-all")
                                .ghost()
                                .xsmall()
                                .label(tr("results.overview.profile_all"))
                                .on_click(cx.listener(|this, _, _, cx| this.profile_all(cx))),
                        )
                    })
                    .child(
                        Button::new("overview-refresh")
                            .ghost()
                            .xsmall()
                            .icon(IconName::RotateCw)
                            .label(tr("results.overview.refresh"))
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
            )
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let State::Ready(lazy) = &self.state else {
            return div().into_any_element();
        };
        let (name, data_type) = &lazy.shell.columns[ix];
        let rows = lazy.shell.row_count.max(0) as u64;
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        let selected = lazy.selected == Some(ix);
        let hovered_bg = cx.theme().accent.opacity(0.5);

        let identity = v_flex()
            .w(px(NAME_WIDTH))
            .flex_none()
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .truncate()
                    .child(name.clone()),
            )
            .child(
                div()
                    .font_family(mono.clone())
                    .text_color(muted)
                    .truncate()
                    .child(data_type.clone()),
            );

        let body: AnyElement = match &lazy.plan.slots[ix] {
            Slot::Ready(column) => h_flex()
                .flex_1()
                .min_w_0()
                .gap_4()
                .items_center()
                .child(nulls(column, cx))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .text_color(muted)
                        .child(div().truncate().child(trf(
                            "results.overview.distinct",
                            &[&group_digits(column.approx_unique.max(0) as u64)],
                        )))
                        .when_some(range_of(column), |this, range| {
                            this.child(
                                div()
                                    .font_family(mono.clone())
                                    .text_color(cx.theme().foreground)
                                    .truncate()
                                    .child(range),
                            )
                        }),
                )
                .child(mini_distribution(column, rows, cx))
                .into_any_element(),
            Slot::Pending => placeholder(tr("results.overview.pending"), None, cx),
            Slot::Queued | Slot::Running => placeholder("…", Some(IconName::LoaderCircle), cx),
            Slot::Failed(_) => h_flex()
                .flex_1()
                .gap_1()
                .items_center()
                .text_color(cx.theme().danger)
                .child(Icon::new(IconName::CircleX).xsmall())
                .child(tr("results.overview.column_failed"))
                .child(div().text_color(muted).child(tr("results.overview.retry")))
                .into_any_element(),
        };

        h_flex()
            .id(("overview-column", ix))
            .h(px(ROW_HEIGHT))
            .w_full()
            .px_3()
            .gap_4()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .text_xs()
            .cursor_pointer()
            .when(selected, |this| this.bg(cx.theme().accent))
            .when(!selected, |this| {
                this.hover(move |style| style.bg(hovered_bg))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.select(ix, cx)))
            .child(identity)
            .child(body)
            .into_any_element()
    }

    fn render_detail(&self, lazy: &Lazy, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let (name, data_type) = &lazy.shell.columns[ix];
        let rows = lazy.shell.row_count.max(0) as u64;
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        let heading = h_flex()
            .gap_2()
            .items_start()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_semibold().truncate().child(name.clone()))
                    .child(
                        div()
                            .text_xs()
                            .font_family(mono.clone())
                            .text_color(muted)
                            .truncate()
                            .child(data_type.clone()),
                    ),
            )
            .child(
                Button::new("overview-detail-close")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .tooltip(tr("results.overview.detail.close"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let State::Ready(lazy) = &mut this.state {
                            lazy.selected = None;
                        }
                        cx.notify();
                    })),
            );

        let body: AnyElement = match &lazy.plan.slots[ix] {
            Slot::Ready(column) => {
                let null_rows = (column.null_fraction * rows as f64).round() as u64;
                let fact = |label: &'static str, value: String| {
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .child(
                            div()
                                .w(px(110.))
                                .flex_none()
                                .text_color(muted)
                                .child(tr(label)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .font_family(mono.clone())
                                .truncate()
                                .child(value),
                        )
                };
                v_flex()
                    .gap_1()
                    .child(fact("results.overview.detail.rows", group_digits(rows)))
                    .child(fact(
                        "results.overview.detail.nulls",
                        format!(
                            "{} ({:.1}%)",
                            group_digits(null_rows),
                            column.null_fraction * 100.
                        ),
                    ))
                    .child(fact(
                        "results.overview.detail.distinct",
                        group_digits(column.approx_unique.max(0) as u64),
                    ))
                    .when_some(column.min.clone(), |this, min| {
                        this.child(fact("results.overview.detail.min", min))
                    })
                    .when_some(column.max.clone(), |this, max| {
                        this.child(fact("results.overview.detail.max", max))
                    })
                    .when_some(column.median.clone(), |this, median| {
                        this.child(fact("results.overview.detail.median", median))
                    })
                    .child(div().h_2())
                    .child(detail_distribution(column, rows, cx))
                    .into_any_element()
            }
            Slot::Failed(message) => Alert::error("overview-detail-error", message.clone())
                .title(tr("results.overview.column_failed"))
                .small()
                .into_any_element(),
            Slot::Pending => div()
                .text_xs()
                .text_color(muted)
                .child(tr("results.overview.pending"))
                .into_any_element(),
            Slot::Queued | Slot::Running => h_flex()
                .gap_1()
                .text_xs()
                .text_color(muted)
                .child(Icon::new(IconName::LoaderCircle).xsmall())
                .child("…")
                .into_any_element(),
        };

        v_flex()
            .id("overview-detail")
            .w(px(DETAIL_WIDTH))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .p_3()
            .gap_3()
            .border_l_1()
            .border_color(cx.theme().border)
            .child(heading)
            .child(body)
            .into_any_element()
    }
}

impl Default for ColumnOverviewPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for ColumnOverviewPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        match &self.state {
            State::Idle => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .child(
                    Icon::new(IconName::Inbox)
                        .large()
                        .text_color(muted.alpha(0.5)),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(tr("results.overview.empty")),
                )
                .into_any_element(),
            State::Loading(_) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .child(Icon::new(IconName::LoaderCircle).large().text_color(muted))
                .child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(tr("results.overview.running")),
                )
                .into_any_element(),
            State::Failed(message) => div()
                .size_full()
                .p_3()
                .child(
                    Alert::error("overview-error", message.clone())
                        .title(tr("results.failed.title")),
                )
                .into_any_element(),
            State::Ready(lazy) => {
                let count = lazy.shell.columns.len();
                let header = self.render_header(lazy, cx);
                let detail = lazy
                    .selected
                    .filter(|&ix| ix < count)
                    .map(|ix| self.render_detail(lazy, ix, cx));
                let list = uniform_list(
                    "overview-columns",
                    count,
                    cx.processor(|this, range: Range<usize>, _window, cx| {
                        this.on_visible(range.clone(), cx);
                        range.map(|ix| this.render_row(ix, cx)).collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.scroll)
                .flex_1()
                .h_full();
                v_flex()
                    .size_full()
                    .child(header)
                    .child(
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .child(div().flex_1().min_w_0().h_full().child(list))
                            .children(detail),
                    )
                    .into_any_element()
            }
        }
    }
}

/// How long ago, coarsely: the cache's age is a hint, not a measurement.
fn format_age(ms: u64) -> String {
    let seconds = ms / 1000;
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        _ => format!("{}h", seconds / 3600),
    }
}

fn range_of(column: &ColumnOverview) -> Option<String> {
    match (&column.min, &column.max) {
        (Some(min), Some(max)) if min != max => Some(format!("{min} – {max}")),
        (Some(min), _) => Some(min.clone()),
        _ => None,
    }
}

fn placeholder(
    text: &'static str,
    icon: Option<IconName>,
    cx: &mut Context<ColumnOverviewPanel>,
) -> AnyElement {
    h_flex()
        .flex_1()
        .gap_1()
        .items_center()
        .text_color(cx.theme().muted_foreground.alpha(0.7))
        .when_some(icon, |this, icon| this.child(Icon::new(icon).xsmall()))
        .child(text)
        .into_any_element()
}

fn nulls(column: &ColumnOverview, cx: &mut Context<ColumnOverviewPanel>) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let null_pct = (column.null_fraction * 100.0).clamp(0.0, 100.0);
    v_flex()
        .w(px(NULLS_WIDTH))
        .flex_none()
        .gap_1()
        .child(
            div()
                .when(null_pct > 0., |d| d.text_color(cx.theme().warning))
                .when(null_pct == 0., |d| d.text_color(muted))
                .child(trf("results.overview.nulls", &[&format!("{null_pct:.1}%")])),
        )
        .child(
            div()
                .w_full()
                .h(px(4.))
                .rounded_sm()
                .bg(cx.theme().border)
                .child(
                    div()
                        .h_full()
                        .rounded_sm()
                        .w(relative((null_pct / 100.) as f32))
                        .bg(cx.theme().warning),
                ),
        )
}

/// Bars of `bins`, `width` by `height`.
fn histogram(
    bins: &[(String, u64)],
    width: f32,
    height: f32,
    cx: &mut Context<ColumnOverviewPanel>,
) -> Div {
    let peak = bins.iter().map(|(_, n)| *n).max().unwrap_or(0).max(1) as f32;
    let bar = (width / bins.len().max(1) as f32 - 1.).max(1.);
    h_flex()
        .w(px(width))
        .h(px(height))
        .flex_none()
        .items_end()
        .gap(px(1.))
        .children(bins.iter().map(|(_, n)| {
            div()
                .w(px(bar))
                // A bin with rows stays visible, however small.
                .h(px(if *n == 0 {
                    0.
                } else {
                    (height * *n as f32 / peak).max(2.)
                }))
                .bg(cx.theme().primary.opacity(0.7))
        }))
}

/// A row's distribution, in the one height every row has: bars for bins,
/// the most common values' shares as one stacked bar and their names.
fn mini_distribution(
    column: &ColumnOverview,
    rows: u64,
    cx: &mut Context<ColumnOverviewPanel>,
) -> AnyElement {
    match &column.distribution {
        Distribution::Bins(bins) => {
            histogram(bins, CHART_WIDTH, CHART_HEIGHT, cx).into_any_element()
        }
        Distribution::Top(values) => {
            let names = values
                .iter()
                .map(|(value, _)| value.as_str())
                .collect::<Vec<_>>()
                .join(" · ");
            let counted = values.iter().any(|(_, n)| n.is_some());
            v_flex()
                .w(px(CHART_WIDTH))
                .flex_none()
                .gap_1()
                .when(counted, |this| {
                    this.child(
                        h_flex()
                            .w_full()
                            .h(px(6.))
                            .rounded_sm()
                            .overflow_hidden()
                            .bg(cx.theme().border)
                            .children(values.iter().enumerate().filter_map(|(rank, (_, n))| {
                                let share = (*n)? as f32 / rows.max(1) as f32;
                                Some(
                                    div()
                                        .h_full()
                                        .w(relative(share))
                                        .bg(cx.theme().primary.opacity(0.8 - 0.12 * rank as f32)),
                                )
                            })),
                    )
                })
                .child(
                    div()
                        .truncate()
                        .text_color(cx.theme().muted_foreground)
                        .child(names),
                )
                .into_any_element()
        }
        Distribution::None => div().w(px(CHART_WIDTH)).flex_none().into_any_element(),
    }
}

/// The detail pane's distribution: a taller histogram with its ends, or
/// every common value with its share and count.
fn detail_distribution(
    column: &ColumnOverview,
    rows: u64,
    cx: &mut Context<ColumnOverviewPanel>,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let width = DETAIL_WIDTH - 24.;
    match &column.distribution {
        Distribution::Bins(bins) => v_flex()
            .gap_1()
            .text_xs()
            .child(
                div()
                    .text_color(muted)
                    .child(tr("results.overview.detail.distribution")),
            )
            .child(histogram(bins, width, DETAIL_CHART_HEIGHT, cx))
            .child(
                h_flex()
                    .w(px(width))
                    .justify_between()
                    .text_color(muted)
                    .font_family(cx.theme().mono_font_family.clone())
                    .children(column.min.clone())
                    .children(column.max.clone()),
            )
            .into_any_element(),
        Distribution::Top(values) => {
            let estimated = values.iter().all(|(_, n)| n.is_none());
            v_flex()
                .gap(px(2.))
                .text_xs()
                .child(div().text_color(muted).child(if estimated {
                    tr("results.overview.estimated")
                } else {
                    tr("results.overview.detail.common")
                }))
                .children(values.iter().map(|(value, n)| {
                    let share = n.map(|n| n as f32 / rows.max(1) as f32).unwrap_or(0.);
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .relative()
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .bottom_0()
                                        .left_0()
                                        .w(relative(share))
                                        .bg(cx.theme().primary.opacity(0.15)),
                                )
                                .child(div().px_1().truncate().child(value.clone())),
                        )
                        .when_some(*n, |this, n| {
                            this.child(div().flex_none().text_color(muted).child(format!(
                                "{} · {:.1}%",
                                group_digits(n),
                                share * 100.
                            )))
                        })
                }))
                .into_any_element()
        }
        Distribution::None => div().into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: that pulls in `gpui_kit::*`, whose
    // `test` macro shadows the built-in `#[test]`.
    use super::{format_age, Outcome, Plan, Slot};
    use crate::overview::{ColumnOverview, Distribution, Shape};

    fn column(name: &str) -> ColumnOverview {
        ColumnOverview {
            name: name.into(),
            data_type: "INTEGER".into(),
            shape: Shape::Numeric,
            null_fraction: 0.,
            approx_unique: 1,
            min: None,
            max: None,
            median: None,
            distribution: Distribution::None,
        }
    }

    fn states(plan: &Plan) -> String {
        plan.slots
            .iter()
            .map(|slot| match slot {
                Slot::Pending => '.',
                Slot::Queued => 'q',
                Slot::Running => 'r',
                Slot::Ready(_) => '#',
                Slot::Failed(_) => 'x',
            })
            .collect()
    }

    #[test]
    fn columns_in_view_go_first_and_batches_are_bounded() {
        let mut plan = Plan::new(10);
        // "Profile all" queues the first eight behind…
        assert!(plan.request(0..8, false));
        assert!(!plan.request(6..8, true), "already queued");
        // …and the last two scroll into view and go ahead of them.
        assert!(plan.request(8..10, true));
        // A click hurries a column already waiting.
        plan.hurry(7);
        let batch = plan.next_batch(3).unwrap();
        assert_eq!(batch, [7, 8, 9]);
        assert_eq!(states(&plan), "qqqqqqqrrr");
        assert!(plan.next_batch(3).is_none(), "one batch at a time");
        assert_eq!(plan.progress(), (0, 10));

        let profiled = plan.finish(Outcome::Profiled(vec![
            column("h"),
            column("i"),
            column("j"),
        ]));
        assert_eq!(profiled.len(), 3);
        assert_eq!(states(&plan), "qqqqqqq###");
        assert_eq!(plan.progress(), (3, 10));
        assert_eq!(plan.profiled(), 3);
        assert_eq!(plan.next_batch(3).unwrap(), [0, 1, 2]);
    }

    #[test]
    fn a_column_profiles_alone_when_asked_for_alone() {
        let mut plan = Plan::new(5000);
        assert!(plan.request([4321], true));
        assert_eq!(plan.next_batch(64).unwrap(), [4321]);
        plan.finish(Outcome::Profiled(vec![column("c4321")]));
        assert_eq!(plan.profiled(), 1);
        assert!(plan.is_idle());
    }

    #[test]
    fn a_failed_batch_is_retried_one_column_at_a_time() {
        let mut plan = Plan::new(3);
        plan.request(0..3, false);
        let batch = plan.next_batch(3).unwrap();
        assert_eq!(batch.len(), 3);
        plan.finish(Outcome::Failed("bad".into()));
        assert_eq!(states(&plan), "qqq");
        assert_eq!(plan.next_batch(3).unwrap(), [0]);
        plan.finish(Outcome::Profiled(vec![column("a")]));
        assert_eq!(plan.next_batch(3).unwrap(), [1]);
        plan.finish(Outcome::Failed("bad".into()));
        assert_eq!(plan.next_batch(3).unwrap(), [2]);
        plan.finish(Outcome::Profiled(vec![column("c")]));
        assert_eq!(states(&plan), "#x#");
        assert!(plan.is_idle());
        assert_eq!(plan.progress(), (0, 0), "reset once idle");

        // Scrolling past a failed column does not ask again; clicking does.
        assert!(!plan.request([1], true));
        assert!(plan.retry(1));
        assert_eq!(states(&plan), "#q#");
    }

    #[test]
    fn cancel_and_stale_put_everything_back() {
        let mut plan = Plan::new(4);
        plan.request(0..4, false);
        plan.next_batch(2).unwrap();
        plan.stop();
        assert_eq!(states(&plan), "rr..");
        plan.finish(Outcome::Cancelled);
        assert_eq!(states(&plan), "....");
        assert!(plan.is_idle());

        plan.request(0..4, false);
        plan.next_batch(2).unwrap();
        plan.finish(Outcome::Stale);
        assert_eq!(states(&plan), "....");
        assert_eq!(plan.progress(), (0, 0));
    }

    #[test]
    fn ages_read_coarsely() {
        assert_eq!(format_age(4_000), "4s");
        assert_eq!(format_age(125_000), "2m");
        assert_eq!(format_age(7_300_000), "2h");
    }
}
