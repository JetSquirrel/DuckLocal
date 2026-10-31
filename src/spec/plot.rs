//! The dashboard plots the catalog charts cannot draw.
//!
//! The catalog's `BarChart` and `LineChart` take a single series, and its one
//! multi-series chart fills areas — so a pivoted `bar` drawn with them comes
//! out as a stack of small charts, a pivoted `line` as a faintly filled area
//! and a `scatter` as dots connected by lines. The two plots here draw those
//! on the same `plot` primitives the catalog composes: [`GroupedBars`] lays a
//! band's series side by side within the band, and [`SeriesPlot`] projects
//! every series onto shared point and value scales, connecting a series'
//! points (a `line`) or not (a `scatter`).
//!
//! Every `bar` draws here, one series or several: the catalog's bar chart
//! colours bar by bar and thins a category axis's labels, and a bar's colour
//! should name its series, its label its category.
//!
//! Composition follows the catalog charts — same axis gutter, grid, label
//! stride, palette, crosshair and tooltip — so a dashboard reads as one family
//! whichever kind a plot is. Like them, a plot here is a value rebuilt on
//! every frame: hover state and the line path caches live in the window's
//! element state, keyed on the plot's id. And the data is `prepare`'s, already
//! capped at its bar/point/series limits, so a frame's work stays bounded the
//! way a catalog chart's is.

use std::sync::Arc;

use gpui_kit::component::plot::label::{truncate_text_to_width, Text, TEXT_GAP};
use gpui_kit::component::plot::scale::{Scale, ScaleBand, ScaleLinear, ScalePoint};
use gpui_kit::component::plot::shape::{Bar, BarAlignment, Line};
use gpui_kit::component::plot::tooltip::{CrossLine, Dot, Tooltip, TooltipState};
use gpui_kit::component::plot::{
    axis_gutter, AxisText, Curve, Grid, PathCaches, Plot, PlotAxis, PlotElement, PlotLabel,
};
use gpui_kit::component::ActiveTheme;
use gpui_kit::*;

use super::format::{full_text, short_text, NumberFormat};
use crate::spec::prepare::{PlotPoint, PreparedPlot};
use crate::ui::chart::format_ticks;

/// The headroom kept above the tallest bar or point, as the catalog charts
/// keep above theirs.
const TOP_GAP: f32 = 10.;
/// The widest one bar in a group. The catalog caps a bar at 30px, which on
/// a dashboard's full-width row leaves a few categories as slivers lost in
/// the space between them; a wider bar fills the row without turning into a
/// slab.
const MAX_BAR_WIDTH: f32 = 56.;
/// The axis and bar labels' font size. The catalog draws 10px, which is hard
/// to read under a dashboard plot — and for CJK labels most of all.
const LABEL_SIZE: f32 = 12.;
const LABEL_HEIGHT: f32 = LABEL_SIZE + TEXT_GAP;
/// The gap between two bars of one group: enough to tell the series apart,
/// not enough to split the group.
const GROUP_GAP: f32 = 2.;
/// The dot a scatter paints per point, and the one the tooltip marks a hovered
/// point with — sized as the catalog's dotted line and hover dots are.
const DOT_SIZE: f32 = 8.;
/// The ring behind a hovered dot at full hover, as the catalog draws it.
const HOVER_HALO: f32 = 20.;
/// The value-axis ticks the grid is drawn at, the catalog's default.
const TICK_COUNT: usize = 5;
/// Room kept between a numeric x axis's ends and its first and last dots, so
/// an extreme point is not cut in half by the plot's edge.
const X_INSET: f32 = DOT_SIZE;
/// How round a bar's tip is: enough to soften it, not enough to make a thin
/// bar a pill.
const BAR_RADIUS: f32 = 4.;

/// The gutter a plot reserves under itself for its x-axis labels, as the
/// catalog computes it for the labels' font size.
fn axis_gap() -> f32 {
    axis_gutter(px(LABEL_SIZE))
}

/// The dashboard's chart palette, shared with the results panel's charts.
fn palette(cx: &App) -> [Hsla; crate::ui::palette::SLOTS] {
    crate::ui::palette::series(cx)
}

/// Within a band of `band_width`, the width of one of `series` side-by-side
/// bars and the offset of the `s`-th from the band's left edge.
fn group_slot(band_width: f32, series: usize, s: usize) -> (f32, f32) {
    let n = series.max(1) as f32;
    let width = ((band_width - GROUP_GAP * (n - 1.)) / n).max(0.);
    (width, s as f32 * (width + GROUP_GAP))
}

/// `count` evenly spaced value-axis positions, from the plot's top edge to
/// the baseline, both included; the last is the baseline the axis itself draws.
fn value_ticks(top: f32, baseline: f32, count: usize) -> Vec<f32> {
    let count = count.max(2);
    let steps = (count - 1) as f32;
    (0..count)
        .map(|i| top + (baseline - top) * i as f32 / steps)
        .collect()
}

/// Round values for a value axis over `min..=max` (zero included): about
/// `count` steps of 1, 2 or 5 times a power of ten, from a multiple of the
/// step at or below the data to one at or above it, so the axis ends on a
/// round number instead of on the largest value.
fn nice_ticks(min: f64, max: f64, count: usize) -> Vec<f64> {
    nice_ticks_over(min.min(0.), max.max(0.), count)
}

/// [`nice_ticks`] over the data alone, zero left out unless the data reach
/// it: a scatter's axes show where its points are, not how far they sit from
/// zero.
fn nice_ticks_over(min: f64, max: f64, count: usize) -> Vec<f64> {
    if !(min.is_finite() && max.is_finite()) {
        return vec![0.];
    }
    if max - min <= 0. {
        return vec![min];
    }
    let rough = (max - min) / count.max(1) as f64;
    let power = 10f64.powf(rough.log10().floor());
    // The multiple whose step count lands nearest `count`, as d3 picks it:
    // the cut-offs are the geometric means between 1, 2, 5 and 10.
    let error = rough / power;
    let multiple = if error >= 50f64.sqrt() {
        10.
    } else if error >= 10f64.sqrt() {
        5.
    } else if error >= 2f64.sqrt() {
        2.
    } else {
        1.
    };
    let step = multiple * power;
    let first = (min / step).floor() as i64;
    let last = (max / step).ceil() as i64;
    // Below one, divide by the power's inverse rather than multiply by the
    // power: 3 × 0.2 is 0.6000000000000001, 6 / 10 is 0.6.
    let tick = |k: i64| {
        if power >= 1. {
            k as f64 * step
        } else {
            k as f64 * multiple / (1. / power).round()
        }
    };
    (first..=last).map(tick).collect()
}

/// An x label as the axis shows it: a date whose year every band shares
/// drops the year, which the dates around it already say — `09-14` rather
/// than `2026-09-14`, and twice as many fit. The tooltip keeps the full band.
fn axis_label(band: &SharedString, shared_year: bool) -> SharedString {
    if shared_year {
        band[5..].to_string().into()
    } else {
        band.clone()
    }
}

/// Whether every band is an ISO date (or a timestamp starting with one) of
/// the same year.
fn one_year(points: &[Arc<PlotPoint>]) -> bool {
    let is_date = |b: &str| {
        let b = b.as_bytes();
        b.len() >= 10
            && b[..4].iter().all(u8::is_ascii_digit)
            && b[4] == b'-'
            && b[7] == b'-'
            && b[5..7].iter().all(u8::is_ascii_digit)
            && b[8..10].iter().all(u8::is_ascii_digit)
    };
    let mut years = points.iter().map(|p| {
        let band: &str = &p.band;
        is_date(band).then(|| band[..4].to_string())
    });
    match years.next() {
        Some(Some(first)) => years.all(|y| y.as_deref() == Some(&first)),
        _ => false,
    }
}

/// How many x labels of up to `widest` characters fit across `width`
/// pixels, for a plot that knows its width.
fn x_label_count_in(width: f32, widest: usize) -> usize {
    const MAX_LABELS: usize = 12;
    let slot = widest as f32 * 8. + 24.;
    ((width / slot) as usize).clamp(2, MAX_LABELS)
}

/// How many x labels fit without neighbours touching: a label needs its text
/// width — roughly 8px per character at the axis font — plus padding, and the
/// plot is at least 600px wide (the minimum window minus sidebar, padding and
/// the axis gutter). Feeding a count rather than a stride lets the labeling
/// below keep the first and last value, where a stride drops the first.
pub(crate) fn x_label_count(points: &[Arc<PlotPoint>]) -> usize {
    const MIN_PLOT_WIDTH: f32 = 600.;
    const MAX_LABELS: usize = 12;
    let widest = points
        .iter()
        .map(|p| p.band.chars().count())
        .max()
        .unwrap_or(0) as f32;
    let slot = widest * 8. + 24.;
    ((MIN_PLOT_WIDTH / slot) as usize).clamp(2, MAX_LABELS)
}

