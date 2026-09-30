//! Chart tab of the results panel: renders the current row set as a chart.
//! The kind is picked by the user, or by `ChartKind::Auto`: a latitude and a
//! longitude column → a map ([`crate::ui::geo`]); first column temporal →
//! multi-series area chart (time series, one series per numeric column);
//! otherwise bar chart — first column as the band label, numeric columns as
//! the values. Line, scatter and pie are a click away.
//!
//! Deriving chart data walks every result row, so it happens once per result
//! set (`ChartData::prepare`) and is cached by the results panel. The panel
//! itself is rebuilt on every frame, and hands the cached rows to the chart as
//! `Rc`s so a frame costs refcount bumps rather than a copy of the row set.

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::chart::{AreaChart, BarChart, PieChart};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::{h_flex, v_flex, Icon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::i18n::{tr, trf};
use crate::query::{ColumnKind, QueryResult};
use crate::spec::plot::{GroupedBars, SeriesPlot};
use crate::spec::prepare::PlotPoint;
use crate::ui::geo::{GeoData, GeoPlot, MAX_GEO_POINTS};

/// What the chart tab draws. `Auto` resolves per result set, see
/// [`ChartData::resolve`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChartKind {
    #[default]
    Auto,
    Bar,
    Line,
    Area,
    Scatter,
    Pie,
    Map,
}

impl ChartKind {
    pub fn label(self) -> &'static str {
        tr(match self {
            ChartKind::Auto => "chart.kind.auto",
            ChartKind::Bar => "chart.kind.bar",
            ChartKind::Line => "chart.kind.line",
            ChartKind::Area => "chart.kind.area",
            ChartKind::Scatter => "chart.kind.scatter",
            ChartKind::Pie => "chart.kind.pie",
            ChartKind::Map => "chart.kind.map",
        })
    }
}

/// One pie slice: a band's first-series value, or the fold of the smallest.
#[derive(Debug)]
pub struct PieSlice {
    name: SharedString,
    value: f32,
    /// Palette slot; `None` for the folded "other" slice.
    slot: Option<usize>,
}

impl PieSlice {
    #[cfg(test)]
    pub(crate) fn value(&self) -> f32 {
        self.value
    }
}

/// A chart is at most a couple of thousand pixels wide, so plotting more
/// points than this cannot show more detail — it only multiplies the
/// primitives the renderer has to push. A wide `PIVOT` over a fortnight of
/// per-minute metrics reaches millions of plotted values, which is enough to
/// wedge the GPU renderer.
const MAX_POINTS: usize = 1_500;
/// The palette holds five colors; past a dozen lines a chart is unreadable
/// whatever the cost of drawing it.
const MAX_SERIES: usize = 12;
/// Bars are wider than line vertices, and unlike a time axis, adjacent bands
/// cannot be meaningfully merged — so surplus bars are dropped, not averaged.
const MAX_BARS: usize = 200;
/// How many detected columns the "nothing to plot" message lists before
/// summarizing the rest.
const MAX_LISTED_COLUMNS: usize = 5;
/// How many series names go into the chart title before summarizing the rest.
const MAX_TITLE_SERIES: usize = 3;
/// Past this many slices a pie is unreadable; the smallest fold into "other".
const MAX_SLICES: usize = 8;

/// Everything the chart needs, derived from a result set once.
pub struct ChartData {
    label_name: String,
    series_names: Vec<String>,
    is_time_series: bool,
    rows: Vec<Arc<PlotPoint>>,
    /// The first series as pie slices, largest first.
    pie: Vec<Arc<PieSlice>>,
    /// Rows the pie leaves out: a zero, negative or missing value is no slice.
    pie_skipped: usize,
    /// Set when the result has a latitude and a longitude column.
    geo: Option<Arc<GeoData>>,
    /// `name (type)` per column, for the "no numeric column" message. Only
    /// populated when there is nothing to plot.
    detected: Vec<String>,
    /// What was dropped or merged to keep the chart drawable, shown under the
    /// title. A chart that quietly plots something other than the query result
    /// would be worse than a slow one.
    notice: Option<String>,
}

