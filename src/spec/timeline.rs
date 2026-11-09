//! A dashboard `timeline`: one event per row, placed along its time so the
//! gaps between steps show, in a swim lane per value of `series` (a user, a
//! service, a source IP). Hovering an event names it, its time, how long after
//! the event before it in its lane, and its tooltip columns.

use std::sync::Arc;

use chrono::{DateTime, NaiveDate, NaiveDateTime};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme};
use gpui_kit::*;

use crate::i18n::trf;
use crate::query::QueryResult;
use crate::ui::chart::MAX_SERIES;

/// The most events a timeline draws, each its own element with its own
/// tooltip. Past it they are thinned evenly along the time, first and last
/// kept, and the plot says so.
pub(crate) const MAX_EVENTS: usize = 1_000;

/// One row of the query, placed.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub lane: usize,
    /// Seconds on the timeline's clock: Unix time for a time with an offset,
    /// the clock as written for one without.
    pub at: f64,
    /// The time as the query returned it.
    pub when: String,
    pub caption: String,
    pub tooltip: Vec<(String, String)>,
    /// Seconds since the event before it in its lane, among all the rows
    /// rather than only those left after thinning.
    pub since_previous: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Timeline {
    pub lanes: Vec<String>,
    /// Ordered by time.
    pub events: Vec<Event>,
    /// Rows whose time could not be read.
    pub skipped: usize,
    /// Lanes past [`MAX_SERIES`], whose events are left out.
    pub lanes_left_out: usize,
    /// Events there were before thinning; `None` when none were dropped.
    pub thinned_from: Option<usize>,
}

impl Timeline {
    pub fn span(&self) -> Option<(f64, f64)> {
        Some((self.events.first()?.at, self.events.last()?.at))
    }
}

/// Seconds for `text`: RFC 3339 or `YYYY-MM-DD HH:MM[:SS[.fff]]` with or
/// without an offset, or a bare date.
pub fn seconds(text: &str) -> Option<f64> {
    let text = text.trim();
    let spaced = text.replacen(' ', "T", 1);
    if let Ok(time) = DateTime::parse_from_rfc3339(&spaced) {
        return Some(time.timestamp_millis() as f64 / 1000.);
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f%z", "%Y-%m-%dT%H:%M%z"] {
        if let Ok(time) = DateTime::parse_from_str(&spaced, format) {
            return Some(time.timestamp_millis() as f64 / 1000.);
        }
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(time) = NaiveDateTime::parse_from_str(&spaced, format) {
            return Some(time.and_utc().timestamp_millis() as f64 / 1000.);
        }
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|midnight| midnight.and_utc().timestamp() as f64)
}

/// The timeline `result` draws: `time`, `lane`, `caption` and `tooltip` are
/// column indexes. Lanes keep the order they first appear in.
pub fn build(
    result: &QueryResult,
    time: usize,
    lane: Option<usize>,
    caption: Option<usize>,
    tooltip: &[usize],
) -> Timeline {
    let mut timeline = Timeline::default();
    let mut left_out: Vec<String> = Vec::new();
    for row in &result.rows {
        let cell = |ix: usize| row.get(ix).cloned().unwrap_or_default();
        let Some(at) = row.get(time).and_then(|cell| seconds(cell)) else {
            timeline.skipped += 1;
            continue;
        };
        let lane_name = lane.map(cell).unwrap_or_default();
        let lane = match timeline.lanes.iter().position(|l| *l == lane_name) {
            Some(ix) => ix,
            None if timeline.lanes.len() >= MAX_SERIES => {
                if !left_out.contains(&lane_name) {
                    left_out.push(lane_name);
                }
                continue;
            }
            None => {
                timeline.lanes.push(lane_name);
                timeline.lanes.len() - 1
            }
        };
        timeline.events.push(Event {
            lane,
            at,
            when: cell(time),
            caption: caption.map(cell).unwrap_or_default(),
            tooltip: tooltip
                .iter()
                .map(|&ix| (result.columns[ix].name.clone(), cell(ix)))
                .collect(),
            since_previous: None,
        });
    }
    timeline.lanes_left_out = left_out.len();
    timeline.events.sort_by(|a, b| a.at.total_cmp(&b.at));
    let mut last_in_lane = vec![None::<f64>; timeline.lanes.len()];
    for event in &mut timeline.events {
        event.since_previous = last_in_lane[event.lane].map(|last| event.at - last);
        last_in_lane[event.lane] = Some(event.at);
    }
    thin(&mut timeline);
    timeline
}