/// Which of `len` items carry an x-axis label: `count` of them spread evenly
/// from the first to the last, as the catalog charts place a tick count.
fn labeled(len: usize, count: usize) -> Vec<bool> {
    let mut labeled = vec![false; len];
    match count.min(len) {
        0 => {}
        1 => labeled[0] = true,
        n if n >= len => labeled.iter_mut().for_each(|l| *l = true),
        n => {
            for k in 0..n {
                let ix = (k as f32 * (len - 1) as f32 / (n - 1) as f32).round() as usize;
                labeled[ix] = true;
            }
        }
    }
    labeled
}

/// The alignment of the `i`-th of `len` x labels: the first hugs the left
/// edge, the last the right, the rest center on their tick, as the catalog's
/// point charts place them.
fn point_label_align(i: usize, len: usize) -> TextAlign {
    match i {
        0 if len == 1 => TextAlign::Center,
        0 => TextAlign::Left,
        i if i == len - 1 => TextAlign::Right,
        _ => TextAlign::Center,
    }
}

/// A `bar` plot, one series or several: one band per x value, a group's
/// series side by side within it. Every bar plot draws here — one series is
/// one colour, as one series of a line is one stroke — so a colour only ever
/// says which series a bar is. Bars are plain quads, cheap enough to paint
/// uncached; the hover band and the tooltip come from the plot's id.
///
/// A category's label is never dropped: when the labels do not fit under
/// their bars the plot lies on its side ([`Layout::Rows`]), each label left of
/// its bar. Only a time axis thins its labels, because a skipped date can be
/// read off its neighbours and a skipped category cannot.
pub(crate) struct GroupedBars {
    id: ElementId,
    points: Vec<Arc<PlotPoint>>,
    series: Vec<SharedString>,
    /// Whether the bands are points in time: always upright, labels thinned.
    time_axis: bool,
    /// The band a filter has picked: it keeps its colour and the rest fade,
    /// so the plot says what the dashboard is narrowed to.
    selected: Option<SharedString>,
    /// How the tooltip reads values; the bar labels came formatted.
    format: Option<NumberFormat>,
    /// A histogram's: the bands are ranges one after another, so the bars
    /// touch, stand upright and thin their labels, and carry no value —
    /// the shape is the point, and the tooltip has the counts.
    bins: bool,
    /// A `stacked_bar`'s: a band's series stack into one bar, each from
    /// where the one before it ended, and the bar is labelled by its total.
    stacked: bool,
}

/// One bar the plot paints: series `s` of band `d`, from `lo` to `hi` on
/// the value scale. A grouped bar runs from zero to its value; a stacked
/// segment from the running total before it to the one after.
#[derive(Clone)]
struct Mark {
    d: Arc<PlotPoint>,
    s: usize,
    lo: f64,
    hi: f64,
    /// Whether it sits on another segment, which a gap of surface keeps it
    /// apart from.
    on_another: bool,
    /// Whether it ends the bar on its side of zero, where the corners round.
    outer: bool,
    /// What is written past its end, when values are shown.
    label: Option<SharedString>,
}

/// How much of its opacity a bar outside the picked band keeps.
const UNPICKED_OPACITY: f32 = 0.3;
/// The space kept between a label and its neighbour, or its bar.
const LABEL_PAD: f32 = 8.;
/// A lying plot's label column: at least this wide, at most this share of the
/// plot, so a long path cannot squeeze its bars to nothing.
const MIN_GUTTER: f32 = 40.;
const MAX_GUTTER_SHARE: f32 = 0.4;

/// How a bar plot lies in a given box.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Layout {
    /// Bands across, bars up from the baseline, labels underneath.
    Columns,
    /// Bands down, bars rightwards: the labels in a `gutter` on the left,
    /// `room` kept right of the longest bar for its value.
    Rows { gutter: f32, room: f32 },
}

/// Roughly how wide `text` draws at the axis font, a wide (CJK) character
/// counting twice. An estimate rather than a measurement, so the layout can be
/// decided where no window is at hand — a click, a hover — and agree there
/// with what was painted.
fn label_width(text: &str) -> f32 {
    use unicode_width::UnicodeWidthStr;
    text.width() as f32 * LABEL_SIZE * 0.62
}

impl GroupedBars {
    pub(crate) fn new(id: impl Into<ElementId>, plot: &PreparedPlot) -> Self {
        let mut this =
            Self::of(id, plot.points.clone(), &plot.series_names).time_axis(plot.time_axis);
        this.format = plot.format;
        // The plot's type says how its bars lie, so the drawing and a click's
        // hit test, each built from the plot, agree.
        this.stacked = plot.kind == "stacked_bar";
        this.bins = plot.kind == "histogram";
        this
    }

    /// Bars over `points` directly, for a caller that holds no `PreparedPlot`
    /// — the results panel's chart tab.
    pub(crate) fn of(
        id: impl Into<ElementId>,
        points: Vec<Arc<PlotPoint>>,
        series_names: &[String],
    ) -> Self {
        Self {
            id: id.into(),
            points,
            series: series_names.iter().map(SharedString::from).collect(),
            time_axis: false,
            selected: None,
            format: None,
            bins: false,
            stacked: false,
        }
    }

    pub(crate) fn time_axis(mut self, time_axis: bool) -> Self {
        self.time_axis = time_axis;
        self
    }

    /// Draw the bands as a histogram's ranges.
    pub(crate) fn bins(mut self) -> Self {
        self.bins = true;
        self
    }

    /// Stack a band's series into one bar.
    pub(crate) fn stacked(mut self) -> Self {
        self.stacked = true;
        self
    }

    /// A band's total, the sum of the series it has.
    fn total(d: &PlotPoint) -> f64 {
        d.values
            .iter()
            .zip(&d.present)
            .filter(|(_, present)| **present)
            .map(|(value, _)| value)
            .sum()
    }

    /// The value written past band `d`'s bar: its total when stacked.
    fn value_label(&self, d: &PlotPoint) -> SharedString {
        if self.stacked {
            short_text(self.format, Self::total(d)).into()
        } else {
            d.label.clone()
        }
    }

    pub(crate) fn selected(mut self, band: Option<SharedString>) -> Self {
        self.selected = band;
        self
    }

    /// One series labels each bar with its value; several would crowd the
    /// band, and the tooltip lists them. A histogram's many narrow bars do
    /// so only while every count fits across its bar, `width` the plot's.
    /// A stack shows its total, one label a band, while every total fits.
    fn values_shown(&self, width: f32) -> bool {
        if self.series.len() != 1 && !self.stacked {
            return false;
        }
        if !self.bins && !self.stacked {
            return true;
        }
        let widest = self
            .points
            .iter()
            .map(|d| label_width(&self.value_label(d)))
            .fold(0., f32::max);
        widest + LABEL_PAD <= width / self.points.len().max(1) as f32
    }

    /// Bands in an order of their own — times, ranges — whose labels may be
    /// thinned: one skipped can be read off its neighbours.
    fn ordered(&self) -> bool {
        self.time_axis || self.bins
    }

    /// Upright when every band's label — and its value, when shown — fits
    /// across its band; on its side otherwise. A time axis stays upright.
    fn layout(&self, bounds: Bounds<Pixels>) -> Layout {
        if self.ordered() || self.points.is_empty() {
            return Layout::Columns;
        }
        let width = bounds.size.width.as_f32();
        let widest = |text: fn(&PlotPoint) -> &str| {
            self.points
                .iter()
                .map(|d| label_width(text(d)))
                .fold(0., f32::max)
        };
        let band = widest(|d| &d.band);
        let value = if self.values_shown(width) {
            self.points
                .iter()
                .map(|d| label_width(&self.value_label(d)))
                .fold(0., f32::max)
        } else {
            0.
        };
        let step = width / self.points.len() as f32;
        if band.max(value) + LABEL_PAD <= step {
            return Layout::Columns;
        }
        Layout::Rows {
            gutter: (band + LABEL_PAD).clamp(MIN_GUTTER, width * MAX_GUTTER_SHARE),
            room: value + LABEL_PAD,
        }
    }