impl ChartData {
    pub fn prepare(result: &QueryResult) -> Self {
        let geo = GeoData::detect(result).map(Arc::new);
        let mut value_ixes: Vec<usize> = result
            .columns
            .iter()
            .enumerate()
            .filter_map(|(ix, column)| (column.kind == ColumnKind::Numeric).then_some(ix))
            .collect();

        if value_ixes.is_empty() || result.columns.is_empty() {
            return Self {
                label_name: String::new(),
                series_names: Vec::new(),
                is_time_series: false,
                rows: Vec::new(),
                pie: Vec::new(),
                pie_skipped: 0,
                geo,
                detected: result
                    .columns
                    .iter()
                    .map(|column| format!("{} ({})", column.name, column.duck_type))
                    .collect(),
                notice: None,
            };
        }

        let mut notices: Vec<String> = Vec::new();
        let series_total = value_ixes.len();
        if series_total > MAX_SERIES {
            value_ixes.truncate(MAX_SERIES);
            notices.push(trf(
                "chart.notice.series_capped",
                &[&MAX_SERIES.to_string(), &series_total.to_string()],
            ));
        }

        // `(source row index, band, one value per series)`.
        let raw: Vec<(usize, SharedString, Vec<f64>)> = result
            .rows
            .iter()
            .enumerate()
            .filter_map(|(ix, row)| {
                let band = row.first()?;
                let values: Option<Vec<f64>> = value_ixes
                    .iter()
                    .map(|&col_ix| parse_number(row.get(col_ix)?))
                    .collect();
                Some((ix, band.clone().into(), values?))
            })
            .collect();

        let is_time_series = result.columns[0].kind == ColumnKind::Temporal;
        let plotted = if is_time_series {
            let source_points = raw.len();
            let reduced = bucket_average(raw, MAX_POINTS);
            if reduced.len() < source_points {
                let per_point = source_points.div_ceil(reduced.len().max(1));
                notices.push(trf(
                    "chart.notice.downsampled",
                    &[
                        &source_points.to_string(),
                        &reduced.len().to_string(),
                        &per_point.to_string(),
                    ],
                ));
            }
            reduced
        } else {
            let source_bars = raw.len();
            let mut bars = raw;
            if source_bars > MAX_BARS {
                bars.truncate(MAX_BARS);
                notices.push(trf(
                    "chart.notice.bars_capped",
                    &[&MAX_BARS.to_string(), &source_bars.to_string()],
                ));
            }
            bars
        };

        let rows: Vec<Arc<PlotPoint>> = plotted
            .into_iter()
            .map(|(ix, band, values)| {
                Arc::new(PlotPoint {
                    band,
                    label: format_value(values[0]).into(),
                    present: vec![true; values.len()],
                    values,
                    ix,
                })
            })
            .collect();
        let (pie, pie_skipped) = pie_slices(&rows);

        Self {
            label_name: result.columns[0].name.clone(),
            series_names: value_ixes
                .iter()
                .map(|&ix| result.columns[ix].name.clone())
                .collect(),
            is_time_series,
            rows,
            pie,
            pie_skipped,
            geo,
            detected: Vec::new(),
            notice: (!notices.is_empty()).then(|| notices.join(" · ")),
        }
    }

    /// Whether there is anything to chart at all; the kind picker is hidden
    /// otherwise.
    pub fn is_plottable(&self) -> bool {
        !self.series_names.is_empty()
    }

    /// The kinds this result can be drawn as, `Auto` first.
    pub fn available_kinds(&self) -> Vec<ChartKind> {
        let mut kinds = Vec::from([
            ChartKind::Auto,
            ChartKind::Bar,
            ChartKind::Line,
            ChartKind::Area,
            ChartKind::Scatter,
        ]);
        if !self.is_time_series {
            kinds.push(ChartKind::Pie);
        }
        if self.geo.is_some() {
            kinds.push(ChartKind::Map);
        }
        kinds
    }