/// At most [`MAX_EVENTS`] events, evenly along the list, first and last kept.
fn thin(timeline: &mut Timeline) {
    let total = timeline.events.len();
    if total <= MAX_EVENTS {
        return;
    }
    let keep = |ix: usize| ix * (MAX_EVENTS - 1) / (total - 1);
    let mut previous = None;
    let mut ix = 0;
    timeline.events.retain(|_| {
        let bucket = keep(ix);
        let kept = previous != Some(bucket) || ix + 1 == total;
        if kept {
            previous = Some(bucket);
        }
        ix += 1;
        kept
    });
    timeline.thinned_from = Some(total);
}

/// `seconds` as the gap it is: `45s`, `12m`, `3h 20m`, `2d 4h`.
pub fn gap(seconds: f64) -> String {
    let s = seconds.abs().round() as u64;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 if s % 3600 / 60 == 0 => format!("{}h", s / 3600),
        3600..86_400 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        _ if s % 86_400 / 3600 == 0 => format!("{}d", s / 86_400),
        _ => format!("{}d {}h", s / 86_400, s % 86_400 / 3600),
    }
}

/// Width of the lane names, left of the track.
const LANE_GUTTER: f32 = 112.;
/// A lane's height: as much of the panel as it gets, within these.
const LANE_MIN_HEIGHT: f32 = 26.;
const LANE_MAX_HEIGHT: f32 = 48.;
const DOT: f32 = 9.;
/// An event's caption is drawn when it starts at least this share of the
/// track after the last caption drawn in its lane; the rest are on hover.
const CAPTION_ROOM: f32 = 0.14;
/// Past this share of the track a caption runs left from its dot, not right.
const CAPTION_FLIP: f32 = 0.85;
/// Times written under the track.
const AXIS_TICKS: usize = 5;

/// Where `at` falls on the track, 0 to 1. A single moment sits in the middle.
fn place(at: f64, (start, end): (f64, f64)) -> f32 {
    if end > start {
        ((at - start) / (end - start)) as f32
    } else {
        0.5
    }
}

/// The lines an event's tooltip shows.
fn tooltip_lines(event: &Event) -> String {
    let mut lines = Vec::with_capacity(event.tooltip.len() + 3);
    if !event.caption.is_empty() {
        lines.push(event.caption.clone());
    }
    lines.push(event.when.clone());
    if let Some(since) = event.since_previous {
        lines.push(trf("timeline.after", &[&gap(since)]));
    }
    lines.extend(event.tooltip.iter().map(|(k, v)| format!("{k}: {v}")));
    lines.join("\n")
}