    /// The band under `position` — relative to the plot's origin, as the
    /// tooltip's is — for a click to pick. An upright plot's axis labels
    /// below the baseline are not a band; a lying plot's labels are their row.
    pub(crate) fn band_at(
        &self,
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
    ) -> Option<SharedString> {
        let (x, y) = (position.x.as_f32(), position.y.as_f32());
        let (band_scale, _) = self.scales(bounds);
        let index = match self.layout(bounds) {
            Layout::Columns => {
                let baseline = bounds.size.height.as_f32() - axis_gap();
                if y > baseline || y < 0. {
                    return None;
                }
                band_scale.nearest_index(x)
            }
            Layout::Rows { .. } => {
                if y < 0. || y > bounds.size.height.as_f32() {
                    return None;
                }
                band_scale.nearest_index(y)
            }
        };
        self.points.get(index).map(|d| d.band.clone())
    }

    /// The band and value scales for `bounds`, shared by `paint` and the
    /// tooltip so the bars and the hover band stay aligned. The value scale
    /// spans the data and zero, so a bar always grows from the zero line.
    fn scales(&self, bounds: Bounds<Pixels>) -> (ScaleBand<SharedString>, ScaleLinear<f64>) {
        let width = bounds.size.width.as_f32();
        let height = bounds.size.height.as_f32();
        let (band_range, value_range) = match self.layout(bounds) {
            Layout::Columns => {
                let top = TOP_GAP
                    + if self.values_shown(width) {
                        LABEL_HEIGHT
                    } else {
                        0.
                    };
                ([0., width], [height - axis_gap(), top])
            }
            Layout::Rows { gutter, room } => ([0., height], [gutter, (width - room).max(gutter)]),
        };
        // The paddings are the catalog `BarChart`'s, so a band sits where a
        // single-series chart would put it; the cap keeps each bar of the
        // group at most MAX_BAR_WIDTH, however wide the plot.
        // A histogram's ranges meet, so its bars nearly do: the gap left is
        // the surface line between two fills.
        let (inner, outer, widest) = if self.bins {
            (0.04, 0.02, f32::INFINITY)
        } else if self.stacked {
            (0.4, 0.2, MAX_BAR_WIDTH)
        } else {
            (0.4, 0.2, MAX_BAR_WIDTH * self.series.len().max(1) as f32)
        };
        let band = ScaleBand::new(self.points.iter().map(|d| d.band.clone()), band_range)
            .padding_inner(inner)
            .padding_outer(outer)
            .max_band_width(widest);
        // A stack reaches as far as its parts on one side of zero add up to.
        let ends: Vec<f64> = if self.stacked {
            self.marks().iter().map(|m| m.hi).collect()
        } else {
            self.points
                .iter()
                .flat_map(|d| d.values.iter().copied())
                .collect()
        };
        let value = ScaleLinear::new(ends.into_iter().chain(Some(0.)), value_range);
        (band, value)
    }

    /// A histogram's axis: the edges between its ranges, each under the
    /// line two bars meet at — `$0`, `$20K`, `$40K` — rather than a range
    /// under each bar, which would say every edge twice. Every k-th edge
    /// once they would touch, so the ones shown stay evenly spaced.
    fn paint_edges(
        &self,
        band_scale: &ScaleBand<SharedString>,
        baseline: f32,
        width: f32,
        bounds: &Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let muted = cx.theme().muted_foreground;
        // A band reads `lo–hi`; the edges are every lo and the last hi.
        let mut edges: Vec<(SharedString, f32)> = Vec::with_capacity(self.points.len() + 1);
        for d in &self.points {
            let (Some(tick), Some((lo, _))) = (band_scale.tick(&d.band), d.band.split_once('–'))
            else {
                continue;
            };
            edges.push((lo.to_string().into(), tick));
        }
        if let Some(last) = self.points.last() {
            if let (Some(tick), Some((_, hi))) =
                (band_scale.tick(&last.band), last.band.split_once('–'))
            {
                edges.push((hi.to_string().into(), tick + band_scale.band_width()));
            }
        }
        let widest = edges
            .iter()
            .map(|(text, _)| label_width(text))
            .fold(0., f32::max);
        let step = width / self.points.len().max(1) as f32;
        let every = ((widest + LABEL_PAD) / step.max(1.)).ceil().max(1.) as usize;
        let last = edges.len().saturating_sub(1);
        let labels = edges
            .into_iter()
            .enumerate()
            .filter(|(i, _)| i % every == 0)
            .map(|(i, (text, x))| {
                Text::new(text, point(px(x), px(baseline + TEXT_GAP)), muted)
                    .font_size(px(LABEL_SIZE))
                    .align(point_label_align(i, last + 1))
            })
            .collect();
        PlotLabel::new(labels).paint(bounds, window, cx);
    }

    /// The bars themselves, in either layout: each series its colour, faded
    /// outside a picked band; the corners round only where a bar ends, and a
    /// stacked segment keeps a line of surface from the one under it.
    #[allow(clippy::too_many_arguments)]
    fn paint_bars(
        &self,
        layout: Layout,
        band_scale: ScaleBand<SharedString>,
        value_scale: ScaleLinear<f64>,
        zero: f32,
        bounds: &Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let band_width = band_scale.band_width();
        let n = self.series.len();
        let stacked = self.stacked;
        let bar_width = if stacked {
            band_width
        } else {
            group_slot(band_width, n, 0).0
        };
        let upright = layout == Layout::Columns;
        let shown = self.values_shown(bounds.size.width.as_f32());
        let palette = palette(cx);
        let muted = cx.theme().muted_foreground;
        let (ends, middles): (Vec<Mark>, Vec<Mark>) =
            self.marks().into_iter().partition(|m| m.outer);
        for (marks, round) in [(middles, false), (ends, true)] {
            let (band_scale, value_scale) = (band_scale.clone(), value_scale.clone());
            let base_scale = value_scale.clone();
            let selected = self.selected.clone();
            let corners = match (round, upright) {
                (false, _) => Corners::default(),
                (true, true) => Corners {
                    top_left: px(BAR_RADIUS),
                    top_right: px(BAR_RADIUS),
                    ..Corners::default()
                },
                (true, false) => Corners {
                    top_right: px(BAR_RADIUS),
                    bottom_right: px(BAR_RADIUS),
                    ..Corners::default()
                },
            };
            let mut bar = Bar::new()
                .data(marks)
                .band_width(bar_width)
                .cross(move |m: &Mark| {
                    let tick = band_scale.tick(&m.d.band)?;
                    Some(if stacked {
                        tick
                    } else {
                        tick + group_slot(band_width, n, m.s).1
                    })
                })
                .base(move |m: &Mark| {
                    let base = base_scale.tick(&m.lo).unwrap_or(zero);
                    let end = base_scale.tick(&m.hi).unwrap_or(base);
                    if m.on_another {
                        base + (end - base).signum() * GROUP_GAP.min((end - base).abs() / 2.)
                    } else {
                        base
                    }
                })
                .value(move |m: &Mark| value_scale.tick(&m.hi))
                .corner_radii(corners)
                .fill(move |m: &Mark, _: Bounds<f32>, _: BarAlignment| {
                    let color = palette[m.s % palette.len()];
                    match &selected {
                        Some(band) if *band != m.d.band => color.opacity(UNPICKED_OPACITY),
                        _ => color,
                    }
                });
            if !upright {
                bar = bar.alignment(BarAlignment::Left);
            }
            if shown {
                bar = bar.label(move |m: &Mark, at| {
                    let Some(text) = m.label.clone() else {
                        return Vec::new();
                    };
                    // A negative bar ends left of zero; its value reads
                    // leftwards from there.
                    let align = match (upright, m.hi < 0.) {
                        (true, _) => TextAlign::Center,
                        (false, true) => TextAlign::Right,
                        (false, false) => TextAlign::Left,
                    };
                    vec![Text::new(text, at, muted)
                        .font_size(px(LABEL_SIZE))
                        .align(align)]
                });
            }
            bar.paint(bounds, window, cx);
        }
    }