    /// The concrete kind `kind` draws for this result: `Auto` picks the map
    /// when there are coordinates, an area chart over time, bars otherwise;
    /// a kind this result cannot draw (a map without coordinates, kept from a
    /// previous result) falls back to `Auto`'s pick.
    pub fn resolve(&self, kind: ChartKind) -> ChartKind {
        if kind != ChartKind::Auto && self.available_kinds().contains(&kind) {
            return kind;
        }
        if self.geo.is_some() {
            ChartKind::Map
        } else if self.is_time_series {
            ChartKind::Area
        } else {
            ChartKind::Bar
        }
    }

    #[cfg(test)]
    pub fn series_count_for_probe(&self) -> usize {
        self.series_names.len()
    }

    /// What `RenderOnce::render` hands to the chart each frame.
    #[cfg(test)]
    pub fn clone_rows_for_probe(&self) -> Vec<Arc<PlotPoint>> {
        self.rows.clone()
    }
}

/// The first series as pie slices, largest first, the smallest folded into
/// "other" past `MAX_SLICES`; plus how many rows had no positive value.
pub(crate) fn pie_slices(rows: &[Arc<PlotPoint>]) -> (Vec<Arc<PieSlice>>, usize) {
    let mut positive: Vec<(SharedString, f64)> = rows
        .iter()
        .filter(|r| r.values[0] > 0.)
        .map(|r| (r.band.clone(), r.values[0]))
        .collect();
    let skipped = rows.len() - positive.len();
    positive.sort_by(|a, b| b.1.total_cmp(&a.1));
    let folded = positive.len() > MAX_SLICES;
    let kept = if folded {
        MAX_SLICES - 1
    } else {
        positive.len()
    };
    let rest: f64 = positive[kept..].iter().map(|(_, v)| v).sum();
    let mut slices: Vec<Arc<PieSlice>> = positive
        .into_iter()
        .take(kept)
        .enumerate()
        .map(|(slot, (name, value))| {
            Arc::new(PieSlice {
                name,
                value: value as f32,
                slot: Some(slot),
            })
        })
        .collect();
    if folded {
        slices.push(Arc::new(PieSlice {
            name: tr("chart.map.other").into(),
            value: rest as f32,
            slot: None,
        }));
    }
    (slices, skipped)
}

/// Merge consecutive points into at most `max_points` buckets, averaging each
/// series within a bucket. All series share the x axis, so they must share
/// bucket boundaries; the bucket keeps the first point's band and source index.
fn bucket_average(
    rows: Vec<(usize, SharedString, Vec<f64>)>,
    max_points: usize,
) -> Vec<(usize, SharedString, Vec<f64>)> {
    if rows.len() <= max_points || max_points == 0 {
        return rows;
    }
    let bucket_size = rows.len().div_ceil(max_points);
    rows.chunks(bucket_size)
        .map(|bucket| {
            let (ix, band, first) = &bucket[0];
            let mut sums = vec![0.0; first.len()];
            for (_, _, values) in bucket {
                for (sum, value) in sums.iter_mut().zip(values) {
                    *sum += value;
                }
            }
            let n = bucket.len() as f64;
            for sum in sums.iter_mut() {
                *sum /= n;
            }
            (*ix, band.clone(), sums)
        })
        .collect()
}

/// Parse a rendered cell back to a number, tolerating thousands separators.
/// Avoids allocating unless the cell actually contains one.
pub(crate) fn parse_number(cell: &str) -> Option<f64> {
    if cell.contains(',') {
        cell.replace(',', "").parse().ok()
    } else {
        cell.parse().ok()
    }
}

#[derive(IntoElement)]
pub struct ChartPanel {
    data: Rc<ChartData>,
    kind: ChartKind,
}

impl ChartPanel {
    /// `kind` is what the user picked; the panel draws `data.resolve(kind)`.
    pub fn new(data: Rc<ChartData>, kind: ChartKind) -> Self {
        Self { data, kind }
    }
}

impl RenderOnce for ChartPanel {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let data = &self.data;

