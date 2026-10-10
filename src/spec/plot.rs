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

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::plot::label::{
    Text, TEXT_GAP, TEXT_HEIGHT, TEXT_SIZE, truncate_text_to_width,
};
use gpui_kit::component::plot::scale::{Scale, ScaleBand, ScaleLinear, ScalePoint};
use gpui_kit::component::plot::shape::{Bar, BarAlignment, Line};
use gpui_kit::component::plot::tooltip::{CrossLine, Dot, Tooltip, TooltipState};
use gpui_kit::component::plot::{
    AxisText, Grid, PathCaches, Plot, PlotAxis, PlotElement, PlotLabel, axis_gutter,
};
use gpui_kit::*;

use crate::spec::prepare::{PlotPoint, PreparedPlot};
use crate::ui::chart::format_value;

/// The headroom kept above the tallest bar or point, as the catalog charts
/// keep above theirs.
const TOP_GAP: f32 = 10.;
/// The widest one bar in a group: the catalog caps a single-series bar at
/// 30px, and a grouped bar is no different.
const MAX_BAR_WIDTH: f32 = 30.;
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

/// The gutter a plot reserves under itself for its x-axis labels, as the
/// catalog computes it for the labels' font size.
fn axis_gap() -> f32 {
    axis_gutter(px(TEXT_SIZE))
}

/// The dashboard's chart palette: series cycle `chart_1..chart_5`, as at the
/// catalog call sites.
fn palette(cx: &App) -> [Hsla; 5] {
    let theme = cx.theme();
    [
        theme.chart_1,
        theme.chart_2,
        theme.chart_3,
        theme.chart_4,
        theme.chart_5,
    ]
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
    label_count: usize,
    /// Whether the bands are points in time: always upright, labels thinned.
    time_axis: bool,
    /// The band a filter has picked: it keeps its colour and the rest fade,
    /// so the plot says what the dashboard is narrowed to.
    selected: Option<SharedString>,
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
    text.width() as f32 * TEXT_SIZE * 0.62
}