    /// Only the cells the query returned: a series a band lacks draws no bar
    /// there, rather than a zero the data never stated. A stack keeps its
    /// positive parts above zero and its negative ones below, each side
    /// adding up from zero in series order.
    fn marks(&self) -> Vec<Mark> {
        let mut marks = Vec::new();
        for d in &self.points {
            let first = marks.len();
            let (mut up, mut down) = (0f64, 0f64);
            for s in 0..self.series.len() {
                if !d.present.get(s).copied().unwrap_or(false) {
                    continue;
                }
                let value = d.values[s];
                let (lo, hi) = if !self.stacked {
                    (0., value)
                } else if value >= 0. {
                    up += value;
                    (up - value, up)
                } else {
                    down += value;
                    (down - value, down)
                };
                marks.push(Mark {
                    d: d.clone(),
                    s,
                    lo,
                    hi,
                    on_another: lo != 0.,
                    outer: !self.stacked,
                    label: (!self.stacked).then(|| d.label.clone()),
                });
            }
            if self.stacked {
                // The last part on each side ends the bar there; the total
                // goes past the end the bar reaches furthest on, above zero
                // unless nothing is.
                let band = &mut marks[first..];
                let last_up = band.iter().rposition(|m| m.hi > 0.);
                let last_down = band.iter().rposition(|m| m.hi < 0.);
                for ix in last_up.iter().chain(&last_down) {
                    band[*ix].outer = true;
                }
                if let Some(ix) = last_up.or(last_down) {
                    band[ix].label = Some(self.value_label(d));
                }
            }
        }
        marks
    }
}

impl IntoElement for GroupedBars {
    type Element = PlotElement<Self>;

    fn into_element(self) -> Self::Element {
        PlotElement::new(self)
    }
}

impl Plot for GroupedBars {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let layout = self.layout(bounds);
        let (band_scale, value_scale) = self.scales(bounds);
        let band_width = band_scale.band_width();
        let width = bounds.size.width.as_f32();
        let height = bounds.size.height.as_f32();
        let zero_default = match layout {
            Layout::Columns => height - axis_gap(),
            Layout::Rows { gutter, .. } => gutter,
        };
        let zero = value_scale.tick(&0.).unwrap_or(zero_default);
        let muted = cx.theme().muted_foreground;
        match layout {
            Layout::Columns => {
                let baseline = height - axis_gap();
                // The axis line sits at zero, which is mid-plot when the data
                // crosses it; the band labels stay at the bottom, clear of any
                // bar.
                PlotAxis::new()
                    .stroke(cx.theme().border)
                    .x(px(zero))
                    .paint(&bounds, window, cx);
                if self.bins {
                    self.paint_edges(&band_scale, baseline, width, &bounds, window, cx);
                }
                let shared_year = self.time_axis && one_year(&self.points);
                let shown = if self.bins {
                    vec![false; self.points.len()]
                } else if self.ordered() {
                    let widest = self
                        .points
                        .iter()
                        .map(|d| axis_label(&d.band, shared_year).chars().count())
                        .max()
                        .unwrap_or(0);
                    labeled(self.points.len(), x_label_count_in(width, widest))
                } else {
                    vec![true; self.points.len()]
                };
                let labels = self
                    .points
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| shown[*i])
                    .filter_map(|(_, d)| {
                        let tick = band_scale.tick(&d.band)?;
                        Some(
                            Text::new(
                                axis_label(&d.band, shared_year),
                                point(px(tick + band_width / 2.), px(baseline + TEXT_GAP)),
                                muted,
                            )
                            .font_size(px(LABEL_SIZE))
                            .align(TextAlign::Center),
                        )
                    })
                    .collect();
                PlotLabel::new(labels).paint(&bounds, window, cx);

                // The grid skips the baseline, which the axis line already draws.
                let top = TOP_GAP
                    + if self.values_shown(width) {
                        LABEL_HEIGHT
                    } else {
                        0.
                    };
                let ticks = value_ticks(top, baseline, TICK_COUNT);
                Grid::new()
                    .y(ticks[..ticks.len() - 1].to_vec())
                    .stroke(cx.theme().chart_grid)
                    .paint(&bounds, window);

                self.paint_bars(layout, band_scale, value_scale, zero, &bounds, window, cx);
            }
            Layout::Rows { gutter, room } => {
                PlotAxis::new()
                    .stroke(cx.theme().border)
                    .y(px(zero))
                    .y_axis(true)
                    .paint(&bounds, window, cx);
                // Every label while a row is a line of text tall; past that,
                // every k-th, as many as the rows can hold.
                let step = band_scale.step().max(1.);
                let every = (LABEL_HEIGHT / step).ceil().max(1.) as usize;
                let room_for_label = gutter - LABEL_PAD;
                let labels = self
                    .points
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| i % every == 0)
                    .filter_map(|(_, d)| {
                        let tick = band_scale.tick(&d.band)?;
                        let text =
                            truncate_text_to_width(&d.band, px(LABEL_SIZE), room_for_label, window);
                        Some(
                            Text::new(
                                text,
                                point(
                                    px(gutter - LABEL_PAD / 2.),
                                    px(tick + band_width / 2. - LABEL_SIZE / 2.),
                                ),
                                muted,
                            )
                            .font_size(px(LABEL_SIZE))
                            .align(TextAlign::Right),
                        )
                    })
                    .collect();
                PlotLabel::new(labels).paint(&bounds, window, cx);

                // Vertical grid lines at the value ticks, the axis excluded.
                let ticks = value_ticks((width - room).max(gutter), gutter, TICK_COUNT);
                Grid::new()
                    .x(ticks[..ticks.len() - 1].to_vec())
                    .stroke(cx.theme().chart_grid)
                    .paint(&bounds, window);

                self.paint_bars(layout, band_scale, value_scale, zero, &bounds, window, cx);
            }
        }
    }

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn tooltip_state(
        &self,
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _cx: &App,
    ) -> Option<TooltipState> {
        let (band_scale, _) = self.scales(bounds);
        match self.layout(bounds) {
            Layout::Columns => {
                // The axis labels below the baseline are not a band.
                let baseline = bounds.size.height.as_f32() - axis_gap();
                if position.y.as_f32() > baseline {
                    return None;
                }
                let index = band_scale.nearest_index(position.x.as_f32());
                let d = self.points.get(index)?;
                let center = band_scale.tick(&d.band)? + band_scale.band_width() / 2.;
                Some(TooltipState::new(
                    index,
                    point(px(center), position.y),
                    vec![],
                ))
            }
            Layout::Rows { .. } => {
                let index = band_scale.nearest_index(position.y.as_f32());
                let d = self.points.get(index)?;
                let center = band_scale.tick(&d.band)? + band_scale.band_width() / 2.;
                Some(TooltipState::new(
                    index,
                    point(position.x, px(center)),
                    vec![],
                ))
            }
        }
    }

    fn tooltip(
        &self,
        state: &TooltipState,
        cursor: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let d = self.points.get(state.index)?;
        let (band_scale, _) = self.scales(bounds);
        let palette = palette(cx);

        // The hovered band highlights whole, the way the catalog's bar chart
        // highlights its bar; the tooltip lists the series the band has.
        let cross_line = match self.layout(bounds) {
            Layout::Columns => {
                CrossLine::new(state.cross_line).span(0., bounds.size.height.as_f32() - axis_gap())
            }
            Layout::Rows { gutter, .. } => CrossLine::new(state.cross_line)
                .horizontal()
                .h_span(gutter, bounds.size.width.as_f32() - gutter),
        };
        let mut tooltip = Tooltip::new(cursor, bounds.size)
            .gap(px(8.))
            .cross_line(cross_line.band(px(band_scale.band_width())))
            .title(d.band.clone());
        for (s, name) in self.series.iter().enumerate() {
            if d.present.get(s).copied().unwrap_or(false) {
                tooltip = tooltip.row(
                    palette[s % palette.len()],
                    name.clone(),
                    full_text(self.format, d.values[s]),
                );
            }
        }
        if self.stacked {
            tooltip = tooltip.plain_row(
                crate::i18n::tr("chart.total"),
                full_text(self.format, Self::total(d)),
            );
        }
        Some(tooltip.into_any_element())
    }
}

/// The surface left between two heatmap cells, so neighbours of one colour
/// still read as two cells.
const CELL_GAP: f32 = 2.;
/// How round a heatmap cell's corners are.
const CELL_RADIUS: f32 = 2.;
/// The smallest cell a value is written in, past the label's own width.
const CELL_LABEL_PAD: f32 = 6.;

/// A `heatmap`: x's bands across, the series down, each cell coloured by its
/// value on one hue, more darker (lighter on a dark surface). A cell the
/// query did not return stays empty rather than reading as the least value.
/// The value is written in a cell wide enough to hold it; the tooltip names
/// every cell's band, series and value.
pub(crate) struct Heatmap {
    id: ElementId,
    points: Vec<Arc<PlotPoint>>,
    series: Vec<SharedString>,
    time_axis: bool,
    format: Option<NumberFormat>,
    /// The least and the most value of the cells present.
    range: (f64, f64),
}