        if data.series_names.is_empty() {
            let detected = if data.detected.len() > MAX_LISTED_COLUMNS {
                trf(
                    "chart.empty.detected_more",
                    &[
                        &data.detected[..MAX_LISTED_COLUMNS].join(" · "),
                        &data.detected.len().to_string(),
                    ],
                )
            } else {
                data.detected.join(" · ")
            };
            return empty_chart_state(
                tr("chart.empty.no_numeric"),
                &[
                    trf("chart.empty.detected_columns", &[&detected]),
                    tr("chart.empty.hint").to_string(),
                ],
                cx,
            );
        }

        let kind = data.resolve(self.kind);
        if kind == ChartKind::Map {
            if let Some(geo) = data.geo.clone() {
                return render_map(geo, cx);
            }
        }

        if data.rows.is_empty() {
            return empty_chart_state(tr("chart.empty.no_rows"), &[], cx);
        }
        if kind == ChartKind::Pie {
            return render_pie(data, cx);
        }

        let palette = [
            cx.theme().chart_1,
            cx.theme().chart_2,
            cx.theme().chart_3,
            cx.theme().chart_4,
            cx.theme().chart_5,
        ];
        let row_count = data.rows.len();
        // Cloning the `Rc`s hands the chart its own `Vec` without copying any
        // row data.
        let rows = data.rows.clone();

        let chart = match kind {
            ChartKind::Line => SeriesPlot::lines_of("results-chart-line", rows, &data.series_names)
                .into_any_element(),
            ChartKind::Scatter => {
                SeriesPlot::scatter_of("results-chart-scatter", rows, &data.series_names)
                    .into_any_element()
            }
            ChartKind::Bar if data.series_names.len() > 1 => {
                GroupedBars::of("results-chart-bars", rows, &data.series_names).into_any_element()
            }
            ChartKind::Area => {
                // One area series per numeric column; ~10 x labels on dense data.
                // `.id` enables the built-in hover tooltip (crosshair + per-series rows).
                let tick_margin = (row_count / 10).max(1);
                let mut chart = AreaChart::new(rows)
                    .x(|d: &Arc<PlotPoint>| d.band.clone())
                    .id("results-chart");
                for (series_ix, series_name) in data.series_names.iter().enumerate() {
                    let color = palette[series_ix % palette.len()];
                    chart = chart
                        .y(move |d: &Arc<PlotPoint>| d.values[series_ix])
                        .stroke(color)
                        .fill(linear_gradient(
                            0.,
                            linear_color_stop(color.opacity(0.4), 1.),
                            linear_color_stop(color.opacity(0.05), 0.),
                        ))
                        .name(series_name.clone());
                }
                chart.tick_margin(tick_margin).into_any_element()
            }
            _ => BarChart::new(rows)
                .band(|d: &Arc<PlotPoint>| d.band.clone())
                .value(|d: &Arc<PlotPoint>| d.values[0])
                .fill(move |d: &Arc<PlotPoint>, _, _, _| palette[d.ix % palette.len()])
                .label(|d: &Arc<PlotPoint>| d.label.clone())
                .id("results-chart")
                .name(data.series_names[0].clone())
                .into_any_element(),
        };

        let label_name = data.label_name.clone();
        let notice = data.notice.clone();
        let series_title = if data.series_names.len() > MAX_TITLE_SERIES {
            trf(
                "chart.title.series_more",
                &[
                    &data.series_names[..MAX_TITLE_SERIES].join(", "),
                    &data.series_names.len().to_string(),
                ],
            )
        } else {
            data.series_names.join(", ")
        };
        // Several series need a key; one series is named in the title.
        let legend = (data.series_names.len() > 1
            && matches!(
                kind,
                ChartKind::Line | ChartKind::Scatter | ChartKind::Area | ChartKind::Bar
            ))
        .then(|| {
            data.series_names
                .iter()
                .enumerate()
                .map(|(ix, name)| {
                    (
                        palette[ix % palette.len()],
                        SharedString::from(name.clone()),
                    )
                })
                .collect::<Vec<_>>()
        });
        chart_frame(
            trf("chart.title.by", &[&series_title, &label_name]),
            trf("chart.title.point_count", &[&row_count.to_string()]),
            notice,
            legend.unwrap_or_default(),
            chart,
            cx,
        )
    }
}

