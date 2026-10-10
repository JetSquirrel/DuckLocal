//! A plot's data, derived from its query's result once.
//!
//! The spec says which columns are `x`, `y` and `series`; this module turns a
//! result set into the band/series matrix the charts draw: bands and series in
//! first-appearance order, duplicate `(x, series)` cells averaged, and the
//! same caps the results chart applies — points downsampled past a thousand
//! and a half, bars truncated past two hundred — because a chart that wedges
//! the renderer is worse than a chart that says it was merged.
//!
//! A `pie` is the same matrix with one series, never merged or truncated —
//! its slices must add up to the whole — then folded into its largest
//! slices. A `map` skips the matrix: it is the results chart's `GeoData`,
//! over the columns the spec names. A `card` is one cell: its column's value
//! in the first row, grouped by thousands when it is a plain number.
//!
//! A `bar` over a DATE column is a time axis, not a list of categories: the
//! days the query returned nothing for come back as empty bands, so a quiet
//! stretch reads as a gap rather than vanishing between two bars.
//!
//! Everything here is pure and tested as such; the view (`view.rs`) only
//! renders what this prepares.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::SharedString;

use super::model::Plot;
use crate::i18n::trf;
use crate::query::QueryResult;
use crate::query::ColumnKind;
use crate::ui::chart::{format_value, parse_number, pie_slices, PieSlice};
use crate::ui::geo::{is_lat_name, is_lng_name, GeoData, MapStyle, SizeScale};

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
    /// The band's position on the axis.
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
    /// A `pie`'s slices, largest first.
    pub pie: Vec<Arc<PieSlice>>,
    /// A `map`'s points.
    pub geo: Option<Arc<GeoData>>,
    /// A `card`'s value, ready to show.
    pub card: Option<SharedString>,
    /// Grid columns the plot spans, default applied.
    pub width: u8,
    /// Whether `x` is a date or time column: its bands are points in time,
    /// in order, whose labels may be thinned — a category's never are.
    pub time_axis: bool,
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
pub(crate) fn prepare<R: std::borrow::Borrow<QueryResult>>(
    plot: &Plot,
    result: Option<&Result<R, String>>,
) -> PreparedPlot {
    let base = |failure: Option<String>| PreparedPlot {
        name: plot.name.clone(),
        title: plot.title.clone().unwrap_or_else(|| plot.name.clone()),
        kind: plot.kind.clone(),
        query: plot.query.clone(),
        label_name: plot.x.clone(),
        series_names: Vec::new(),
        points: Vec::new(),
        pie: Vec::new(),
        geo: None,
        card: None,
        width: plot.width(),
        time_axis: false,
        notice: None,
        failure,
    };
    let Some(result) = result else {
        return base(Some(trf("dashboard.query_missing", &[&plot.query])));
    };
    let result: &QueryResult = match result {
        Ok(result) => result.borrow(),
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
    if plot.kind == "card" {
        let ix = match plot.value.as_deref() {
            Some(name) => match column(name) {
                Some(ix) => ix,
                None => return base(Some(missing_column(name, result))),
            },
            None if result.columns.is_empty() => {
                return base(Some(missing_column("value", result)))
            }
            None => 0,
        };
        let (value, notice) = match result.rows.first() {
            Some(row) => (
                group_thousands(row.get(ix).map(String::as_str).unwrap_or("")),
                (result.rows.len() > 1)
                    .then(|| trf("dashboard.card.first_row", &[&result.rows.len().to_string()])),
            ),
            None => ("—".to_string(), Some(crate::i18n::tr("dashboard.card.no_rows").to_string())),
        };
        return PreparedPlot {
            label_name: result.columns[ix].name.clone(),
            card: Some(SharedString::from(value)),
            notice,
            ..base(None)
        };
    }

    if plot.kind == "map" {
        // Named columns are taken at their word; a missing one is found by
        // name among the numeric columns, as the results chart finds them.
        let find = |named: &Option<String>, looks_like: fn(&str) -> bool, skip: Option<usize>| {
            match named {
                Some(name) => column(name).ok_or_else(|| missing_column(name, result)),
                None => result
                    .columns
                    .iter()
                    .enumerate()
                    .find(|(ix, c)| {
                        Some(*ix) != skip && c.kind == ColumnKind::Numeric && looks_like(&c.name)
                    })
                    .map(|(ix, _)| ix)
                    .ok_or_else(|| missing_coordinates(result)),
            }
        };
        let lat_ix = match find(&plot.lat, is_lat_name, None) {
            Ok(ix) => ix,
            Err(message) => return base(Some(message)),
        };
        let lng_ix = match find(&plot.lng, is_lng_name, Some(lat_ix)) {
            Ok(ix) => ix,
            Err(message) => return base(Some(message)),
        };
        let color_ix = match plot.color.as_deref().map(|name| column(name).ok_or(name)) {
            Some(Ok(ix)) => Some(ix),
            Some(Err(name)) => return base(Some(missing_column(name, result))),
            None => None,
        };
        let size_ix = match plot.size.as_deref().map(|name| column(name).ok_or(name)) {
            Some(Ok(ix)) => Some(ix),
            Some(Err(name)) => return base(Some(missing_column(name, result))),
            None => None,
        };
        let tooltip = match &plot.tooltip {
            Some(names) => {
                let mut ixs = Vec::with_capacity(names.len());
                for name in names {
                    match column(name) {
                        Some(ix) => ixs.push(ix),
                        None => return base(Some(missing_column(name, result))),
                    }
                }
                Some(ixs)
            }
            None => None,
        };
        let scale = plot
            .size_scale
            .as_deref()
            .and_then(SizeScale::parse)
            .unwrap_or_default();
        let style = MapStyle {
            color: color_ix,
            size: size_ix.map(|ix| (ix, scale)),
            tooltip,
        };
        let geo = GeoData::from_columns(result, lat_ix, lng_ix, &style);
        return PreparedPlot {
            label_name: format!(
                "{} / {}",
                result.columns[lat_ix].name, result.columns[lng_ix].name
            ),
            geo: Some(Arc::new(geo)),
            ..base(None)
        };
    }
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

    let time_axis = result.columns[x_ix].kind == ColumnKind::Temporal;
    if plot.kind == "bar" && time_axis {
        if let Some(filled) = fill_missing_days(&points, series_names.len()) {
            points = filled;
        }
    }

    let source_points = points.len();
    if plot.kind == "pie" {
        // Every band stays: the slices fold below, and a merged or dropped
        // band would make the shares wrong.
    } else if plot.kind == "bar" {
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

    // Labels render once, here, rather than on every frame. A band no series
    // has — a day the query skipped — says nothing, not zero.
    for point in points.iter_mut() {
        point.label = point
            .values
            .iter()
            .zip(&point.present)
            .find(|(_, present)| **present)
            .map(|(value, _)| SharedString::from(format_value(*value)))
            .unwrap_or_default();
    }

    let points: Vec<Arc<PlotPoint>> = points.into_iter().map(Arc::new).collect();
    let (pie, pie_skipped) = if plot.kind == "pie" {
        pie_slices(&points)
    } else {
        (Vec::new(), 0)
    };
    if pie_skipped > 0 {
        notices.push(trf("chart.pie.notice.skipped", &[&pie_skipped.to_string()]));
    }

    PreparedPlot {
        name: plot.name.clone(),
        title: plot.title.clone().unwrap_or_else(|| plot.name.clone()),
        kind: plot.kind.clone(),
        query: plot.query.clone(),
        label_name: result.columns[x_ix].name.clone(),
        series_names,
        points,
        pie,
        geo: None,
        card: None,
        width: plot.width(),
        time_axis,
        notice: (!notices.is_empty()).then(|| notices.join(" · ")),
        failure: None,
    }
}

/// `points` with an empty band for every day the query skipped, when every
/// band is a day (a DATE, or a midnight TIMESTAMP) and the days run one way,
/// in the order the query sorted them. `None` leaves the bands as they are: a
/// band that is not a day, days out of order, or a span past `MAX_BARS` days,
/// which the gaps would only push off the end of the chart.
fn fill_missing_days(points: &[PlotPoint], series: usize) -> Option<Vec<PlotPoint>> {
    let days = points
        .iter()
        .map(|p| day_of(&p.band))
        .collect::<Option<Vec<_>>>()?;
    let (first, last) = (*days.first()?, *days.last()?);
    let ascending = first <= last;
    let ordered = days
        .windows(2)
        .all(|w| if ascending { w[0] < w[1] } else { w[0] > w[1] });
    let span = (last - first).num_days().unsigned_abs() as usize + 1;
    if !ordered || span == days.len() || span > MAX_BARS {
        return None;
    }
    let step = chrono::Duration::days(if ascending { 1 } else { -1 });
    let mut filled = Vec::with_capacity(span);
    let mut given = points.iter().zip(&days).peekable();
    let mut day = first;
    for ix in 0..span {
        match given.next_if(|(_, d)| **d == day) {
            Some((point, _)) => filled.push(PlotPoint {
                band: point.band.clone(),
                label: point.label.clone(),
                values: point.values.clone(),
                present: point.present.clone(),
                ix,
            }),
            None => filled.push(PlotPoint {
                band: SharedString::from(day.format("%Y-%m-%d").to_string()),
                label: SharedString::new(""),
                values: vec![0.0; series],
                present: vec![false; series],
                ix,
            }),
        }
        day += step;
    }
    Some(filled)
}

/// The day a band names: `2026-10-02`, or `2026-10-02 00:00:00` — a
/// timestamp truncated to its day. Any other time of day is not a day.
fn day_of(band: &str) -> Option<chrono::NaiveDate> {
    let (date, rest) = band.split_at_checked(10)?;
    let rest = rest.trim_start_matches([' ', 'T']);
    if !(rest.is_empty() || rest.trim_end_matches(['0', ':', '.']).is_empty()) {
        return None;
    }
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
}

/// A cell as a card shows it: a plain decimal number with its integer part
/// grouped by thousands (`1859708` → `1,859,708`), anything else — a date, a
/// word, an exponent — exactly as the database wrote it.
fn group_thousands(cell: &str) -> String {
    let (sign, unsigned) = match cell.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", cell),
    };
    let (int, frac) = match unsigned.split_once('.') {
        Some((int, frac)) => (int, Some(frac)),
        None => (unsigned, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(int) || frac.is_some_and(|f| !digits(f)) {
        return cell.to_string();
    }
    let mut grouped = String::with_capacity(int.len() + int.len() / 3);
    for (ix, ch) in int.chars().enumerate() {
        if ix > 0 && (int.len() - ix) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    match frac {
        Some(frac) => format!("{sign}{grouped}.{frac}"),
        None => format!("{sign}{grouped}"),
    }
}

/// Why a `map` with no `lat` or `lng` has nothing to place.
fn missing_coordinates(result: &QueryResult) -> String {
    let available = result
        .columns
        .iter()
        .map(|column| column.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    trf("dashboard.map_no_coordinates", &[&available])
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
            lat: None,
            lng: None,
            color: None,
            size: None,
            size_scale: None,
            tooltip: None,
            value: None,
            width: None,
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
    fn a_bar_over_dates_keeps_the_quiet_days() {
        let data = |rows: Vec<Vec<&str>>| {
            result(
                vec![
                    column("day", ColumnKind::Temporal),
                    column("y", ColumnKind::Numeric),
                ],
                rows,
            )
        };
        let mut bar = plot("bar", Some("y"), None);
        bar.x = "day".into();
        let bands = |prepared: &PreparedPlot| {
            prepared
                .points
                .iter()
                .map(|p| (p.band.to_string(), p.present[0], p.label.to_string()))
                .collect::<Vec<_>>()
        };

        let prepared = prepare(
            &bar,
            Some(&Ok(data(vec![vec!["2026-10-02", "3"], vec!["2026-10-05", "4"]]))),
        );
        assert!(prepared.time_axis);
        assert_eq!(
            bands(&prepared),
            [
                ("2026-10-02".into(), true, "3".into()),
                ("2026-10-03".into(), false, String::new()),
                ("2026-10-04".into(), false, String::new()),
                ("2026-10-05".into(), true, "4".into()),
            ]
        );

        // Newest first stays newest first; midnight timestamps are days too.
        let prepared = prepare(
            &bar,
            Some(&Ok(data(vec![
                vec!["2026-10-05 00:00:00", "4"],
                vec!["2026-10-03 00:00:00", "3"],
            ]))),
        );
        let days: Vec<_> = bands(&prepared).into_iter().map(|b| b.0).collect();
        assert_eq!(days, ["2026-10-05 00:00:00", "2026-10-04", "2026-10-03 00:00:00"]);

        // A time of day, or days out of order, are left as the query gave them.
        for rows in [
            vec![vec!["2026-10-02 12:30:00", "1"], vec!["2026-10-05 08:00:00", "2"]],
            vec![vec!["2026-10-05", "1"], vec!["2026-10-02", "2"], vec!["2026-10-09", "3"]],
        ] {
            let prepared = prepare(&bar, Some(&Ok(data(rows))));
            assert!(prepared.points.iter().all(|p| p.present[0]));
        }

        // A text x is a category, however much it looks like a date.
        let text = result(
            vec![column("day", ColumnKind::Text), column("y", ColumnKind::Numeric)],
            vec![vec!["2026-10-02", "1"], vec!["2026-10-05", "2"]],
        );
        let prepared = prepare(&bar, Some(&Ok(text)));
        assert!(!prepared.time_axis);
        assert_eq!(prepared.points.len(), 2);
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
            Some(&Err::<QueryResult, _>("boom".to_string())),
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

    fn map_plot(lat: Option<&str>, lng: Option<&str>, color: Option<&str>) -> Plot {
        Plot {
            kind: "map".into(),
            x: String::new(),
            y: None,
            lat: lat.map(str::to_string),
            lng: lng.map(str::to_string),
            color: color.map(str::to_string),
            ..plot("map", None, None)
        }
    }

    fn stations() -> QueryResult {
        QueryResult {
            columns: vec![
                column("name", ColumnKind::Text),
                column("type", ColumnKind::Text),
                column("geo_lat", ColumnKind::Numeric),
                column("geo_lng", ColumnKind::Numeric),
            ],
            rows: [
                ["Utrecht", "mega", "52.09", "5.11"],
                ["Zwolle", "ic", "52.50", "6.09"],
                ["Aalten", "stop", "NULL", "6.57"],
            ]
            .iter()
            .map(|r| r.iter().map(|c| c.to_string()).collect())
            .collect(),
            elapsed_ms: 0,
            truncated: false,
        }
    }

    #[test]
    fn a_map_places_the_rows_its_columns_locate() {
        let result = Ok(stations());
        let named = prepare(
            &map_plot(Some("geo_lat"), Some("geo_lng"), Some("type")),
            Some(&result),
        );
        assert_eq!(named.failure, None);
        let geo = named.geo.expect("a map carries its points");
        assert_eq!(geo.point_count(), 2);
        assert_eq!(geo.dropped, 1, "a NULL latitude places nothing");
        assert_eq!(geo.category_name.as_deref(), Some("type"));

        // Left out, the coordinates are found by name.
        let guessed = prepare(&map_plot(None, None, None), Some(&result));
        assert_eq!(guessed.geo.map(|g| g.point_count()), Some(2));
        assert_eq!(guessed.label_name, "geo_lat / geo_lng");
    }

    #[test]
    fn a_map_without_coordinates_says_so() {
        let result = Ok(QueryResult {
            columns: vec![column("name", ColumnKind::Text)],
            rows: vec![vec!["x".into()]],
            elapsed_ms: 0,
            truncated: false,
        });
        let prepared = prepare(&map_plot(None, None, None), Some(&result));
        assert!(prepared.geo.is_none());
        assert!(prepared.failure.is_some());

        let prepared = prepare(&map_plot(Some("nope"), Some("geo_lng"), None), Some(&Ok(stations())));
        assert!(prepared.failure.unwrap().contains("nope"));
    }

    #[test]
    fn a_pie_keeps_every_band_and_folds_the_small_ones() {
        // More bands than a bar chart keeps: a pie must add up to the whole.
        let rows: Vec<Vec<String>> = (1..=300)
            .map(|ix| vec![format!("c{ix}"), ix.to_string()])
            .chain([vec!["neg".to_string(), "-1".to_string()]])
            .collect();
        let result = Ok(QueryResult {
            columns: vec![column("x", ColumnKind::Text), column("y", ColumnKind::Numeric)],
            rows,
            elapsed_ms: 0,
            truncated: false,
        });
        let pie = prepare(&plot("pie", Some("y"), None), Some(&result));
        assert_eq!(pie.points.len(), 301, "nothing truncated");
        assert_eq!(pie.pie.len(), 8, "folded to the largest slices");
        let total: f64 = pie.pie.iter().map(|s| s.value() as f64).sum();
        assert_eq!(total, (1..=300).sum::<i64>() as f64);
        assert!(pie.notice.unwrap().contains('1'), "the negative band is reported");
    }

    #[test]
    fn a_card_shows_its_first_row_grouped() {
        let data = || {
            Ok(result(
                vec![
                    column("label", ColumnKind::Text),
                    column("total", ColumnKind::Numeric),
                ],
                vec![vec!["all", "1859708"], vec!["more", "2"]],
            ))
        };
        let card = |value: Option<&str>| Plot {
            value: value.map(str::to_string),
            ..plot("card", None, None)
        };
        let shown = prepare(&card(Some("total")), Some(&data()));
        assert_eq!(shown.card.as_ref().map(|c| c.as_ref()), Some("1,859,708"));
        assert_eq!(shown.width, 3, "a card is a quarter row by default");
        assert!(shown.notice.is_some(), "a second row is said, not shown");
        // Left out, the value is the first column.
        let first = prepare(&card(None), Some(&data()));
        assert_eq!(first.card.as_ref().map(|c| c.as_ref()), Some("all"));
        let missing = prepare(&card(Some("nope")), Some(&data()));
        assert!(missing.failure.unwrap().contains("nope"));
        assert_eq!(prepare(&plot("line", Some("y"), None), Some(&data())).width, 12);
    }

    #[test]
    fn only_plain_numbers_are_grouped() {
        assert_eq!(group_thousands("1234567.891"), "1,234,567.891");
        assert_eq!(group_thousands("-1000"), "-1,000");
        assert_eq!(group_thousands("999"), "999");
        assert_eq!(group_thousands("1e10"), "1e10");
        assert_eq!(group_thousands("2026-09-30"), "2026-09-30");
        assert_eq!(group_thousands(""), "");
    }
}