pub(crate) fn render(name: &str, timeline: &Arc<Timeline>, cx: &App) -> AnyElement {
    let Some(span) = timeline.span() else {
        return div().into_any_element();
    };
    let theme = cx.theme();
    let (muted, border) = (theme.muted_foreground, theme.border);
    let mut rows = Vec::with_capacity(timeline.lanes.len());
    for (lane_ix, lane_name) in timeline.lanes.iter().enumerate() {
        let color = crate::ui::palette::color(lane_ix, cx);
        let mut marks: Vec<AnyElement> = Vec::new();
        let mut last_caption: Option<f32> = None;
        for (event_ix, event) in timeline
            .events
            .iter()
            .enumerate()
            .filter(|(_, event)| event.lane == lane_ix)
        {
            let x = place(event.at, span);
            let text = SharedString::from(tooltip_lines(event));
            marks.push(
                div()
                    .id(SharedString::from(format!("timeline-{name}-{event_ix}")))
                    .absolute()
                    .left(relative(x))
                    .top(relative(0.5))
                    .ml(px(-DOT / 2.))
                    .mt(px(-DOT / 2.))
                    .size(px(DOT))
                    .rounded_full()
                    .bg(color)
                    .border_1()
                    .border_color(theme.background)
                    .tooltip(move |window, cx| {
                        let text = text.clone();
                        gpui_kit::component::tooltip::Tooltip::element(move |_, _| {
                            div().whitespace_normal().child(text.clone())
                        })
                        .build(window, cx)
                    })
                    .into_any_element(),
            );
            if !event.caption.is_empty() && last_caption.is_none_or(|last| x - last >= CAPTION_ROOM)
            {
                last_caption = Some(x);
                let caption = div()
                    .absolute()
                    .top(relative(0.5))
                    .mt(px(DOT / 2. + 1.))
                    .max_w(px(160.))
                    .text_xs()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(event.caption.clone());
                marks.push(
                    if x > CAPTION_FLIP {
                        caption.right(relative(1. - x))
                    } else {
                        caption.left(relative(x))
                    }
                    .into_any_element(),
                );
            }
        }
        rows.push(
            h_flex()
                .flex_1()
                .min_h(px(LANE_MIN_HEIGHT))
                .max_h(px(LANE_MAX_HEIGHT))
                .border_b_1()
                .border_color(border.opacity(0.5))
                .child(
                    div()
                        .w(px(LANE_GUTTER))
                        .flex_none()
                        .pr_2()
                        .text_xs()
                        .text_color(muted)
                        .text_ellipsis()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(lane_name.clone()),
                )
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .h_full()
                        .mx_3()
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .top(relative(0.5))
                                .h(px(1.))
                                .bg(border),
                        )
                        .children(marks),
                )
                .into_any_element(),
        );
    }
    let (start, end) = span;
    let ticks = if end > start { AXIS_TICKS } else { 1 };
    let axis = (0..ticks).map(|i| {
        let share = if ticks == 1 {
            0.5
        } else {
            i as f32 / (ticks - 1) as f32
        };
        let at = start + (end - start) * share as f64;
        // The ends say when; between them, how far along.
        let label = if ticks == 1 || i == 0 {
            timeline.events[0].when.clone()
        } else if i + 1 == ticks {
            timeline.events[timeline.events.len() - 1].when.clone()
        } else {
            trf("timeline.offset", &[&gap(at - start)])
        };
        let tick = div()
            .absolute()
            .top_0()
            .text_xs()
            .text_color(muted)
            .whitespace_nowrap()
            .child(label);
        match i {
            0 => tick.left_0(),
            _ if i + 1 == ticks => tick.right_0(),
            _ => tick.left(relative(share)).ml(px(-20.)),
        }
    });
    v_flex()
        .size_full()
        .px_2()
        .pb_2()
        .overflow_hidden()
        .child(v_flex().flex_1().min_h_0().justify_center().children(rows))
        .child(
            h_flex()
                .flex_none()
                .h(px(18.))
                .child(div().w(px(LANE_GUTTER)).flex_none())
                .child(div().relative().flex_1().h_full().mx_3().children(axis)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: that brings `gpui_kit::*`, whose `test` macro
    // shadows the built-in `#[test]`.
    use super::{build, gap, seconds, MAX_EVENTS};
    use crate::query::{ColumnKind, ColumnMeta, QueryResult};
    use crate::ui::chart::MAX_SERIES;

    fn table(rows: Vec<Vec<String>>) -> QueryResult {
        let column = |name: &str| ColumnMeta {
            name: name.into(),
            duck_type: "Text".into(),
            kind: ColumnKind::Text,
        };
        QueryResult {
            columns: vec![column("at"), column("who"), column("what")],
            rows,
            elapsed_ms: 0,
            truncated: false,
            text_nulls: Default::default(),
        }
    }

    fn row(at: &str, who: &str, what: &str) -> Vec<String> {
        vec![at.to_string(), who.to_string(), what.to_string()]
    }

    #[test]
    fn times_read_with_or_without_an_offset() {
        assert_eq!(seconds("1970-01-01T02:15:00Z"), Some(8_100.));
        assert_eq!(seconds("1970-01-01 10:15:00+08:00"), Some(8_100.));
        assert_eq!(seconds("1970-01-01 02:15"), Some(8_100.));
        assert_eq!(seconds("1970-01-01 02:15:00.5"), Some(8_100.5));
        assert_eq!(seconds("1970-01-01"), Some(0.));
        assert_eq!(seconds("soon"), None);
        assert_eq!(seconds("NULL"), None);
    }

    #[test]
    fn events_are_ordered_in_lanes_as_first_seen() {
        let timeline = build(
            &table(vec![
                row("1970-01-01 02:20", "vendor", "callback"),
                row("1970-01-01 02:00", "client", "request"),
                row("not a time", "client", "?"),
                row("1970-01-01 02:05", "client", "retry"),
            ]),
            0,
            Some(1),
            Some(2),
            &[],
        );
        assert_eq!(timeline.lanes, ["vendor", "client"]);
        assert_eq!(timeline.skipped, 1);
        let order: Vec<&str> = timeline.events.iter().map(|e| e.caption.as_str()).collect();
        assert_eq!(order, ["request", "retry", "callback"]);
        assert_eq!(timeline.span().map(|(s, e)| e - s), Some(1200.));
        let since: Vec<Option<f64>> = timeline.events.iter().map(|e| e.since_previous).collect();
        assert_eq!(since, [None, Some(300.), None], "each lane counts its own");
    }

    #[test]
    fn past_the_budget_events_thin_and_keep_their_real_gaps() {
        let rows = (0..MAX_EVENTS as u32 * 3)
            .map(|i| {
                row(
                    &format!(
                        "2026-01-{:02} {:02}:{:02}:{:02}",
                        i / 86_400 + 1,
                        i / 3600 % 24,
                        i / 60 % 60,
                        i % 60
                    ),
                    "only",
                    "",
                )
            })
            .collect();
        let timeline = build(&table(rows), 0, Some(1), None, &[]);
        assert_eq!(timeline.events.len(), MAX_EVENTS);
        assert_eq!(timeline.thinned_from, Some(MAX_EVENTS * 3));
        assert_eq!(
            timeline.events[0].at + 2999.,
            timeline.events[MAX_EVENTS - 1].at
        );
        assert_eq!(
            timeline.events[1].since_previous,
            Some(1.),
            "the gap is to the row before, drawn or not"
        );
    }

    #[test]
    fn lanes_past_the_budget_are_left_out_and_counted() {
        let rows = (0..MAX_SERIES + 2)
            .map(|i| row("2026-01-01 00:00", &format!("lane {i}"), ""))
            .collect();
        let timeline = build(&table(rows), 0, Some(1), None, &[]);
        assert_eq!(timeline.lanes.len(), MAX_SERIES);
        assert_eq!(timeline.lanes_left_out, 2);
        assert_eq!(timeline.events.len(), MAX_SERIES);
    }

    #[test]
    fn gaps_read_as_people_say_them() {
        assert_eq!(gap(45.), "45s");
        assert_eq!(gap(720.), "12m");
        assert_eq!(gap(7200.), "2h");
        assert_eq!(gap(12_000.), "3h 20m");
        assert_eq!(gap(187_200.), "2d 4h");
    }
}