/// The layout every kind shares: a title with a muted count beside it, an
/// optional notice and legend under it, and the chart filling the rest.
fn chart_frame(
    title: String,
    count: String,
    notice: Option<String>,
    legend: Vec<(Hsla, SharedString)>,
    chart: AnyElement,
    cx: &App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    v_flex()
        .size_full()
        .p_4()
        .gap_2()
        .child(
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .gap_2()
                        .items_baseline()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(div().text_sm().text_color(muted).child(count)),
                )
                .when_some(notice, |this, notice| {
                    this.child(div().text_xs().text_color(muted).child(notice))
                })
                .when(!legend.is_empty(), |this| this.child(legend_row(legend, cx))),
        )
        .child(div().flex_1().min_h_0().child(chart))
        .into_any_element()
}

/// What a map says under its title: capping, dropped rows, the color
/// column; and its legend. Shared with dashboards.
pub(crate) fn map_notes(geo: &GeoData, cx: &App) -> (Option<String>, Vec<(Hsla, SharedString)>) {
    let mut notices = Vec::new();
    if let Some(total) = geo.capped_from {
        notices.push(trf(
            "chart.map.notice.capped",
            &[&MAX_GEO_POINTS.to_string(), &total.to_string()],
        ));
    }
    if geo.dropped > 0 {
        notices.push(trf("chart.map.notice.dropped", &[&geo.dropped.to_string()]));
    }
    if let Some(name) = &geo.category_name {
        notices.push(trf("chart.map.colored_by", &[name]));
    }
    if let Some(name) = &geo.size_name {
        notices.push(trf("chart.map.sized_by", &[name]));
    }
    if geo.size_missing > 0 {
        notices.push(trf("chart.map.notice.unsized", &[&geo.size_missing.to_string()]));
    }
    let legend = geo
        .categories
        .iter()
        .enumerate()
        .map(|(slot, name)| (geo.category_color(slot, cx), name.clone()))
        .collect();
    ((!notices.is_empty()).then(|| notices.join(" · ")), legend)
}

fn render_map(geo: Arc<GeoData>, cx: &App) -> AnyElement {
    let (notice, legend) = map_notes(&geo, cx);
    chart_frame(
        trf("chart.map.title", &[&geo.lat_name, &geo.lng_name]),
        trf("chart.title.point_count", &[&geo.point_count().to_string()]),
        notice,
        legend,
        GeoPlot::new("results-chart-map", geo).into_any_element(),
        cx,
    )
}

/// A key of colored dots and names, wrapping as wide as it is given.
pub(crate) fn legend_row(legend: Vec<(Hsla, SharedString)>, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    h_flex()
        .flex_wrap()
        .gap_x_3()
        .gap_y_1()
        .children(legend.into_iter().map(|(color, name)| {
            h_flex()
                .gap_1()
                .items_center()
                .child(div().size_2().rounded_full().bg(color))
                .child(div().text_xs().text_color(muted).child(name))
        }))
        .into_any_element()
}