impl Heatmap {
    pub(crate) fn new(id: impl Into<ElementId>, plot: &PreparedPlot) -> Self {
        let mut this =
            Self::of(id, plot.points.clone(), &plot.series_names).time_axis(plot.time_axis);
        this.format = plot.format;
        this
    }

    /// A heatmap over `points` directly, for the results panel's chart tab.
    pub(crate) fn of(
        id: impl Into<ElementId>,
        points: Vec<Arc<PlotPoint>>,
        series_names: &[String],
    ) -> Self {
        let range = heat_range(&points);
        Self {
            id: id.into(),
            points,
            series: series_names.iter().map(SharedString::from).collect(),
            time_axis: false,
            format: None,
            range,
        }
    }

    pub(crate) fn time_axis(mut self, time_axis: bool) -> Self {
        self.time_axis = time_axis;
        self
    }

    /// Where `value` sits between the least and the most cell, 0 to 1. One
    /// value everywhere takes the ramp's middle: nothing is least or most.
    fn share(&self, value: f64) -> f32 {
        let (min, max) = self.range;
        if max > min {
            ((value - min) / (max - min)) as f32
        } else {
            0.5
        }
    }

    /// The row labels' column on the left, the column labels' gutter below,
    /// and one cell's width and height.
    fn grid(&self, bounds: Bounds<Pixels>) -> (f32, f32, f32, f32) {
        let width = bounds.size.width.as_f32();
        let height = bounds.size.height.as_f32();
        let widest = self
            .series
            .iter()
            .map(|name| label_width(name))
            .fold(0., f32::max);
        let gutter = (widest + LABEL_PAD).clamp(MIN_GUTTER, width * MAX_GUTTER_SHARE);
        let bottom = axis_gap();
        let columns = self.points.len().max(1) as f32;
        let rows = self.series.len().max(1) as f32;
        let cell_width = ((width - gutter) / columns).max(0.);
        let cell_height = ((height - bottom - TOP_GAP) / rows).max(0.);
        (gutter, bottom, cell_width, cell_height)
    }

    /// The (band, series) cell under `position`, relative to the plot.
    fn cell_at(&self, position: Point<Pixels>, bounds: Bounds<Pixels>) -> Option<(usize, usize)> {
        let (gutter, _, cell_width, cell_height) = self.grid(bounds);
        let (x, y) = (position.x.as_f32() - gutter, position.y.as_f32() - TOP_GAP);
        if x < 0. || y < 0. || cell_width <= 0. || cell_height <= 0. {
            return None;
        }
        let (b, s) = ((x / cell_width) as usize, (y / cell_height) as usize);
        (b < self.points.len() && s < self.series.len()).then_some((b, s))
    }
}

/// The least and the most value a heatmap's present cells hold.
fn heat_range(points: &[Arc<PlotPoint>]) -> (f64, f64) {
    points
        .iter()
        .flat_map(|d| {
            d.values
                .iter()
                .zip(&d.present)
                .filter(|(_, present)| **present)
                .map(|(value, _)| *value)
        })
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(v), hi.max(v))
        })
}

/// A heatmap's key: the colours of its least, middle and most value, each
/// named by the value it stands for.
pub(crate) fn heat_key(
    points: &[Arc<PlotPoint>],
    format: Option<NumberFormat>,
    cx: &App,
) -> Vec<(Hsla, SharedString)> {
    let (min, max) = heat_range(points);
    if !(min.is_finite() && max.is_finite()) {
        return Vec::new();
    }
    if max <= min {
        let (fill, _) = crate::ui::palette::sequential(0.5, cx);
        return vec![(fill, full_text(format, min).into())];
    }
    [0., 0.5, 1.]
        .into_iter()
        .map(|t| {
            let (fill, _) = crate::ui::palette::sequential(t, cx);
            let value = min + (max - min) * t as f64;
            (fill, SharedString::from(full_text(format, value)))
        })
        .collect()
}

impl IntoElement for Heatmap {
    type Element = PlotElement<Self>;

    fn into_element(self) -> Self::Element {
        PlotElement::new(self)
    }
}

impl Plot for Heatmap {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let (gutter, bottom, cell_width, cell_height) = self.grid(bounds);
        let height = bounds.size.height.as_f32();
        let muted = cx.theme().muted_foreground;
        let origin = bounds.origin;

        // Cells, and a value in each one wide and tall enough for it.
        let mut values = Vec::new();
        for (b, d) in self.points.iter().enumerate() {
            for s in 0..self.series.len() {
                if !d.present.get(s).copied().unwrap_or(false) {
                    continue;
                }
                let value = d.values[s];
                let (fill_color, ink) = crate::ui::palette::sequential(self.share(value), cx);
                let left = gutter + b as f32 * cell_width;
                let top = TOP_GAP + s as f32 * cell_height;
                let cell = Bounds::new(
                    point(origin.x + px(left), origin.y + px(top)),
                    size(
                        px((cell_width - CELL_GAP).max(1.)),
                        px((cell_height - CELL_GAP).max(1.)),
                    ),
                );
                window.paint_quad(fill(cell, fill_color).corner_radii(px(CELL_RADIUS)));
                let text = short_text(self.format, value);
                if label_width(&text) + CELL_LABEL_PAD <= cell_width && LABEL_HEIGHT <= cell_height
                {
                    values.push(
                        Text::new(
                            text,
                            point(
                                px(left + (cell_width - CELL_GAP) / 2.),
                                px(top + (cell_height - CELL_GAP - LABEL_SIZE) / 2.),
                            ),
                            ink,
                        )
                        .font_size(px(LABEL_SIZE))
                        .align(TextAlign::Center),
                    );
                }
            }
        }
        PlotLabel::new(values).paint(&bounds, window, cx);

        // Series down the left, each beside its row while rows are a line of
        // text tall; every k-th past that.
        let every = (LABEL_HEIGHT / cell_height.max(1.)).ceil().max(1.) as usize;
        let rows = self
            .series
            .iter()
            .enumerate()
            .filter(|(s, _)| s % every == 0)
            .map(|(s, name)| {
                let text = truncate_text_to_width(name, px(LABEL_SIZE), gutter - LABEL_PAD, window);
                Text::new(
                    text,
                    point(
                        px(gutter - LABEL_PAD / 2.),
                        px(TOP_GAP
                            + s as f32 * cell_height
                            + (cell_height - CELL_GAP - LABEL_SIZE) / 2.),
                    ),
                    muted,
                )
                .font_size(px(LABEL_SIZE))
                .align(TextAlign::Right)
            })
            .collect();
        PlotLabel::new(rows).paint(&bounds, window, cx);

        // Bands along the bottom, each under its column while the widest
        // fits across one; every k-th past that, so the labelled columns stay
        // evenly spaced. The tooltip names the rest.
        let shared_year = self.time_axis && one_year(&self.points);
        let widest = self
            .points
            .iter()
            .map(|d| label_width(&axis_label(&d.band, shared_year)))
            .fold(0., f32::max);
        let every = ((widest + LABEL_PAD) / cell_width.max(1.)).ceil().max(1.) as usize;
        let columns = self
            .points
            .iter()
            .enumerate()
            .filter(|(b, _)| b % every == 0)
            .map(|(b, d)| {
                Text::new(
                    axis_label(&d.band, shared_year),
                    point(
                        px(gutter + b as f32 * cell_width + (cell_width - CELL_GAP) / 2.),
                        px(height - bottom + TEXT_GAP),
                    ),
                    muted,
                )
                .font_size(px(LABEL_SIZE))
                .align(TextAlign::Center)
            })
            .collect();
        PlotLabel::new(columns).paint(&bounds, window, cx);
    }

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn tooltip_state(
        &self,
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _cx: &App,
    ) -> Option<TooltipState> {
        let (b, s) = self.cell_at(position, bounds)?;
        if !self.points[b].present.get(s).copied().unwrap_or(false) {
            return None;
        }
        let (gutter, _, cell_width, _) = self.grid(bounds);
        let center = gutter + b as f32 * cell_width + (cell_width - CELL_GAP) / 2.;
        Some(TooltipState::new(
            b * self.series.len() + s,
            point(px(center), position.y),
            vec![],
        ))
    }

    fn tooltip(
        &self,
        state: &TooltipState,
        cursor: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let n = self.series.len().max(1);
        let (b, s) = (state.index / n, state.index % n);
        let d = self.points.get(b)?;
        let (_, bottom, cell_width, _) = self.grid(bounds);
        let value = d.values[s];
        let (fill_color, _) = crate::ui::palette::sequential(self.share(value), cx);
        let cross_line = CrossLine::new(state.cross_line)
            .band(px((cell_width - CELL_GAP).max(1.)))
            .span(TOP_GAP, bounds.size.height.as_f32() - bottom - TOP_GAP);
        Some(
            Tooltip::new(cursor, bounds.size)
                .gap(px(8.))
                .cross_line(cross_line)
                .title(d.band.clone())
                .row(
                    fill_color,
                    self.series[s].clone(),
                    full_text(self.format, value),
                )
                .into_any_element(),
        )
    }
}