impl GroupedBars {
    pub(crate) fn new(id: impl Into<ElementId>, plot: &PreparedPlot) -> Self {
        Self::of(id, plot.points.clone(), &plot.series_names).time_axis(plot.time_axis)
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
            label_count: x_label_count(&points),
            points,
            series: series_names.iter().map(SharedString::from).collect(),
            time_axis: false,
            selected: None,
        }
    }

    pub(crate) fn time_axis(mut self, time_axis: bool) -> Self {
        self.time_axis = time_axis;
        self
    }

    pub(crate) fn selected(mut self, band: Option<SharedString>) -> Self {
        self.selected = band;
        self
    }

    /// One series labels each bar with its value; several would crowd the
    /// band, and the tooltip lists them.
    fn values_shown(&self) -> bool {
        self.series.len() == 1
    }

    /// Upright when every band's label — and its value, when shown — fits
    /// across its band; on its side otherwise. A time axis stays upright.
    fn layout(&self, bounds: Bounds<Pixels>) -> Layout {
        if self.time_axis || self.points.is_empty() {
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
        let value = if self.values_shown() { widest(|d| &d.label) } else { 0. };
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
                let top = TOP_GAP + if self.values_shown() { TEXT_HEIGHT } else { 0. };
                ([0., width], [height - axis_gap(), top])
            }
            Layout::Rows { gutter, room } => ([0., height], [gutter, (width - room).max(gutter)]),
        };
        // The paddings are the catalog `BarChart`'s, so a band sits where a
        // single-series chart would put it; the cap keeps each bar of the
        // group at most MAX_BAR_WIDTH, however wide the plot.
        let band = ScaleBand::new(self.points.iter().map(|d| d.band.clone()), band_range)
            .padding_inner(0.4)
            .padding_outer(0.2)
            .max_band_width(MAX_BAR_WIDTH * self.series.len().max(1) as f32);
        let value = ScaleLinear::new(
            self.points
                .iter()
                .flat_map(|d| d.values.iter().copied())
                .chain(Some(0.)),
            value_range,
        );
        (band, value)
    }

    /// Only the cells the query returned: a series a band lacks draws no bar
    /// there, rather than a zero the data never stated.
    fn bars(&self) -> Vec<(Arc<PlotPoint>, usize)> {
        let mut bars = Vec::new();
        for d in &self.points {
            for s in 0..self.series.len() {
                if d.present.get(s).copied().unwrap_or(false) {
                    bars.push((d.clone(), s));
                }
            }
        }
        bars
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
        let palette = palette(cx);
        let muted = cx.theme().muted_foreground;
        let n = self.series.len();
        let selected = self.selected.clone();
        let fill = move |d: &(Arc<PlotPoint>, usize), _: Bounds<f32>, _: BarAlignment| {
            let color = palette[d.1 % palette.len()];
            match &selected {
                Some(band) if *band != d.0.band => color.opacity(UNPICKED_OPACITY),
                _ => color,
            }
        };
        let (bar_width, _) = group_slot(band_width, n, 0);
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
                let shown = if self.time_axis {
                    labeled(self.points.len(), self.label_count)
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
                                d.band.clone(),
                                point(px(tick + band_width / 2.), px(baseline + TEXT_GAP)),
                                muted,
                            )
                            .align(TextAlign::Center),
                        )
                    })
                    .collect();
                PlotLabel::new(labels).paint(&bounds, window, cx);

                // The grid skips the baseline, which the axis line already draws.
                let top = TOP_GAP + if self.values_shown() { TEXT_HEIGHT } else { 0. };
                let ticks = value_ticks(top, baseline, TICK_COUNT);
                Grid::new()
                    .y(ticks[..ticks.len() - 1].to_vec())
                    .stroke(cx.theme().chart_grid)
                    .dash_array(&[px(4.), px(2.)])
                    .paint(&bounds, window);

                let mut bar = Bar::new()
                    .data(self.bars())
                    .band_width(bar_width)
                    .cross(move |d: &(Arc<PlotPoint>, usize)| {
                        band_scale
                            .tick(&d.0.band)
                            .map(|tick| tick + group_slot(band_width, n, d.1).1)
                    })
                    .base(move |_| zero)
                    .value(move |d: &(Arc<PlotPoint>, usize)| value_scale.tick(&d.0.values[d.1]))
                    .fill(fill);
                if self.values_shown() {
                    bar = bar.label(move |d: &(Arc<PlotPoint>, usize), at| {
                        vec![Text::new(d.0.label.clone(), at, muted).align(TextAlign::Center)]
                    });
                }
                bar.paint(&bounds, window, cx);
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
                let every = (TEXT_HEIGHT / step).ceil().max(1.) as usize;
                let room_for_label = gutter - LABEL_PAD;
                let labels = self
                    .points
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| i % every == 0)
                    .filter_map(|(_, d)| {
                        let tick = band_scale.tick(&d.band)?;
                        let text =
                            truncate_text_to_width(&d.band, px(TEXT_SIZE), room_for_label, window);
                        Some(
                            Text::new(
                                text,
                                point(
                                    px(gutter - LABEL_PAD / 2.),
                                    px(tick + band_width / 2. - TEXT_SIZE / 2.),
                                ),
                                muted,
                            )
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
                    .dash_array(&[px(4.), px(2.)])
                    .paint(&bounds, window);

                let mut bar = Bar::new()
                    .data(self.bars())
                    .alignment(BarAlignment::Left)
                    .band_width(bar_width)
                    .cross(move |d: &(Arc<PlotPoint>, usize)| {
                        band_scale
                            .tick(&d.0.band)
                            .map(|tick| tick + group_slot(band_width, n, d.1).1)
                    })
                    .base(move |_| zero)
                    .value(move |d: &(Arc<PlotPoint>, usize)| value_scale.tick(&d.0.values[d.1]))
                    .fill(fill);
                if self.values_shown() {
                    bar = bar.label(move |d: &(Arc<PlotPoint>, usize), at| {
                        // A negative bar ends left of zero; its value reads
                        // leftwards from there.
                        let align = if d.0.values[d.1] < 0. {
                            TextAlign::Right
                        } else {
                            TextAlign::Left
                        };
                        vec![Text::new(d.0.label.clone(), at, muted).align(align)]
                    });
                }
                bar.paint(&bounds, window, cx);
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
                Some(TooltipState::new(index, point(px(center), position.y), vec![]))
            }
            Layout::Rows { .. } => {
                let index = band_scale.nearest_index(position.y.as_f32());
                let d = self.points.get(index)?;
                let center = band_scale.tick(&d.band)? + band_scale.band_width() / 2.;
                Some(TooltipState::new(index, point(position.x, px(center)), vec![]))
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
            Layout::Columns => CrossLine::new(state.cross_line)
                .span(0., bounds.size.height.as_f32() - axis_gap()),
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
                    format_value(d.values[s]),
                );
            }
        }
        Some(tooltip.into_any_element())
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
    label_count: usize,
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
            label_count: x_label_count(&points),
            points,
            series: series_names.iter().map(SharedString::from).collect(),
        }
    }

    pub(crate) fn lines(id: impl Into<ElementId>, plot: &PreparedPlot) -> Self {
        Self::lines_of(id, plot.points.clone(), &plot.series_names)
    }

    pub(crate) fn scatter(id: impl Into<ElementId>, plot: &PreparedPlot) -> Self {
        Self::scatter_of(id, plot.points.clone(), &plot.series_names)
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

    /// The point (x) and value (y) scales for `bounds`, shared by `paint` and
    /// the tooltip so the series and the hover dots stay aligned. The value
    /// scale fits the data from zero, as the catalog's point charts do.
    fn scales(&self, bounds: Bounds<Pixels>) -> (ScalePoint<SharedString>, ScaleLinear<f64>) {
        let height = bounds.size.height.as_f32() - axis_gap();
        let x = ScalePoint::new(
            self.points.iter().map(|d| d.band.clone()),
            [0., bounds.size.width.as_f32()],
        );
        let y = ScaleLinear::new(
            self.points
                .iter()
                .flat_map(|d| {
                    d.values
                        .iter()
                        .zip(&d.present)
                        .filter_map(|(value, present)| present.then_some(*value))
                })
                .chain(Some(0.)),
            [height, TOP_GAP],
        );
        (x, y)
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
        let (x, y) = self.scales(bounds);
        let height = bounds.size.height.as_f32() - axis_gap();
        let palette = palette(cx);

        let shown = labeled(self.points.len(), self.label_count);
        let labels = self
            .points
            .iter()
            .enumerate()
            .filter(|(i, _)| shown[*i])
            .filter_map(|(i, d)| {
                let tick = x.tick_at(i)?;
                Some(
                    AxisText::new(d.band.clone(), px(tick), cx.theme().muted_foreground)
                        .align(point_label_align(i, self.points.len())),
                )
            });
        PlotAxis::new()
            .stroke(cx.theme().border)
            .x(px(height))
            .x_label(labels)
            .paint(&bounds, window, cx);

        let ticks = value_ticks(0., height, TICK_COUNT);
        Grid::new()
            .y(ticks[..ticks.len() - 1].to_vec())
            .stroke(cx.theme().chart_grid)
            .dash_array(&[px(4.), px(2.)])
            .paint(&bounds, window);

        match self.connect {
            SeriesConnect::Line => {
                // A cell the query did not return is skipped, not drawn as
                // zero; the line bridges the gap, as the catalog's line does
                // over filtered data.
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
                            .stroke(palette[s % palette.len()])
                            .stroke_width(2.);
                        line.paint_cached(&bounds, caches.slot(s), window);
                    }
                });
            }
            SeriesConnect::Scatter => {
                // Dots and nothing else: the quads `Line` would paint, without
                // the path that would claim the points are connected.
                for (i, d) in self.points.iter().enumerate() {
                    let Some(x_tick) = x.tick_at(i) else { continue };
                    for s in 0..self.series.len() {
                        if !d.present.get(s).copied().unwrap_or(false) {
                            continue;
                        }
                        let Some(y_tick) = y.tick(&d.values[s]) else { continue };
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
        // The axis labels below the plot are not a datum.
        let height = bounds.size.height.as_f32() - axis_gap();
        if position.y.as_f32() > height {
            return None;
        }
        let (x, y) = self.scales(bounds);
        let index = x.nearest_index(position.x.as_f32());
        let d = self.points.get(index)?;
        let x_tick = x.tick_at(index)?;
        // One dot per series the band has, in series order; `tooltip` colors
        // them from the same ordering.
        let dots = self
            .present_series(index)
            .filter_map(|(s, _)| Some(point(px(x_tick), px(y.tick(&d.values[s])?))))
            .collect();
        Some(TooltipState::new(index, point(px(x_tick), position.y), dots))
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
                format_value(d.values[s]),
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
        GROUP_GAP, GroupedBars, Layout, group_slot, labeled, point_label_align, value_ticks,
        x_label_count,
    };
    use crate::spec::prepare::PlotPoint;
    use gpui_kit::{Bounds, SharedString, TextAlign, point, px, size};
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
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(400.), px(240.)));
        // Six short kinds fit across 400px: upright, every label shown.
        let kinds = bars(&["fix", "feature", "docs", "refactor", "test", "build"]);
        assert_eq!(kinds.layout(bounds), Layout::Columns);
        // Fourteen paths do not: on its side, the labels in a left gutter
        // no wider than 40% of the plot.
        let areas: Vec<String> = (0..14).map(|i| format!("src/app_export_{i} (removed)")).collect();
        let areas = bars(&areas.iter().map(String::as_str).collect::<Vec<_>>());
        let Layout::Rows { gutter, room } = areas.layout(bounds) else {
            panic!("fourteen long labels cannot stand upright in 400px");
        };
        assert!((40.0..=160.).contains(&gutter), "{gutter}");
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
}
