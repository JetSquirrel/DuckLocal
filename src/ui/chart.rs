//! Chart tab of the results panel: renders the current row set as a chart.
//! First column temporal → multi-series area chart (time series, one series
//! per numeric column); otherwise bar chart — first column as the band
//! label, first numeric column as the value.
//!
//! Deriving chart data walks every result row, so it happens once per result
//! set (`ChartData::prepare`) and is cached by the results panel. The panel
//! itself is rebuilt on every frame, and hands the cached rows to the chart as
//! `Rc`s so a frame costs refcount bumps rather than a copy of the row set.

use std::rc::Rc;

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::chart::{AreaChart, BarChart};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::query::{ColumnKind, QueryResult};

pub struct ChartDatum {
    band: SharedString,
    /// Pre-rendered bar label; avoids formatting on every frame.
    label: SharedString,
    values: Vec<f64>,
    ix: usize,
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

/// Everything the chart needs, derived from a result set once.
pub struct ChartData {
    label_name: String,
    series_names: Vec<String>,
    is_time_series: bool,
    rows: Vec<Rc<ChartDatum>>,
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
            notices.push(format!("仅显示前 {MAX_SERIES} / {series_total} 个数值列"));
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
                notices.push(format!(
                    "已降采样：{source_points} 点 → {} 点（每点为 {per_point} 个采样的均值）",
                    reduced.len()
                ));
            }
            reduced
        } else {
            let source_bars = raw.len();
            let mut bars = raw;
            if source_bars > MAX_BARS {
                bars.truncate(MAX_BARS);
                notices.push(format!("仅显示前 {MAX_BARS} / {source_bars} 行"));
            }
            bars
        };

        let rows = plotted
            .into_iter()
            .map(|(ix, band, values)| {
                Rc::new(ChartDatum {
                    band,
                    label: format_value(values[0]).into(),
                    values,
                    ix,
                })
            })
            .collect();

        Self {
            label_name: result.columns[0].name.clone(),
            series_names: value_ixes
                .iter()
                .map(|&ix| result.columns[ix].name.clone())
                .collect(),
            is_time_series,
            rows,
            detected: Vec::new(),
            notice: (!notices.is_empty()).then(|| notices.join(" · ")),
        }
    }

    #[cfg(test)]
    pub fn series_count_for_probe(&self) -> usize {
        self.series_names.len()
    }

    /// What `RenderOnce::render` hands to the chart each frame.
    #[cfg(test)]
    pub fn clone_rows_for_probe(&self) -> Vec<Rc<ChartDatum>> {
        self.rows.clone()
    }
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
fn parse_number(cell: &str) -> Option<f64> {
    if cell.contains(',') {
        cell.replace(',', "").parse().ok()
    } else {
        cell.parse().ok()
    }
}

#[derive(IntoElement)]
pub struct ChartPanel {
    data: Rc<ChartData>,
}

impl ChartPanel {
    pub fn new(data: Rc<ChartData>) -> Self {
        Self { data }
    }
}

impl RenderOnce for ChartPanel {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let data = &self.data;

        if data.series_names.is_empty() {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .p_4()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .text_center()
                        .child("当前结果没有数值列，无法绘制图表。"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .text_center()
                        .child(format!("检测到的列：{}", data.detected.join(" · "))),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .text_center()
                        .child("提示：文本列可用 cast(列名 AS DOUBLE) 或聚合函数（如 avg/sum/count）转换为数值。"),
                )
                .into_any_element();
        }

        if data.rows.is_empty() {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .p_4()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("没有可绘制的数据行。"),
                )
                .into_any_element();
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

        let chart = if data.is_time_series {
            // One area series per numeric column; ~10 x labels on dense data.
            // `.id` enables the built-in hover tooltip (crosshair + per-series rows).
            let tick_margin = (row_count / 10).max(1);
            let mut chart = AreaChart::new(rows)
                .x(|d: &Rc<ChartDatum>| d.band.clone())
                .id("results-chart");
            for (series_ix, series_name) in data.series_names.iter().enumerate() {
                let color = palette[series_ix % palette.len()];
                chart = chart
                    .y(move |d: &Rc<ChartDatum>| d.values[series_ix])
                    .stroke(color)
                    .fill(linear_gradient(
                        0.,
                        linear_color_stop(color.opacity(0.4), 1.),
                        linear_color_stop(color.opacity(0.05), 0.),
                    ))
                    .name(series_name.clone());
            }
            chart.tick_margin(tick_margin).into_any_element()
        } else {
            BarChart::new(rows)
                .band(|d: &Rc<ChartDatum>| d.band.clone())
                .value(|d: &Rc<ChartDatum>| d.values[0])
                .fill(move |d: &Rc<ChartDatum>, _, _, _| palette[d.ix % palette.len()])
                .label(|d: &Rc<ChartDatum>| d.label.clone())
                .id("results-chart")
                .name(data.series_names[0].clone())
                .into_any_element()
        };

        let label_name = data.label_name.clone();
        let notice = data.notice.clone();
        v_flex()
            .size_full()
            .p_4()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .items_baseline()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("{}（按 {label_name}）", data.series_names.join(", "))),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("绘制 {row_count} 点")),
                    ),
            )
            .when_some(notice, |this, notice| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(notice),
                )
            })
            .child(div().flex_1().min_h_0().child(chart))
            .into_any_element()
    }
}

fn format_value(value: f64) -> String {
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
    use super::{ChartData, parse_number};
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
            .map(|m| Vec::from([format!("2026-08-19 {:02}:{:02}", m / 60 % 24, m % 60), "10".into()]))
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
            data.notice.as_deref().unwrap_or_default().contains("降采样"),
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
}