/// Whether a series' points are connected. `prepare` hands bands, not
/// numbers, so the x axis is categorical either way: a `scatter` spreads its
/// points across the same point scale a `line` uses, honestly unconnected.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SeriesConnect {
    Line,
    Scatter,
}

/// A `line` or `scatter` plot over one or more series, on shared scales: one
/// stroke per series, or one dot per point. Lines are tessellated through the
/// window's path caches, as the catalog's are, so a repaint while the
/// dashboard scrolls costs the quads, not the curves.
pub(crate) struct SeriesPlot {
    id: ElementId,
    connect: SeriesConnect,
    points: Vec<Arc<PlotPoint>>,
    series: Vec<SharedString>,
    /// Each point's x as a number, for a scatter over a numeric column: the x
    /// axis is then linear, its labels round numbers.
    xs: Option<Arc<Vec<f64>>>,
    /// How the value axis and the tooltip read values.
    format: Option<NumberFormat>,
}

/// Where a series plot puts its points along x: evenly spaced bands, or a
/// number's place on a linear axis.
#[derive(Clone)]
enum XScale {
    Bands(ScalePoint<SharedString>),
    Numbers {
        scale: ScaleLinear<f64>,
        xs: Arc<Vec<f64>>,
    },
}

impl XScale {
    fn tick_at(&self, i: usize) -> Option<f32> {
        match self {
            XScale::Bands(scale) => scale.tick_at(i),
            XScale::Numbers { scale, xs } => scale.tick(xs.get(i)?),
        }
    }

    fn nearest_index(&self, x: f32) -> usize {
        match self {
            XScale::Bands(scale) => scale.nearest_index(x),
            XScale::Numbers { .. } => (0..self.len())
                .min_by(|&a, &b| {
                    let off = |i| self.tick_at(i).map_or(f32::MAX, |t| (t - x).abs());
                    off(a).total_cmp(&off(b))
                })
                .unwrap_or(0),
        }
    }

    fn len(&self) -> usize {
        match self {
            XScale::Bands(_) => 0,
            XScale::Numbers { xs, .. } => xs.len(),
        }
    }
}

/// A series plot's value axis: the round values it labels and grids, and the
/// gutter left of the plot their labels take.
struct ValueAxis {
    ticks: Vec<f64>,
    /// The ticks' labels, in one unit (see `format_ticks`).
    labels: Vec<String>,
    gutter: f32,
}

impl SeriesPlot {
    fn new(
        id: impl Into<ElementId>,
        points: Vec<Arc<PlotPoint>>,
        series_names: &[String],
        connect: SeriesConnect,
    ) -> Self {
        Self {
            id: id.into(),
            connect,
            points,
            series: series_names.iter().map(SharedString::from).collect(),
            xs: None,
            format: None,
        }
    }

    pub(crate) fn lines(id: impl Into<ElementId>, plot: &PreparedPlot) -> Self {
        let mut this = Self::lines_of(id, plot.points.clone(), &plot.series_names);
        this.format = plot.format;
        this
    }

    pub(crate) fn scatter(id: impl Into<ElementId>, plot: &PreparedPlot) -> Self {
        let mut this = Self::scatter_of(id, plot.points.clone(), &plot.series_names);
        this.xs = plot.x_values.clone();
        this.format = plot.format;
        this
    }

    /// Lines over `points` directly, for a caller that holds no
    /// `PreparedPlot` — the results panel's chart tab.
    pub(crate) fn lines_of(
        id: impl Into<ElementId>,
        points: Vec<Arc<PlotPoint>>,
        series_names: &[String],
    ) -> Self {
        Self::new(id, points, series_names, SeriesConnect::Line)
    }

    /// Dots over `points` directly; see [`Self::lines_of`].
    pub(crate) fn scatter_of(
        id: impl Into<ElementId>,
        points: Vec<Arc<PlotPoint>>,
        series_names: &[String],
    ) -> Self {
        Self::new(id, points, series_names, SeriesConnect::Scatter)
    }

    /// Every value a series has, skipping cells the query did not return.
    fn values(&self) -> impl Iterator<Item = f64> + '_ {
        self.points.iter().flat_map(|d| {
            d.values
                .iter()
                .zip(&d.present)
                .filter_map(|(value, present)| present.then_some(*value))
        })
    }

    /// The value axis: round ticks over the data and zero, and a gutter as
    /// wide as the widest tick label.
    fn value_axis(&self) -> ValueAxis {
        let ticks = if self.xs.is_some() {
            // A numeric scatter's y spans its points, as its x does.
            let (min, max) = self
                .values()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                    (lo.min(v), hi.max(v))
                });
            nice_ticks_over(min, max, TICK_COUNT - 1)
        } else {
            let (min, max) = self
                .values()
                .fold((0f64, 0f64), |(lo, hi), v| (lo.min(v), hi.max(v)));
            nice_ticks(min, max, TICK_COUNT - 1)
        };
        let labels = match self.format {
            Some(format) => format.ticks(&ticks),
            None => format_ticks(&ticks),
        };
        let widest = labels
            .iter()
            .map(|label| label_width(label))
            .fold(0., f32::max);
        ValueAxis {
            ticks,
            labels,
            gutter: widest + LABEL_PAD,
        }
    }

    /// The point (x) and value (y) scales for `bounds`, shared by `paint` and
    /// the tooltip so the series and the hover dots stay aligned. Both are in
    /// the whole plot's coordinates: x starts right of the value labels'
    /// gutter, and y spans the round ticks, so the top gridline is a round
    /// number at or above the largest value.
    fn scales(&self, bounds: Bounds<Pixels>, axis: &ValueAxis) -> (XScale, ScaleLinear<f64>) {
        let height = bounds.size.height.as_f32() - axis_gap();
        let width = bounds.size.width.as_f32();
        let range = [axis.gutter.min(width) + X_INSET, width - X_INSET];
        let x = match &self.xs {
            Some(xs) => XScale::Numbers {
                scale: ScaleLinear::new(xs.iter().copied().chain(self.x_ticks(width)), range),
                xs: xs.clone(),
            },
            None => XScale::Bands(ScalePoint::new(
                self.points.iter().map(|d| d.band.clone()),
                [axis.gutter.min(width), width],
            )),
        };
        let y = ScaleLinear::new(
            self.values().chain(axis.ticks.iter().copied()),
            [height, TOP_GAP],
        );
        (x, y)
    }

    /// A numeric x axis's round values, as many as the width has room for.
    fn x_ticks(&self, width: f32) -> Vec<f64> {
        let Some(xs) = &self.xs else {
            return Vec::new();
        };
        let (Some(first), Some(last)) = (xs.first(), xs.last()) else {
            return Vec::new();
        };
        let count = ((width / 90.) as usize).clamp(2, 8);
        nice_ticks_over(*first, *last, count)
    }

    /// Which series the hovered band has, in series order: the dots
    /// `tooltip_state` collected are in the same order, so the two line up.
    fn present_series(&self, index: usize) -> impl Iterator<Item = (usize, &SharedString)> {
        let d = self.points.get(index);
        self.series
            .iter()
            .enumerate()
            .filter(move |(s, _)| d.is_some_and(|d| d.present.get(*s).copied().unwrap_or(false)))
    }
}

impl IntoElement for SeriesPlot {
    type Element = PlotElement<Self>;

    fn into_element(self) -> Self::Element {
        PlotElement::new(self)
    }
}

impl Plot for SeriesPlot {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let axis = self.value_axis();
        let (x, y) = self.scales(bounds, &axis);
        let height = bounds.size.height.as_f32() - axis_gap();
        let width = bounds.size.width.as_f32();
        let palette = palette(cx);
        let muted = cx.theme().muted_foreground;
        // The axis line and the grid run right of the value labels, not
        // through them.
        let gutter = axis.gutter.min(width);
        let plot_area = Bounds::new(
            bounds.origin + point(px(gutter), px(0.)),
            size(px(width - gutter), bounds.size.height),
        );

