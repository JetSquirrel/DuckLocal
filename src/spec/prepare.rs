//! A plot's data, derived from its query's result once.
//!
//! The spec says which columns are `x`, `y` and `series`; this module turns a
//! result set into the band/series matrix the charts draw: bands and series in
//! first-appearance order, duplicate `(x, series)` cells averaged, and the
//! same caps the results chart applies — points downsampled past a thousand
//! and a half, bars truncated past two hundred — because a chart that wedges
//! the renderer is worse than a chart that says it was merged.
//!
//! Everything here is pure and tested as such; the view (`view.rs`) only
//! renders what this prepares.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::SharedString;

use super::model::Plot;
use crate::i18n::trf;
use crate::query::QueryResult;
use crate::ui::chart::{format_value, parse_number};

/// A chart is at most a couple of thousand pixels wide; past this, points are
/// bucket-averaged, as in the results chart.
const MAX_POINTS: usize = 1_500;
/// Adjacent bands cannot be meaningfully merged, so surplus bars are dropped.
const MAX_BARS: usize = 200;
/// The palette holds five colors; past a dozen series a chart is unreadable.
const MAX_SERIES: usize = 12;

#[derive(Debug)]
pub struct PlotPoint {
    pub band: SharedString,
    /// Pre-rendered value label; avoids formatting on every frame.
    pub label: SharedString,
    /// One value per series, aligned with `series_names`.
    pub values: Vec<f64>,
    /// Which series actually had a value at this band; the rest read as 0 but
    /// were never in the result, and `present` is how a renderer tells.
    pub present: Vec<bool>,
    /// Source order, for the bar palette.
    pub ix: usize,
}

/// Everything one plot needs to render, or why it cannot.
#[derive(Debug)]
pub struct PreparedPlot {
    /// The plot block's name, unique after validation; the view keys on it.
    pub name: String,
    /// `title` when written, the plot's name otherwise.
    pub title: String,
    pub kind: String,
    /// The query block this plot draws; the view keys tables by it.
    pub query: String,
    pub label_name: String,
    pub series_names: Vec<String>,
    pub points: Vec<Arc<PlotPoint>>,
    /// What was dropped or merged to keep the plot drawable.
    pub notice: Option<String>,
    /// Why there is nothing to draw: the query failed, or a column the plot
    /// names is not in the result. (The `check` command catches the second
    /// ahead of time; the database can still disagree at render time.)
    pub failure: Option<String>,
}