/// A pie over `slices` and its legend (each slice with its share), shared by
/// the results panel and dashboards so both color and fold slices alike.
/// Also returns the total the shares are of.
pub(crate) fn pie_parts(
    slices: &[Arc<PieSlice>],
    series: SharedString,
    id: impl Into<ElementId>,
    cx: &App,
) -> (AnyElement, Vec<(Hsla, SharedString)>, f64) {
    let palette = [
        cx.theme().chart_1,
        cx.theme().chart_2,
        cx.theme().chart_3,
        cx.theme().chart_4,
        cx.theme().chart_5,
    ];
    let muted = cx.theme().muted_foreground;
    // Past the palette, slices reuse its colors at lower strength so
    // neighbours still differ.
    let color = move |slice: &PieSlice| match slice.slot {
        Some(slot) if slot < palette.len() => palette[slot],
        Some(slot) => palette[slot % palette.len()].opacity(0.55),
        None => muted.opacity(0.6),
    };
    let total: f64 = slices.iter().map(|s| s.value as f64).sum();
    let legend = slices
        .iter()
        .map(|s| {
            let share = s.value as f64 / total * 100.;
            (
                color(s),
                SharedString::from(format!("{} · {share:.1}%", s.name)),
            )
        })
        .collect();
    let chart = PieChart::new(slices.to_vec())
        .value(|s: &Arc<PieSlice>| s.value)
        .color(move |s: &Arc<PieSlice>| color(s))
        .pad_angle(0.01)
        .tooltip_name(|s: &Arc<PieSlice>| s.name.clone())
        .name(series)
        .id(id)
        .into_any_element();
    (chart, legend, total)
}

fn render_pie(data: &ChartData, cx: &App) -> AnyElement {
    if data.pie.is_empty() {
        return empty_chart_state(tr("chart.pie.no_positive"), &[], cx);
    }
    let series = data.series_names[0].clone();
    let (chart, legend, total) =
        pie_parts(&data.pie, series.clone().into(), "results-chart-pie", cx);
    let notice = (data.pie_skipped > 0)
        .then(|| trf("chart.pie.notice.skipped", &[&data.pie_skipped.to_string()]));
    let notice = match (data.notice.clone(), notice) {
        (Some(a), Some(b)) => Some(format!("{a} · {b}")),
        (a, b) => a.or(b),
    };
    chart_frame(
        trf("chart.pie.title", &[&series, &data.label_name]),
        trf("chart.pie.total", &[&format_value(total)]),
        notice,
        legend,
        chart,
        cx,
    )
}

/// Shared empty-state layout: a medium-weight message over muted hints.
fn empty_chart_state(message: &str, hints: &[String], cx: &App) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .p_4()
        .gap_2()
        .child(
            Icon::new(gpui_kit::assets::IconName::ChartPie)
                .large()
                .text_color(cx.theme().muted_foreground.alpha(0.5)),
        )
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .text_center()
                .child(message.to_string()),
        )
        .children(hints.iter().map(|hint| {
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .text_center()
                .child(hint.clone())
                .into_any_element()
        }))
        .into_any_element()
}