        // As many x labels as fit the width this plot has, a shared year
        // dropped from dates.
        let shared_year = one_year(&self.points);
        let widest = self
            .points
            .iter()
            .map(|d| axis_label(&d.band, shared_year).chars().count())
            .max()
            .unwrap_or(0);
        let shown = labeled(self.points.len(), x_label_count_in(width - gutter, widest));
        let labels: Vec<AxisText> = match &x {
            // Round numbers along a numeric axis, not the points' own values.
            XScale::Numbers { scale, .. } => {
                let ticks = self.x_ticks(width);
                let names = format_ticks(&ticks);
                ticks
                    .iter()
                    .zip(names)
                    .enumerate()
                    .filter_map(|(i, (t, name))| {
                        let tick = scale.tick(t)? - gutter;
                        Some(
                            AxisText::new(name, px(tick), muted)
                                .font_size(px(LABEL_SIZE))
                                .align(point_label_align(i, ticks.len())),
                        )
                    })
                    .collect()
            }
            XScale::Bands(_) => self
                .points
                .iter()
                .enumerate()
                .filter(|(i, _)| shown[*i])
                .filter_map(|(i, d)| {
                    let tick = x.tick_at(i)? - gutter;
                    Some(
                        AxisText::new(axis_label(&d.band, shared_year), px(tick), muted)
                            .font_size(px(LABEL_SIZE))
                            .align(point_label_align(i, self.points.len())),
                    )
                })
                .collect(),
        };
        PlotAxis::new()
            .stroke(cx.theme().border)
            .x(px(height))
            .x_label(labels)
            .paint(&plot_area, window, cx);

        // A gridline and a label at every round value; the baseline's line
        // is the axis's.
        let positions: Vec<(&str, f32)> = axis
            .ticks
            .iter()
            .zip(&axis.labels)
            .filter_map(|(t, label)| Some((label.as_str(), y.tick(t)?)))
            .collect();
        Grid::new()
            .y(positions
                .iter()
                .filter(|(_, at)| (*at - height).abs() > 0.5)
                .map(|(_, at)| px(*at))
                .collect::<Vec<_>>())
            .stroke(cx.theme().chart_grid)
            .paint(&plot_area, window);
        let value_labels = positions
            .iter()
            .map(|(t, at)| {
                Text::new(
                    t.to_string(),
                    point(px(gutter - LABEL_PAD / 2.), px(at - LABEL_SIZE / 2.)),
                    muted,
                )
                .font_size(px(LABEL_SIZE))
                .align(TextAlign::Right)
            })
            .collect();
        PlotLabel::new(value_labels).paint(&bounds, window, cx);