/// Prepare one plot from its query's result. `table` plots need no
/// preparation — the view renders the result grid directly — and a plot whose
/// query failed carries the failure instead of data.
pub(crate) fn prepare(plot: &Plot, result: Option<&Result<QueryResult, String>>) -> PreparedPlot {
    let base = |failure: Option<String>| PreparedPlot {
        name: plot.name.clone(),
        title: plot.title.clone().unwrap_or_else(|| plot.name.clone()),
        kind: plot.kind.clone(),
        query: plot.query.clone(),
        label_name: plot.x.clone(),
        series_names: Vec::new(),
        points: Vec::new(),
        notice: None,
        failure,
    };
    let Some(result) = result else {
        return base(Some(trf("dashboard.query_missing", &[&plot.query])));
    };
    let result = match result {
        Ok(result) => result,
        Err(error) => return base(Some(error.clone())),
    };
    if plot.kind == "table" {
        return base(None);
    }

    let column = |name: &str| {
        result
            .columns
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(name))
    };
    let Some(x_ix) = column(&plot.x) else {
        return base(Some(missing_column(&plot.x, result)));
    };
    let Some(y_name) = plot.y.as_deref() else {
        // A non-table plot without y cannot have validated; say so anyway.
        return base(Some(crate::i18n::tr("dashboard.invalid_plot").to_string()));
    };
    let Some(y_ix) = column(y_name) else {
        return base(Some(missing_column(y_name, result)));
    };
    let series_ix = plot.series.as_deref().map(|name| match column(name) {
        Some(ix) => Ok(ix),
        None => Err(missing_column(name, result)),
    });
    let series_ix = match series_ix {
        Some(Err(message)) => return base(Some(message)),
        Some(Ok(ix)) => Some(ix),
        None => None,
    };

    // First-appearance order for bands and series; duplicates average.
    let mut bands: Vec<String> = Vec::new();
    let mut band_ix: HashMap<String, usize> = HashMap::new();
    let mut series: Vec<String> = Vec::new();
    let mut series_ix_of: HashMap<String, usize> = HashMap::new();
    let mut series_total = 0usize;
    let mut series_capped = false;
    let mut sums: Vec<Vec<(f64, u32)>> = Vec::new();
    for row in &result.rows {
        let (Some(band), Some(value)) = (row.get(x_ix), row.get(y_ix)) else {
            continue;
        };
        let Some(value) = parse_number(value) else {
            continue;
        };
        let series_name = series_ix
            .and_then(|ix| row.get(ix))
            .cloned()
            .unwrap_or_else(|| result.columns[y_ix].name.clone());
        let s = match series_ix_of.get(&series_name) {
            Some(&s) => s,
            None => {
                series_total += 1;
                if series.len() >= MAX_SERIES {
                    series_capped = true;
                    continue;
                }
                let s = series.len();
                series_ix_of.insert(series_name.clone(), s);
                series.push(series_name);
                for column in sums.iter_mut() {
                    column.push((0.0, 0));
                }
                s
            }
        };
        let b = *band_ix.entry(band.clone()).or_insert_with(|| {
            let b = bands.len();
            bands.push(band.clone());
            sums.push(vec![(0.0, 0); series.len()]);
            b
        });
        let cell = &mut sums[b][s];
        cell.0 += value;
        cell.1 += 1;
    }

    let default_series = result.columns[y_ix].name.clone();
    let series_names = if series_ix.is_some() {
        series
    } else {
        // No series column: one series named after y, even if no row parsed.
        vec![default_series]
    };

    let mut notices: Vec<String> = Vec::new();
    if series_capped {
        notices.push(trf(
            "chart.notice.series_capped",
            &[&MAX_SERIES.to_string(), &series_total.to_string()],
        ));
    }
    let mut missing_cells = 0usize;
    let mut points: Vec<PlotPoint> = bands
        .iter()
        .enumerate()
        .map(|(ix, band)| {
            let mut values = Vec::with_capacity(series_names.len());
            let mut present = Vec::with_capacity(series_names.len());
            for s in 0..series_names.len() {
                match sums[ix].get(s) {
                    Some(&(sum, count)) if count > 0 => {
                        values.push(sum / count as f64);
                        present.push(true);
                    }
                    _ => {
                        values.push(0.0);
                        present.push(false);
                        missing_cells += 1;
                    }
                }
            }
            PlotPoint {
                band: SharedString::from(band.clone()),
                label: SharedString::new(""),
                values,
                present,
                ix,
            }
        })
        .collect();
    if missing_cells > 0 && series_names.len() > 1 {
        notices.push(trf(
            "dashboard.notice.zero_filled",
            &[&missing_cells.to_string()],
        ));
    }

    let source_points = points.len();
    if plot.kind == "bar" {
        if source_points > MAX_BARS {
            points.truncate(MAX_BARS);
            notices.push(trf(
                "chart.notice.bars_capped",
                &[&MAX_BARS.to_string(), &source_points.to_string()],
            ));
        }
    } else if source_points > MAX_POINTS {
        // All series share the x axis, so buckets are shared too; a bucket
        // keeps its first point's band and index.
        let bucket_size = source_points.div_ceil(MAX_POINTS);
        let mut reduced: Vec<PlotPoint> = Vec::with_capacity(MAX_POINTS);
        for chunk in points.chunks(bucket_size) {
            let first = &chunk[0];
            let mut sums = vec![(0.0f64, 0u32); series_names.len()];
            for point in chunk {
                for (s, slot) in sums.iter_mut().enumerate() {
                    if point.present[s] {
                        slot.0 += point.values[s];
                        slot.1 += 1;
                    }
                }
            }
            let mut values = Vec::with_capacity(sums.len());
            let mut present = Vec::with_capacity(sums.len());
            for (sum, count) in sums {
                values.push(if count > 0 { sum / count as f64 } else { 0.0 });
                present.push(count > 0);
            }
            reduced.push(PlotPoint {
                band: first.band.clone(),
                label: SharedString::new(""),
                values,
                present,
                ix: first.ix,
            });
        }
        let per_point = source_points.div_ceil(reduced.len().max(1));
        notices.push(trf(
            "chart.notice.downsampled",
            &[
                &source_points.to_string(),
                &reduced.len().to_string(),
                &per_point.to_string(),
            ],
        ));
        points = reduced;
    }

    // Labels render once, here, rather than on every frame.
    for point in points.iter_mut() {
        let first = point
            .values
            .iter()
            .zip(&point.present)
            .find(|(_, present)| **present)
            .map(|(value, _)| *value)
            .unwrap_or(0.0);
        point.label = SharedString::from(format_value(first));
    }

    PreparedPlot {
        name: plot.name.clone(),
        title: plot.title.clone().unwrap_or_else(|| plot.name.clone()),
        kind: plot.kind.clone(),
        query: plot.query.clone(),
        label_name: result.columns[x_ix].name.clone(),
        series_names,
        points: points.into_iter().map(Arc::new).collect(),
        notice: (!notices.is_empty()).then(|| notices.join(" · ")),
        failure: None,
    }
}