pub(crate) fn format_value(value: f64) -> String {
    if value.abs() >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value.abs() >= 10_000.0 {
        format!("{:.0}K", value / 1_000.0)
    } else if value == value.trunc() {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: that pulls in `gpui_kit::*`, whose
    // `test` macro shadows the built-in `#[test]`.
    use super::{parse_number, ChartData, ChartKind, MAX_SLICES};
    use crate::query::{ColumnKind, ColumnMeta, QueryResult};

    fn column(name: &str, kind: ColumnKind) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            duck_type: format!("{kind:?}"),
            kind,
        }
    }

    fn result(columns: Vec<ColumnMeta>, rows: Vec<Vec<&str>>) -> QueryResult {
        QueryResult {
            columns,
            rows: rows
                .into_iter()
                .map(|row| row.into_iter().map(String::from).collect())
                .collect(),
            elapsed_ms: 0,
            truncated: false,
        }
    }

    #[test]
    fn temporal_first_column_becomes_a_time_series() {
        let data = ChartData::prepare(&result(
            Vec::from([
                column("d", ColumnKind::Temporal),
                column("amount", ColumnKind::Numeric),
                column("qty", ColumnKind::Numeric),
            ]),
            Vec::from([
                Vec::from(["2026-09-09", "1.5", "2"]),
                Vec::from(["2026-09-10", "2.5", "3"]),
            ]),
        ));
        assert!(data.is_time_series);
        assert_eq!(data.series_names, ["amount", "qty"]);
        assert_eq!(data.label_name, "d");
        assert_eq!(data.rows.len(), 2);
        assert_eq!(data.rows[0].values, [1.5, 2.0]);
        assert_eq!(data.rows[1].band, "2026-09-10");
    }

    #[test]
    fn text_first_column_becomes_a_bar_chart() {
        let data = ChartData::prepare(&result(
            Vec::from([
                column("city", ColumnKind::Text),
                column("total", ColumnKind::Numeric),
            ]),
            Vec::from([Vec::from(["北京", "1884201.4"])]),
        ));
        assert!(!data.is_time_series);
        assert_eq!(data.rows.len(), 1);
        // Bar labels are rendered once, at prepare time.
        assert_eq!(data.rows[0].label, "1.9M");
    }

    #[test]
    fn thousands_separators_still_parse() {
        assert_eq!(parse_number("1,884,201.40"), Some(1_884_201.40));
        assert_eq!(parse_number("42"), Some(42.0));
        assert_eq!(parse_number("NULL"), None);
    }

    #[test]
    fn without_a_numeric_column_there_is_nothing_to_plot() {
        let data = ChartData::prepare(&result(
            Vec::from([
                column("city", ColumnKind::Text),
                column("note", ColumnKind::Text),
            ]),
            Vec::from([Vec::from(["北京", "x"])]),
        ));
        assert!(data.series_names.is_empty());
        assert!(data.rows.is_empty());
        // The panel lists the columns it did find.
        assert_eq!(data.detected, ["city (Text)", "note (Text)"]);
    }

    #[test]
    fn dense_time_series_is_downsampled_and_says_so() {
        let rows: Vec<Vec<String>> = (0..20_160)
            .map(|m| {
                Vec::from([
                    format!("2026-08-19 {:02}:{:02}", m / 60 % 24, m % 60),
                    "10".into(),
                ])
            })
            .collect();
        let data = ChartData::prepare(&QueryResult {
            columns: Vec::from([
                column("ts", ColumnKind::Temporal),
                column("cpu", ColumnKind::Numeric),
            ]),
            rows,
            elapsed_ms: 0,
            truncated: false,
        });
        assert!(data.rows.len() <= super::MAX_POINTS);
        assert!(!data.rows.is_empty());
        // Averaging a constant series leaves the value untouched.
        assert!(data.rows.iter().all(|r| r.values[0] == 10.0));
        assert!(
            data.notice
                .as_deref()
                .unwrap_or_default()
                .contains("降采样"),
            "a downsampled chart must say so, got {:?}",
            data.notice
        );
    }

    #[test]
    fn bucket_average_means_each_series_over_its_bucket() {
        // Two series, four points, two buckets: means are (1,3)/2 and (5,7)/2.
        let rows = Vec::from([
            (0usize, "a".into(), Vec::from([1.0, 10.0])),
            (1, "b".into(), Vec::from([3.0, 30.0])),
            (2, "c".into(), Vec::from([5.0, 50.0])),
            (3, "d".into(), Vec::from([7.0, 70.0])),
        ]);
        let out = super::bucket_average(rows, 2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].2, [2.0, 20.0]);
        assert_eq!(out[1].2, [6.0, 60.0]);
        // Each bucket keeps its first point's band and source index.
        assert_eq!(out[0].1, "a");
        assert_eq!(out[1].0, 2);
    }

    #[test]
    fn a_point_per_row_is_left_alone() {
        let rows = Vec::from([(0usize, "a".into(), Vec::from([1.0]))]);
        assert_eq!(super::bucket_average(rows, 1_500).len(), 1);
    }

    #[test]
    fn too_many_series_are_capped_and_reported() {
        let mut columns = Vec::from([column("ts", ColumnKind::Temporal)]);
        let mut row = Vec::from([String::from("2026-08-19 04:00:00")]);
        for ix in 0..100 {
            columns.push(column(&format!("node-{ix}"), ColumnKind::Numeric));
            row.push(String::from("1"));
        }
        let data = ChartData::prepare(&QueryResult {
            columns,
            rows: Vec::from([row]),
            elapsed_ms: 0,
            truncated: false,
        });
        assert_eq!(data.series_names.len(), super::MAX_SERIES);
        assert_eq!(data.rows[0].values.len(), super::MAX_SERIES);
        let notice = data.notice.as_deref().unwrap_or_default();
        assert!(notice.contains("100"), "got {notice:?}");
    }

    #[test]
    fn too_many_bars_are_truncated_not_averaged() {
        // Adjacent categories carry no order, so merging them would invent
        // bands that were never in the result.
        let rows: Vec<Vec<String>> = (0..5_000)
            .map(|ix| Vec::from([format!("city-{ix}"), format!("{ix}")]))
            .collect();
        let data = ChartData::prepare(&QueryResult {
            columns: Vec::from([
                column("city", ColumnKind::Text),
                column("total", ColumnKind::Numeric),
            ]),
            rows,
            elapsed_ms: 0,
            truncated: false,
        });
        assert_eq!(data.rows.len(), super::MAX_BARS);
        assert_eq!(data.rows[0].band, "city-0");
        // Truncated, so every band is still a real one from the result.
        assert_eq!(data.rows[super::MAX_BARS - 1].band, "city-199");
        assert!(data.notice.is_some());
    }

    #[test]
    fn unparseable_rows_are_dropped() {
        let data = ChartData::prepare(&result(
            Vec::from([
                column("city", ColumnKind::Text),
                column("total", ColumnKind::Numeric),
            ]),
            Vec::from([
                Vec::from(["a", "1"]),
                Vec::from(["b", "NULL"]),
                Vec::from(["c", "3"]),
            ]),
        ));
        assert_eq!(data.rows.len(), 2);
        // `ix` stays the source row index, which the bar palette cycles on.
        assert_eq!(data.rows[1].ix, 2);
    }

    #[test]
    fn auto_picks_a_map_when_there_are_coordinates() {
        let data = ChartData::prepare(&result(
            Vec::from([
                column("name", ColumnKind::Text),
                column("lat", ColumnKind::Numeric),
                column("lng", ColumnKind::Numeric),
            ]),
            Vec::from([Vec::from(["Utrecht", "52.09", "5.11"])]),
        ));
        assert_eq!(data.resolve(ChartKind::Auto), ChartKind::Map);
        assert!(data.available_kinds().contains(&ChartKind::Map));
        // The other kinds still draw the coordinates as plain numbers.
        assert_eq!(data.resolve(ChartKind::Bar), ChartKind::Bar);
    }

    #[test]
    fn a_kind_the_result_cannot_draw_falls_back_to_auto() {
        let data = ChartData::prepare(&result(
            Vec::from([
                column("d", ColumnKind::Temporal),
                column("amount", ColumnKind::Numeric),
            ]),
            Vec::from([Vec::from(["2026-09-09", "1.5"])]),
        ));
        assert_eq!(data.resolve(ChartKind::Map), ChartKind::Area);
        assert_eq!(data.resolve(ChartKind::Pie), ChartKind::Area);
        assert_eq!(data.resolve(ChartKind::Line), ChartKind::Line);
    }

    #[test]
    fn pie_slices_fold_the_smallest_into_other() {
        let rows: Vec<Vec<String>> = (1..=20)
            .map(|ix| Vec::from([format!("c{ix}"), format!("{ix}")]))
            .chain([Vec::from(["neg".into(), "-5".into()])])
            .collect();
        let data = ChartData::prepare(&QueryResult {
            columns: Vec::from([
                column("city", ColumnKind::Text),
                column("total", ColumnKind::Numeric),
            ]),
            rows,
            elapsed_ms: 0,
            truncated: false,
        });
        assert_eq!(data.pie.len(), MAX_SLICES);
        // Largest first, and nothing is lost to the fold.
        assert_eq!(data.pie[0].name, "c20");
        assert!(data.pie.last().unwrap().slot.is_none());
        let total: f32 = data.pie.iter().map(|s| s.value).sum();
        assert_eq!(total, 210.);
        assert_eq!(data.pie_skipped, 1);
    }
}