        match self.connect {
            SeriesConnect::Line => {
                // A cell the query did not return is skipped, not drawn as
                // zero; the line bridges the gap, as the catalog's line does
                // over filtered data. Straight from point to point: a curve
                // through them would show values between the points that the
                // data never had.
                let caches = PathCaches::for_paint("spec-lines", window, cx);
                caches.update(cx, |caches, _| {
                    for (s, _) in self.series.iter().enumerate() {
                        let (x, y) = (x.clone(), y.clone());
                        let line = Line::new()
                            .data(self.points.iter().enumerate())
                            .x(move |(i, _)| x.tick_at(*i))
                            .y(move |(_, d)| {
                                d.present
                                    .get(s)
                                    .copied()
                                    .unwrap_or(false)
                                    .then(|| y.tick(&d.values[s]))
                                    .flatten()
                            })
                            .curve(Curve::Linear)
                            .stroke(palette[s % palette.len()])
                            .stroke_width(2.);
                        line.paint_cached(&bounds, caches.slot(s), window);
                    }
                });
            }
            SeriesConnect::Scatter => {
                // Dots and nothing else: the quads `Line` would paint, without
                // the path that would claim the points are connected. One
                // layer for all of them, so the scene orders them as a group
                // in paint order rather than placing each of up to 18,000 in
                // its bounds tree.
                window.paint_layer(bounds, |window| {
                    for (i, d) in self.points.iter().enumerate() {
                        let Some(x_tick) = x.tick_at(i) else { continue };
                        for s in 0..self.series.len() {
                            if !d.present.get(s).copied().unwrap_or(false) {
                                continue;
                            }
                            let Some(y_tick) = y.tick(&d.values[s]) else {
                                continue;
                            };
                            let color = palette[s % palette.len()];
                            let origin = bounds.origin
                                + point(px(x_tick - DOT_SIZE / 2.), px(y_tick - DOT_SIZE / 2.));
                            window.paint_quad(quad(
                                Bounds::new(origin, size(px(DOT_SIZE), px(DOT_SIZE))),
                                px(DOT_SIZE / 2.),
                                Background::from(color),
                                px(1.),
                                color,
                                BorderStyle::default(),
                            ));
                        }
                    }
                });
            }
        }
    }

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn tooltip_state(
        &self,
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _cx: &App,
    ) -> Option<TooltipState> {
        // The axis labels below the plot, and the value labels left of it,
        // are not a datum.
        let height = bounds.size.height.as_f32() - axis_gap();
        let axis = self.value_axis();
        if position.y.as_f32() > height || position.x.as_f32() < axis.gutter {
            return None;
        }
        let (x, y) = self.scales(bounds, &axis);
        let index = match &x {
            // Dots, not columns: the one nearest the pointer, across and up.
            XScale::Numbers { .. } => (0..self.points.len()).min_by(|&a, &b| {
                let off = |i: usize| {
                    let d = &self.points[i];
                    let s = d.present.iter().position(|p| *p).unwrap_or(0);
                    match (x.tick_at(i), y.tick(&d.values[s])) {
                        (Some(tx), Some(ty)) => {
                            (tx - position.x.as_f32()).powi(2) + (ty - position.y.as_f32()).powi(2)
                        }
                        _ => f32::MAX,
                    }
                };
                off(a).total_cmp(&off(b))
            })?,
            XScale::Bands(_) => x.nearest_index(position.x.as_f32()),
        };
        let d = self.points.get(index)?;
        let x_tick = x.tick_at(index)?;
        // One dot per series the band has, in series order; `tooltip` colors
        // them from the same ordering.
        let dots = self
            .present_series(index)
            .filter_map(|(s, _)| Some(point(px(x_tick), px(y.tick(&d.values[s])?))))
            .collect();
        Some(TooltipState::new(
            index,
            point(px(x_tick), position.y),
            dots,
        ))
    }

    fn tooltip(
        &self,
        state: &TooltipState,
        cursor: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let d = self.points.get(state.index)?;
        let height = bounds.size.height.as_f32() - axis_gap();
        let palette = palette(cx);
        let background = cx.theme().background;

        // The crosshair stays inside the plot; the dots mark the hovered
        // band's points, one per series the band has.
        let mut tooltip = Tooltip::new(cursor, bounds.size)
            .gap(px(8.))
            .cross_line(CrossLine::new(state.cross_line).height(height))
            .dots(
                state
                    .dots
                    .iter()
                    .zip(self.present_series(state.index))
                    .map(|(p, (s, _))| {
                        Dot::new(*p)
                            .size(px(DOT_SIZE))
                            .halo(px(HOVER_HALO))
                            .stroke(background)
                            .fill(palette[s % palette.len()])
                    }),
            )
            .title(d.band.clone());
        for (s, name) in self.present_series(state.index) {
            tooltip = tooltip.row(
                palette[s % palette.len()],
                name.clone(),
                full_text(self.format, d.values[s]),
            );
        }
        Some(tooltip.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: that pulls in `gpui_kit::*`, whose
    // `test` macro shadows the built-in `#[test]`.
    use super::{
        axis_label, group_slot, labeled, nice_ticks, one_year, point_label_align, value_ticks,
        x_label_count, x_label_count_in, GroupedBars, Layout, GROUP_GAP,
    };
    use crate::spec::prepare::PlotPoint;
    use gpui_kit::{point, px, size, Bounds, SharedString, TextAlign};
    use std::sync::Arc;

    #[test]
    fn a_click_finds_the_band_under_it() {
        let points: Vec<Arc<PlotPoint>> = ["a", "b", "c"]
            .iter()
            .enumerate()
            .map(|(ix, band)| {
                Arc::new(PlotPoint {
                    band: SharedString::from(*band),
                    label: SharedString::new(""),
                    values: vec![1.0],
                    present: vec![true],
                    ix,
                })
            })
            .collect();
        let bars = GroupedBars::of("t", points, &["y".to_string()]);
        let bounds = Bounds::new(point(px(100.), px(50.)), size(px(300.), px(200.)));
        let at = |x: f32, y: f32| bars.band_at(point(px(x), px(y)), bounds);
        assert_eq!(at(10., 100.).as_deref(), Some("a"));
        assert_eq!(at(150., 100.).as_deref(), Some("b"));
        assert_eq!(at(290., 100.).as_deref(), Some("c"));
        // The axis labels under the baseline are not a bar.
        assert_eq!(at(150., 199.), None);
    }

    #[test]
    fn a_stack_adds_up_each_side_of_zero_and_says_its_total() {
        // Three series in one band: 3 and 5 stack up, -2 hangs below zero;
        // a second band has only the middle series.
        let point = |band: &str, ix, values: Vec<f64>, present: Vec<bool>| {
            Arc::new(PlotPoint {
                band: SharedString::from(band),
                label: SharedString::new(""),
                values,
                present,
                ix,
            })
        };
        let points = vec![
            point("a", 0, vec![3., -2., 5.], vec![true; 3]),
            point("b", 1, vec![0., 4., 0.], vec![false, true, false]),
        ];
        let series = ["x".to_string(), "y".to_string(), "z".to_string()];
        let marks = GroupedBars::of("t", points, &series).stacked().marks();
        let spans: Vec<(usize, f64, f64, bool, bool)> = marks
            .iter()
            .map(|m| (m.s, m.lo, m.hi, m.on_another, m.outer))
            .collect();
        assert_eq!(
            spans,
            [
                (0, 0., 3., false, false),
                (1, 0., -2., false, true),
                (2, 3., 8., true, true),
                (1, 0., 4., false, true),
            ]
        );
        // The total, net of the part below zero, goes past the top.
        let labels: Vec<Option<&str>> = marks.iter().map(|m| m.label.as_deref()).collect();
        assert_eq!(labels, [None, None, Some("6"), Some("4")]);
    }

    fn bars(bands: &[&str]) -> GroupedBars {
        let points = bands
            .iter()
            .enumerate()
            .map(|(ix, band)| {
                Arc::new(PlotPoint {
                    band: SharedString::from(*band),
                    label: SharedString::from("1,234"),
                    values: vec![1.0],
                    present: vec![true],
                    ix,
                })
            })
            .collect();
        GroupedBars::of("t", points, &["y".to_string()])
    }

    #[test]
    fn categories_that_do_not_fit_lie_on_their_side() {
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(480.), px(240.)));
        // Six short kinds fit across 480px: upright, every label shown.
        let kinds = bars(&["fix", "feature", "docs", "refactor", "test", "build"]);
        assert_eq!(kinds.layout(bounds), Layout::Columns);
        // Fourteen paths do not: on its side, the labels in a left gutter
        // no wider than 40% of the plot.
        let areas: Vec<String> = (0..14)
            .map(|i| format!("src/app_export_{i} (removed)"))
            .collect();
        let areas = bars(&areas.iter().map(String::as_str).collect::<Vec<_>>());
        let Layout::Rows { gutter, room } = areas.layout(bounds) else {
            panic!("fourteen long labels cannot stand upright in 480px");
        };
        assert!((40.0..=192.).contains(&gutter), "{gutter}");
        assert!(room > 0.);
        // A click on a row, or on its label, picks that row's band.
        let at = |y: f32| areas.band_at(point(px(10.), px(y)), bounds);
        assert_eq!(at(1.).as_deref(), Some("src/app_export_0 (removed)"));
        assert_eq!(at(239.).as_deref(), Some("src/app_export_13 (removed)"));
        // The same labels on a time axis stay upright, to be thinned.
        let areas = areas.time_axis(true);
        assert_eq!(areas.layout(bounds), Layout::Columns);
    }

    #[test]
    fn a_group_tiles_its_band() {
        for series in 1..=12 {
            let (width, first) = group_slot(90., series, 0);
            assert_eq!(first, 0.);
            let (_, last) = group_slot(90., series, series - 1);
            // The last bar ends where the band ends.
            assert!((last + width - 90.).abs() < 1e-4, "{series} series");
            if series > 1 {
                let (_, second) = group_slot(90., series, 1);
                assert!((second - width - GROUP_GAP).abs() < 1e-4, "{series} series");
            }
        }
    }

    #[test]
    fn a_lone_bar_takes_the_band() {
        assert_eq!(group_slot(48., 1, 0), (48., 0.));
    }

    #[test]
    fn value_ticks_span_top_to_baseline() {
        assert_eq!(value_ticks(0., 100., 5), vec![0., 25., 50., 75., 100.]);
        assert_eq!(value_ticks(10., 110., 2), vec![10., 110.]);
    }

    #[test]
    fn edge_labels_hug_their_edges() {
        assert_eq!(point_label_align(0, 5), TextAlign::Left);
        assert_eq!(point_label_align(4, 5), TextAlign::Right);
        assert_eq!(point_label_align(2, 5), TextAlign::Center);
        assert_eq!(point_label_align(0, 1), TextAlign::Center);
    }

    #[test]
    fn labels_spread_from_first_to_last() {
        assert_eq!(labeled(4, 2), vec![true, false, false, true]);
        assert_eq!(labeled(5, 3), vec![true, false, true, false, true]);
        assert!(labeled(3, 12).iter().all(|l| *l));
        assert_eq!(labeled(0, 6), Vec::<bool>::new());
    }

    #[test]
    fn wide_labels_mean_fewer_ticks() {
        let point = |band: &str| {
            Arc::new(PlotPoint {
                band: band.into(),
                label: SharedString::default(),
                values: vec![1.],
                present: vec![true],
                ix: 0,
            })
        };
        // Ten-character dates get a handful of labels, short CJK bands more,
        // and the count never drops below the two edge labels.
        assert_eq!(x_label_count(&[point("2026-09-01")]), 5);
        assert_eq!(x_label_count(&[point("手机银行")]), 10);
        assert_eq!(x_label_count(&[point("")]), 12);
    }

    fn bands(bands: &[&str]) -> Vec<Arc<PlotPoint>> {
        bands
            .iter()
            .enumerate()
            .map(|(ix, band)| {
                Arc::new(PlotPoint {
                    band: SharedString::from(*band),
                    label: SharedString::new(""),
                    values: vec![1.0],
                    present: vec![true],
                    ix,
                })
            })
            .collect()
    }

    #[test]
    fn a_value_axis_steps_by_round_numbers_past_the_data() {
        assert_eq!(nice_ticks(0., 327., 4), vec![0., 100., 200., 300., 400.]);
        assert_eq!(nice_ticks(0., 8., 4), vec![0., 2., 4., 6., 8.]);
        assert_eq!(nice_ticks(0., 0.9, 4), vec![0., 0.2, 0.4, 0.6, 0.8, 1.]);
        // Zero is always on the axis, below negative data or above it.
        assert_eq!(nice_ticks(-30., -5., 4), vec![-30., -20., -10., 0.]);
        assert_eq!(nice_ticks(5., 5., 4), vec![0., 1., 2., 3., 4., 5.]);
        // Nothing to span: a single tick at zero.
        assert_eq!(nice_ticks(0., 0., 4), vec![0.]);
        assert_eq!(nice_ticks(0., f64::NAN, 4), vec![0.]);
    }

    #[test]
    fn dates_of_one_year_drop_it_on_the_axis() {
        let days = bands(&["2026-09-01", "2026-09-14"]);
        assert!(one_year(&days));
        assert_eq!(axis_label(&days[1].band, true).as_ref(), "09-14");
        // A timestamp counts by its date.
        assert!(one_year(&bands(&[
            "2026-09-01 10:00:00",
            "2026-09-02 00:00:00"
        ])));
        // Two years, or anything not a date, keep the whole label.
        assert!(!one_year(&bands(&["2025-12-31", "2026-01-01"])));
        assert!(!one_year(&bands(&["手机银行", "柜面"])));
        assert!(!one_year(&bands(&["2026", "2027"])));
        assert!(!one_year(&[]));
    }

    #[test]
    fn x_labels_fit_the_width_drawn() {
        // A half-width plot fits fewer dates than the full-width estimate.
        let full = x_label_count(&bands(&["2026-09-01"]));
        assert!(x_label_count_in(400., 10) < full);
        assert_eq!(x_label_count_in(400., 5), 6);
        // Never fewer than the two ends.
        assert_eq!(x_label_count_in(10., 10), 2);
    }
}