fn missing_column(name: &str, result: &QueryResult) -> String {
    let available = result
        .columns
        .iter()
        .map(|column| column.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    trf("dashboard.missing_column_named", &[name, &available])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::{ColumnKind, ColumnMeta};

    fn plot(kind: &str, y: Option<&str>, series: Option<&str>) -> Plot {
        Plot {
            name: "p".into(),
            kind: kind.into(),
            query: "q".into(),
            x: "x".into(),
            y: y.map(str::to_string),
            series: series.map(str::to_string),
            title: None,
            line: 1,
            col: 1,
            span: (0, 4),
            name_span: (6, 9),
        }
    }

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
    fn a_plain_series_plots_in_row_order() {
        let data = result(
            vec![
                column("x", ColumnKind::Temporal),
                column("y", ColumnKind::Numeric),
            ],
            vec![vec!["2026-09-01", "1.5"], vec!["2026-09-02", "2.5"]],
        );
        let prepared = prepare(&plot("line", Some("y"), None), Some(&Ok(data)));
        assert!(prepared.failure.is_none());
        assert_eq!(prepared.series_names, ["y"]);
        assert_eq!(prepared.points.len(), 2);
        assert_eq!(prepared.points[0].values, [1.5]);
        assert_eq!(prepared.points[1].band.as_ref(), "2026-09-02");
        assert_eq!(prepared.label_name, "x");
    }

    #[test]
    fn a_series_column_pivots_rows_into_series() {
        let data = result(
            vec![
                column("x", ColumnKind::Temporal),
                column("svc", ColumnKind::Text),
                column("y", ColumnKind::Numeric),
            ],
            vec![
                vec!["d1", "a", "1"],
                vec!["d1", "b", "10"],
                vec!["d2", "a", "2"],
                vec!["d3", "b", "30"],
            ],
        );
        let prepared = prepare(&plot("line", Some("y"), Some("svc")), Some(&Ok(data)));
        assert_eq!(prepared.series_names, ["a", "b"]);
        assert_eq!(prepared.points.len(), 3);
        assert_eq!(prepared.points[0].values, [1.0, 10.0]);
        assert!(prepared.points[0].present.iter().all(|p| *p));
        // d2 has no b, d3 has no a: zero-filled, but `present` says so.
        assert_eq!(prepared.points[1].values[0], 2.0);
        assert!(!prepared.points[1].present[1]);
        assert!(!prepared.points[2].present[0]);
        assert!(prepared.notice.is_some(), "zero-filled cells are said");
    }

    #[test]
    fn duplicate_cells_average() {
        let data = result(
            vec![
                column("x", ColumnKind::Text),
                column("y", ColumnKind::Numeric),
            ],
            vec![vec!["a", "1"], vec!["a", "3"]],
        );
        let prepared = prepare(&plot("bar", Some("y"), None), Some(&Ok(data)));
        assert_eq!(prepared.points.len(), 1);
        assert_eq!(prepared.points[0].values, [2.0]);
    }

    #[test]
    fn a_missing_column_is_a_failure_that_names_what_exists() {
        let data = result(
            vec![
                column("x", ColumnKind::Text),
                column("y", ColumnKind::Numeric),
            ],
            vec![vec!["a", "1"]],
        );
        let prepared = prepare(&plot("bar", Some("nope"), None), Some(&Ok(data)));
        let failure = prepared.failure.unwrap();
        assert!(failure.contains("nope"), "{failure}");
        assert!(failure.contains("y"), "{failure}");
        assert!(prepared.points.is_empty());
    }

    #[test]
    fn a_failed_query_carries_its_error() {
        let prepared = prepare(
            &plot("bar", Some("y"), None),
            Some(&Err("boom".to_string())),
        );
        assert_eq!(prepared.failure.as_deref(), Some("boom"));
    }

    #[test]
    fn bars_truncate_where_lines_downsample() {
        let owned: Vec<Vec<String>> = (0..5_000)
            .map(|ix| vec![format!("band-{ix:05}"), "1".into()])
            .collect();
        let rows: Vec<Vec<&str>> = owned
            .iter()
            .map(|row| row.iter().map(String::as_str).collect())
            .collect();
        let bar = prepare(
            &plot("bar", Some("y"), None),
            Some(&Ok(result(
                vec![
                    column("x", ColumnKind::Text),
                    column("y", ColumnKind::Numeric),
                ],
                rows.clone(),
            ))),
        );
        assert_eq!(bar.points.len(), MAX_BARS);
        // Truncated, never merged: every surviving band is a real one.
        assert_eq!(bar.points[MAX_BARS - 1].band.as_ref(), "band-00199");
        assert!(bar.notice.is_some());

        let line = prepare(
            &plot("line", Some("y"), None),
            Some(&Ok(result(
                vec![
                    column("x", ColumnKind::Temporal),
                    column("y", ColumnKind::Numeric),
                ],
                rows,
            ))),
        );
        assert!(line.points.len() <= MAX_POINTS);
        assert!(line.points.iter().all(|p| p.values[0] == 1.0));
        assert!(line.notice.is_some());
    }
}
