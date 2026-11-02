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
//! in the first row, grouped by thousands when it is a plain number, with a
//! note under it. A `text` panel draws no query: its words are the spec's.
//!
//! A `bar` over a DATE column is a time axis, not a list of categories: the
//! days the query returned nothing for come back as empty bands, so a quiet
//! stretch reads as a gap rather than vanishing between two bars.
//!
//! Everything here is pure and tested as such; the view (`view.rs`) only
//! renders what this prepares.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui_kit::SharedString;

use super::format::{full_text, short_text, NumberFormat};
use super::model::{Note, Plot};
use crate::i18n::{tr, trf};
use crate::query::ColumnKind;
use crate::query::QueryResult;
use crate::ui::chart::{format_value, parse_number, pie_slices, PieSlice};
use crate::ui::geo::{is_lat_name, is_lng_name, GeoData, MapStyle, SizeScale};
// The results chart's caps, so a dashboard plot and the chart tab drop and
// merge at the same points.
use crate::ui::chart::{MAX_BARS, MAX_POINTS, MAX_SERIES};

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

/// Whether a card's change is the one wanted. A status, so it never shows by
/// colour alone: the arrow says the direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mood {
    Good,
    Bad,
    /// No change: neither.
    Flat,
}

/// A card's change, ready to show: `▲ 12.3%`, and whether it is good news.
#[derive(Clone, Debug, PartialEq)]
pub struct CardDelta {
    pub text: SharedString,
    pub mood: Mood,
}

impl CardDelta {
    /// `change` as `plot` reads it: in `delta_format`, else the card's own
    /// `format`; good when it rises, or when it falls and `down_is_good`.
    fn of(change: f64, plot: &Plot) -> Self {
        let format = plot
            .delta_format
            .as_deref()
            .or(plot.format.as_deref())
            .and_then(NumberFormat::parse);
        let (arrow, mood) = if change == 0. {
            ("–", Mood::Flat)
        } else if (change > 0.) != plot.down_is_good {
            (if change > 0. { "▲" } else { "▼" }, Mood::Good)
        } else {
            (if change > 0. { "▲" } else { "▼" }, Mood::Bad)
        };
        // The arrow carries the sign, so the number is its size.
        let text = format!("{arrow} {}", full_text(format, change.abs()));
        Self {
            text: SharedString::from(text),
            mood,
        }
    }
}

/// Everything one plot needs to render, or why it cannot.
#[derive(Debug)]
pub struct PreparedPlot {
    /// The plot block's name, unique after validation; the view keys on it.
    pub name: String,
    /// `title` when written, the plot's name otherwise.
    pub title: String,
    /// What the plot shows, behind an info mark beside its title.
    pub description: Option<SharedString>,
    pub kind: String,
    /// The query block this plot draws; the view keys tables by it.
    pub query: String,
    pub label_name: String,
    pub series_names: Vec<String>,
    /// Each series' color, by place in the palette, when a re-run has moved it
    /// from its position; see [`Self::slot`]. Empty on a first run.
    pub series_slots: Vec<usize>,
    /// Every series this plot has drawn and its color, gone ones included, so
    /// one coming back has the color it left with.
    pub known_slots: HashMap<String, usize>,
    pub points: Vec<Arc<PlotPoint>>,
    /// A `pie`'s slices, largest first.
    pub pie: Vec<Arc<PieSlice>>,
    /// A `map`'s points.
    pub geo: Option<Arc<GeoData>>,
    /// A `card`'s value, ready to show.
    pub card: Option<SharedString>,
    /// A `card`'s change, when it names a `delta` and its row holds a number.
    pub delta: Option<CardDelta>,
    /// The line under a `card`'s value: the spec's words, or its column's
    /// value from the card's row.
    pub note: Option<SharedString>,
    /// A `card`'s tone, as the spec names it.
    pub tone: Option<String>,
    /// A `text` panel's words under its title.
    pub text: Option<SharedString>,
    /// Grid columns the plot spans, default applied.
    pub width: u8,
    /// Whether `x` is a date or time column: its bands are points in time,
    /// in order, whose labels may be thinned — a category's never are.
    pub time_axis: bool,
    /// A scatter over a numeric `x`, or a line or scatter over times: each
    /// point's x as a number, in the order of `points` (ascending), for a
    /// linear x axis. `None` for every other plot, whose x is bands.
    pub x_values: Option<Arc<Vec<f64>>>,
    /// Whether `x_values` are times, whose labels are the points' own dates
    /// rather than round numbers.
    pub x_is_time: bool,
    /// How the plot's numbers read: `format`, parsed.
    pub format: Option<NumberFormat>,
    /// What was dropped or merged to keep the plot drawable.
    pub notice: Option<String>,
    /// Why there is nothing to draw: the query failed, or a column the plot
    /// names is not in the result. (The `check` command catches the second
    /// ahead of time; the database can still disagree at render time.)
    pub failure: Option<String>,
}

/// A histogram of column `x_ix`: a bar per range, its height the rows whose
/// value falls in it. Rows without a number are left out, and said.
fn histogram(result: &QueryResult, x_ix: usize, base: PreparedPlot) -> PreparedPlot {
    let column = &result.columns[x_ix];
    if column.kind != ColumnKind::Numeric {
        return PreparedPlot {
            failure: Some(trf(
                "chart.histogram.not_numeric",
                &[&column.name, &column.duck_type],
            )),
            ..base
        };
    }
    let values: Vec<f64> = result
        .rows
        .iter()
        .filter_map(|row| parse_number(row.get(x_ix)?))
        .filter(|v| v.is_finite())
        .collect();
    let mut notices = Vec::new();
    if result.truncated {
        notices.push(trf(
            "chart.notice.truncated",
            &[&result.rows.len().to_string()],
        ));
    }
    let skipped = result.rows.len() - values.len();
    if skipped > 0 {
        notices.push(trf("chart.histogram.skipped", &[&skipped.to_string()]));
    }
    let points = super::histogram::bin(&values)
        .map(|(bins, counts)| super::histogram::points(bins, &counts, base.format))
        .unwrap_or_default();
    PreparedPlot {
        label_name: column.name.clone(),
        series_names: vec![crate::i18n::tr("chart.histogram.rows").to_string()],
        points,
        notice: crate::i18n::join_all(&notices),
        ..base
    }
}

/// The most dots a scatter draws. Past it the points are thinned evenly
/// along x, which keeps the cloud's shape, and the plot says so.
pub(crate) const MAX_SCATTER_POINTS: usize = 4_000;

/// A scatter over a numeric x: one point per row with a number for both x
/// and y, sorted by x, each present in its own series only.
fn scatter_points(
    result: &QueryResult,
    x_ix: usize,
    y_ix: usize,
    series_ix: Option<usize>,
    base: PreparedPlot,
) -> PreparedPlot {
    let mut series: Vec<String> = Vec::new();
    let mut left_out: HashSet<String> = HashSet::new();
    let mut nonfinite = 0usize;
    let mut rows: Vec<(f64, f64, usize)> = Vec::new();
    for row in &result.rows {
        let (Some(x), Some(y)) = (
            row.get(x_ix).and_then(|cell| parse_number(cell)),
            row.get(y_ix).and_then(|cell| parse_number(cell)),
        ) else {
            continue;
        };
        if !x.is_finite() || !y.is_finite() {
            nonfinite += 1;
            continue;
        }
        let name = series_ix
            .and_then(|ix| row.get(ix))
            .cloned()
            .unwrap_or_else(|| result.columns[y_ix].name.clone());
        let s = match series.iter().position(|known| *known == name) {
            Some(s) => s,
            None => {
                if series.len() >= MAX_SERIES {
                    left_out.insert(name);
                    continue;
                }
                series.push(name);
                series.len() - 1
            }
        };
        rows.push((x, y, s));
    }
    if series.is_empty() {
        series.push(result.columns[y_ix].name.clone());
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut notices = Vec::new();
    if result.truncated {
        notices.push(truncated_notice(result));
    }
    if !left_out.is_empty() {
        notices.push(trf(
            "chart.notice.series_left_out",
            &[
                &MAX_SERIES.to_string(),
                &(series.len() + left_out.len()).to_string(),
            ],
        ));
    }
    if nonfinite > 0 {
        notices.push(trf("chart.notice.nonfinite", &[&nonfinite.to_string()]));
    }
    let drawn = rows.len();
    if drawn > MAX_SCATTER_POINTS {
        let stride = drawn.div_ceil(MAX_SCATTER_POINTS);
        rows = rows.into_iter().step_by(stride).collect();
        notices.push(trf(
            "dashboard.notice.scatter_thinned",
            &[&rows.len().to_string(), &drawn.to_string()],
        ));
    }

    let mut xs = Vec::with_capacity(rows.len());
    let points = rows
        .into_iter()
        .enumerate()
        .map(|(ix, (x, y, s))| {
            xs.push(x);
            let mut values = vec![0.0; series.len()];
            let mut present = vec![false; series.len()];
            values[s] = y;
            present[s] = true;
            Arc::new(PlotPoint {
                band: SharedString::from(format_value(x)),
                label: SharedString::from(short_text(base.format, y)),
                values,
                present,
                ix,
            })
        })
        .collect();
    PreparedPlot {
        label_name: result.columns[x_ix].name.clone(),
        series_names: series,
        points,
        x_values: Some(Arc::new(xs)),
        notice: (!notices.is_empty()).then(|| notices.join(" ")),
        ..base
    }
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
        description: plot.description.clone().map(SharedString::from),
        kind: plot.kind.clone(),
        query: plot.query.clone(),
        label_name: plot.x.clone(),
        series_names: Vec::new(),
        series_slots: Vec::new(),
        known_slots: HashMap::new(),
        points: Vec::new(),
        pie: Vec::new(),
        geo: None,
        card: None,
        delta: None,
        note: None,
        tone: plot.tone.clone(),
        text: None,
        width: plot.width(),
        time_axis: false,
        x_values: None,
        x_is_time: false,
        format: plot.format.as_deref().and_then(NumberFormat::parse),
        notice: None,
        failure,
    };
    if plot.kind == "text" {
        return PreparedPlot {
            // A text panel without a title is its words alone, not its name.
            title: plot.title.clone().unwrap_or_default(),
            text: plot
                .text
                .as_deref()
                .map(|text| SharedString::from(dedent(text))),
            ..base(None)
        };
    }
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
        let note = match &plot.note {
            Some(Note::Text(words)) => Some(words.clone()),
            Some(Note::Column(name)) => match column(name) {
                Some(ix) => result
                    .rows
                    .first()
                    .and_then(|row| row.get(ix))
                    .map(|cell| group_thousands(cell)),
                None => return base(Some(missing_column(name, result))),
            },
            None => None,
        };
        // A year is a date, not a quantity: 2024, not 2,024.
        let format = |cell: &str| {
            let formatted = plot
                .format
                .as_deref()
                .and_then(NumberFormat::parse)
                .zip(parse_number(cell));
            if let Some((format, number)) = formatted {
                format.full(number)
            } else if is_year_column(&result.columns[ix].name) {
                cell.to_string()
            } else {
                group_thousands(cell)
            }
        };
        let (value, notice) = match result.rows.first() {
            Some(row) => (
                format(row.get(ix).map(String::as_str).unwrap_or("")),
                (result.rows.len() > 1).then(|| {
                    trf(
                        "dashboard.card.first_row",
                        &[&result.rows.len().to_string()],
                    )
                }),
            ),
            None => (
                "—".to_string(),
                Some(crate::i18n::tr("dashboard.card.no_rows").to_string()),
            ),
        };
        let delta = match plot.delta.as_deref() {
            Some(name) => match column(name) {
                Some(ix) => result
                    .rows
                    .first()
                    .and_then(|row| row.get(ix))
                    .and_then(|cell| parse_number(cell))
                    .map(|change| CardDelta::of(change, plot)),
                None => return base(Some(missing_column(name, result))),
            },
            None => None,
        };
        return PreparedPlot {
            label_name: result.columns[ix].name.clone(),
            card: Some(SharedString::from(value)),
            note: note.map(SharedString::from),
            delta,
            notice,
            ..base(None)
        };
    }

    if plot.kind == "map" {
        // Named columns are taken at their word; a missing one is found by
        // name among the numeric columns, as the results chart finds them.
        let find =
            |named: &Option<String>, looks_like: fn(&str) -> bool, skip: Option<usize>| match named
            {
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
            notice: result.truncated.then(|| truncated_notice(result)),
            ..base(None)
        };
    }
    let Some(x_ix) = column(&plot.x) else {
        return base(Some(missing_column(&plot.x, result)));
    };
    if plot.kind == "histogram" {
        return histogram(result, x_ix, base(None));
    }
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

    // A scatter over numbers is points, not bands: a dot per row at its own
    // (x, y) on a linear axis. Averaging the rows that share an x, as a
    // line's bands do, would draw two birds of one flipper length as one dot
    // that neither of them is.
    if plot.kind == "scatter" && result.columns[x_ix].kind == ColumnKind::Numeric {
        return scatter_points(result, x_ix, y_ix, series_ix, base(None));
    }

    // First-appearance order for bands and series; duplicates average.
    let mut bands: Vec<String> = Vec::new();
    let mut band_ix: HashMap<String, usize> = HashMap::new();
    let mut series: Vec<String> = Vec::new();
    let mut series_ix_of: HashMap<String, usize> = HashMap::new();
    let mut left_out: HashSet<String> = HashSet::new();
    let mut sums: Vec<Vec<(f64, u32)>> = Vec::new();
    // A line stops at a reading that is missing, where a bar has nothing to
    // show: which cells those are, by band and series.
    let breaks_at_null = plot.kind == "line";
    let mut nulls: HashSet<(usize, usize)> = HashSet::new();
    let mut nonfinite = 0usize;
    for row in &result.rows {
        let (Some(band), Some(cell)) = (row.get(x_ix), row.get(y_ix)) else {
            continue;
        };
        let value = parse_number(cell);
        if value.is_some_and(|value| !value.is_finite()) {
            nonfinite += 1;
            continue;
        }
        let is_null = breaks_at_null && cell == "NULL";
        if value.is_none() && !is_null {
            continue;
        }
        let series_name = series_ix
            .and_then(|ix| row.get(ix))
            .cloned()
            .unwrap_or_else(|| result.columns[y_ix].name.clone());
        let s = match series_ix_of.get(&series_name) {
            Some(&s) => s,
            None => {
                if series.len() >= MAX_SERIES {
                    left_out.insert(series_name);
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
        match value {
            Some(value) => {
                let cell = &mut sums[b][s];
                cell.0 += value;
                cell.1 += 1;
            }
            None => {
                nulls.insert((b, s));
            }
        }
    }

    let default_series = result.columns[y_ix].name.clone();
    let series_names = if series_ix.is_some() {
        series
    } else {
        // No series column: one series named after y, even if no row parsed.
        vec![default_series]
    };

    let mut notices: Vec<String> = Vec::new();
    if result.truncated {
        notices.push(truncated_notice(result));
    }
    if !left_out.is_empty() {
        notices.push(trf(
            "chart.notice.series_left_out",
            &[
                &MAX_SERIES.to_string(),
                &(series_names.len() + left_out.len()).to_string(),
            ],
        ));
    }
    if nonfinite > 0 {
        notices.push(trf("chart.notice.nonfinite", &[&nonfinite.to_string()]));
    }
    let mut breaks = 0usize;
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
                    _ if nulls.contains(&(ix, s)) => {
                        breaks += 1;
                        values.push(GAP);
                        present.push(false);
                    }
                    _ => {
                        values.push(0.0);
                        present.push(false);
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

    if breaks > 0 {
        notices.push(trf("chart.notice.null_breaks", &[&breaks.to_string()]));
    }

    let time_axis = result.columns[x_ix].kind == ColumnKind::Temporal;
    if matches!(plot.kind.as_str(), "bar" | "stacked_bar") && time_axis {
        if let Some(filled) = fill_missing_days(&points, series_names.len()) {
            points = filled;
        }
    }
    // Sorted before any averaging, so that a bucket holds neighbours in time.
    let on_a_time_axis = time_axis
        && matches!(plot.kind.as_str(), "line" | "scatter")
        && order_by_time(&mut points, &mut notices);
    if time_axis && plot.kind == "area" && !evenly_spaced(&points) {
        notices.push(tr("chart.notice.even_spacing").to_string());
    }

    let source_points = points.len();
    if plot.kind == "pie" {
        // Every band stays: the slices fold below, and a merged or dropped
        // band would make the shares wrong.
    } else if matches!(plot.kind.as_str(), "bar" | "stacked_bar" | "heatmap") {
        // A heatmap's columns are cells, not points a line can average.
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
            for (s, (sum, count)) in sums.into_iter().enumerate() {
                values.push(if count > 0 {
                    sum / count as f64
                } else if chunk.iter().any(|point| point.is_gap(s)) {
                    GAP
                } else {
                    0.0
                });
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
    let format = plot.format.as_deref().and_then(NumberFormat::parse);
    for point in points.iter_mut() {
        point.label = point
            .values
            .iter()
            .zip(&point.present)
            .find(|(_, present)| **present)
            .map(|(value, _)| SharedString::from(short_text(format, *value)))
            .unwrap_or_default();
    }

    let points: Vec<Arc<PlotPoint>> = points.into_iter().map(Arc::new).collect();
    let x_values = on_a_time_axis
        .then(|| time_line(&points).map(|(_, xs)| xs))
        .flatten();
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
        description: plot.description.clone().map(SharedString::from),
        kind: plot.kind.clone(),
        query: plot.query.clone(),
        label_name: result.columns[x_ix].name.clone(),
        series_names,
        series_slots: Vec::new(),
        known_slots: HashMap::new(),
        points,
        pie,
        geo: None,
        card: None,
        delta: None,
        note: None,
        tone: plot.tone.clone(),
        text: None,
        width: plot.width(),
        time_axis,
        x_is_time: x_values.is_some(),
        x_values,
        format: plot.format.as_deref().and_then(NumberFormat::parse),
        notice: crate::i18n::join_all(&notices),
        failure: None,
    }
}

/// In a band's `values`, a reading the query returned as NULL. Never `present`,
/// so nothing that reads present values sees it; see [`PlotPoint::is_gap`].
const GAP: f64 = f64::NAN;

impl PlotPoint {
    /// Whether series `s` has a NULL here, which a line stops at, rather than
    /// no row at all, which it crosses.
    pub(crate) fn is_gap(&self, s: usize) -> bool {
        !self.present.get(s).copied().unwrap_or(true)
            && self.values.get(s).is_some_and(|value| value.is_nan())
    }
}

fn truncated_notice(result: &QueryResult) -> String {
    trf("chart.notice.truncated", &[&result.rows.len().to_string()])
}

/// Seconds a date or time band stands for, on a scale of its own kind: from
/// the epoch for a date or a timestamp, an offset where it gives one, and from
/// midnight for a time of day. `None` for anything else, an interval included.
pub(crate) fn time_of(band: &str) -> Option<f64> {
    use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
    let seconds = |at: NaiveDateTime| {
        let at = at.and_utc();
        at.timestamp() as f64 + f64::from(at.timestamp_subsec_nanos()) / 1e9
    };
    for format in ["%Y-%m-%d %H:%M:%S%.f%#z", "%Y-%m-%dT%H:%M:%S%.f%#z"] {
        if let Ok(at) = DateTime::parse_from_str(band, format) {
            return Some(seconds(at.naive_utc()));
        }
    }
    for format in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"] {
        if let Ok(at) = NaiveDateTime::parse_from_str(band, format) {
            return Some(seconds(at));
        }
    }
    if let Ok(day) = NaiveDate::parse_from_str(band, "%Y-%m-%d") {
        return Some(seconds(day.and_hms_opt(0, 0, 0)?));
    }
    let at = NaiveTime::parse_from_str(band, "%H:%M:%S%.f").ok()?;
    Some(f64::from(at.num_seconds_from_midnight()) + f64::from(at.nanosecond()) / 1e9)
}

/// Points in time order, and each one's place on a linear axis.
pub(crate) type TimeLine = (Vec<Arc<PlotPoint>>, Arc<Vec<f64>>);

/// `points` in time order with each one's place in seconds from the first, for
/// a linear axis. `None` when a band is not a time or they are all one time,
/// and an even axis says as much as a linear one would.
pub(crate) fn time_line(points: &[Arc<PlotPoint>]) -> Option<TimeLine> {
    let mut timed = points
        .iter()
        .map(|point| Some((time_of(&point.band)?, point.clone())))
        .collect::<Option<Vec<_>>>()?;
    timed.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (first, last) = (timed.first()?.0, timed.last()?.0);
    if first == last {
        return None;
    }
    let xs = timed.iter().map(|(at, _)| at - first).collect();
    Some((
        timed.into_iter().map(|(_, point)| point).collect(),
        Arc::new(xs),
    ))
}

/// Puts `points` in time order, leaving out a band with no time, which has no
/// place on the axis, and saying so. False, with nothing changed, when another
/// band isn't a time: the axis then stays evenly spaced.
fn order_by_time(points: &mut Vec<PlotPoint>, notices: &mut Vec<String>) -> bool {
    let times: Option<Vec<Option<f64>>> = points
        .iter()
        .map(|point| match point.band.as_ref() {
            "NULL" => Some(None),
            band => time_of(band).map(Some),
        })
        .collect();
    let Some(times) = times else {
        return false;
    };
    let count = points.len();
    let mut timed: Vec<(f64, PlotPoint)> = times
        .into_iter()
        .zip(points.drain(..))
        .filter_map(|(at, point)| Some((at?, point)))
        .collect();
    if timed.len() < count {
        notices.push(trf(
            "chart.notice.no_time",
            &[&(count - timed.len()).to_string()],
        ));
    }
    timed.sort_by(|a, b| a.0.total_cmp(&b.0));
    points.extend(timed.into_iter().map(|(_, point)| point));
    true
}

/// Whether the bands are the same time apart, as an even axis draws them.
/// Bands that aren't times have no spacing to get wrong.
fn evenly_spaced(points: &[PlotPoint]) -> bool {
    let Some(times) = points
        .iter()
        .map(|point| time_of(&point.band))
        .collect::<Option<Vec<_>>>()
    else {
        return true;
    };
    let mut steps = times.windows(2).map(|pair| pair[1] - pair[0]);
    let Some(first) = steps.next() else {
        return true;
    };
    steps.all(|step| (step - first).abs() < 1e-6)
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
/// grouped by thousands (`1859708` → `1,859,708`) and a long fraction rounded
/// (see [`round_fraction`]), anything else — a date, a word, an exponent —
/// exactly as the database wrote it.
/// A text panel's words without the indentation every line shares: a
/// heredoc indented to sit inside its block keeps it otherwise, on every line
/// but the first.
fn dedent(text: &str) -> String {
    let lines: Vec<&str> = text.trim_matches('\n').lines().collect();
    // The first line arrives trimmed — the heredoc's text starts at its first
    // word — so the indentation to take off is the rest's.
    let common = lines
        .iter()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .enumerate()
        .map(|(ix, line)| {
            let line = if ix == 0 {
                line.trim_start()
            } else {
                line.get(common..).unwrap_or(line.trim_start())
            };
            line.trim_end()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether a column holds years by its name: `year`, `fiscal_year`,
/// `year_built`, `yr`.
fn is_year_column(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| word == "year" || word == "yr")
}

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
    // A lone `.0` is how DuckDB writes a whole DOUBLE — `round(x, 0)` comes
    // back as `79455942.0` — and says nothing. A DECIMAL's `.00` is kept: it
    // is a scale someone chose, as money's two places are.
    let frac = frac.filter(|frac| *frac != "0");
    // Round first: rounding can carry into the integer part (`9.999` → `10`).
    let (int, frac) = match frac {
        Some(frac) => round_fraction(int, frac),
        None => (int.to_string(), String::new()),
    };
    let mut grouped = String::with_capacity(int.len() + int.len() / 3);
    for (ix, ch) in int.chars().enumerate() {
        if ix > 0 && (int.len() - ix) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    if frac.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}.{frac}")
    }
}

/// A fraction as a headline number needs it. Two digits or fewer are the
/// database's own (a `DECIMAL(10,2)`'s `12.50` keeps its zero); a longer one
/// is mostly a float's binary noise (`11439704.020000005`), so it is rounded
/// to two places at 1 or more, and to three significant digits below 1
/// (`0.000123456` → `0.000123`), trailing zeros dropped.
fn round_fraction(int: &str, frac: &str) -> (String, String) {
    if frac.len() <= 2 {
        return (int.to_string(), frac.to_string());
    }
    let places = if int.trim_start_matches('0').is_empty() {
        frac.len()
            .min(frac.bytes().take_while(|&b| b == b'0').count() + 3)
    } else {
        2
    };
    let Ok(value) = format!("{int}.{frac}").parse::<f64>() else {
        return (int.to_string(), frac.to_string());
    };
    let rounded = format!("{value:.places$}");
    let (int, frac) = rounded.split_once('.').unwrap_or((&rounded, ""));
    (int.to_string(), frac.trim_end_matches('0').to_string())
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

/// A dashboard plot's series in name order — digits by value, letters
/// without regard to case — when a `series` column splits it. First
/// appearance is the database's choice wherever the query orders by x alone,
/// so the colors and the key traded places from one opening to the next.
/// The results chart keeps its own order: its SQL puts the largest first.
pub(crate) fn sort_series_by_name(plot: &mut PreparedPlot) {
    let mut names = plot.series_names.clone();
    names.sort_by(|a, b| crate::sources::natural_str_cmp(a, b));
    keep_series_order(plot, &names);
}

impl PreparedPlot {
    /// Where in the palette series `s` takes its color.
    pub(crate) fn slot(&self, s: usize) -> usize {
        self.series_slots.get(s).copied().unwrap_or(s)
    }

    /// What this plot draws of its query's result, for `open --state`: what
    /// the chart says under its title, as numbers an agent can read.
    pub(crate) fn scope(&self, result: Option<&QueryResult>) -> serde_json::Value {
        let plotted = match self.kind.as_str() {
            "table" | "card" | "text" => None,
            "pie" => Some(self.pie.len()),
            "map" => Some(self.geo.as_ref().map_or(0, |geo| geo.point_count())),
            _ => Some(self.points.len()),
        };
        serde_json::json!({
            "source_rows": result.map(|result| result.rows.len()),
            "query_truncated": result.map(|result| result.truncated),
            "plotted_points": plotted,
            "series": self.series_names.len(),
            "x_scale": match (&self.x_values, self.x_is_time) {
                (Some(_), true) => "time",
                (Some(_), false) => "number",
                (None, _) => "even",
            },
            "notice": self.notice,
        })
    }
}

/// Gives `plot`'s series the colors they had in `before`, by name. A color
/// follows position otherwise, so a pick that empties the first series would
/// recolor every one after it. A new series takes a color no series has had.
pub(crate) fn keep_series_colors(plot: &mut PreparedPlot, before: &PreparedPlot) {
    let mut known = before.known_slots.clone();
    for (s, name) in before.series_names.iter().enumerate() {
        known.entry(name.clone()).or_insert(before.slot(s));
    }
    let mut slots = Vec::with_capacity(plot.series_names.len());
    for name in &plot.series_names {
        let slot = match known.get(name) {
            Some(&slot) => slot,
            None => {
                let free = (0..)
                    .find(|slot| !known.values().any(|held| held == slot))
                    .unwrap_or_default();
                known.insert(name.clone(), free);
                free
            }
        };
        slots.push(slot);
    }
    plot.series_slots = slots;
    plot.known_slots = known;
}

/// `plot`'s series put back in the order `before` had them, the ones new to
/// it after: a series keeps its color when a pick re-runs the query. The
/// order is first appearance, and a query ordered by x alone leaves which
/// series comes first at each x to the database — Weekday and Weekend would
/// trade colors on a click.
pub(crate) fn keep_series_order(plot: &mut PreparedPlot, before: &[String]) {
    let names = &plot.series_names;
    let mut order: Vec<usize> = before
        .iter()
        .filter_map(|name| names.iter().position(|n| n == name))
        .collect();
    let new: Vec<usize> = (0..names.len()).filter(|ix| !order.contains(ix)).collect();
    order.extend(new);
    if order.iter().enumerate().all(|(at, &ix)| at == ix) {
        return;
    }
    plot.series_names = order.iter().map(|&ix| names[ix].clone()).collect();
    for point in &mut plot.points {
        *point = Arc::new(PlotPoint {
            band: point.band.clone(),
            label: point.label.clone(),
            values: order.iter().map(|&ix| point.values[ix]).collect(),
            present: order.iter().map(|&ix| point.present[ix]).collect(),
            ix: point.ix,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::{ColumnKind, ColumnMeta};

    #[test]
    fn a_series_keeps_its_color_while_others_come_and_go() {
        let run = |series: &[&str]| {
            let data = result(
                vec![
                    column("x", ColumnKind::Text),
                    column("y", ColumnKind::Numeric),
                    column("s", ColumnKind::Text),
                ],
                series.iter().map(|s| vec!["one", "1", *s]).collect(),
            );
            prepare(&plot("line", Some("y"), Some("s")), Some(&Ok(data)))
        };
        let next = |series: &[&str], before: &PreparedPlot| {
            let mut plot = run(series);
            keep_series_order(&mut plot, &before.series_names);
            keep_series_colors(&mut plot, before);
            plot
        };
        let color_of = |plot: &PreparedPlot, name: &str| {
            let s = plot.series_names.iter().position(|n| n == name).unwrap();
            plot.slot(s)
        };

        let all = run(&["A", "B", "C"]);
        assert_eq!(color_of(&all, "B"), 1);
        // A is filtered away, and B and C stay as they were.
        let narrowed = next(&["B", "C"], &all);
        assert_eq!(color_of(&narrowed, "B"), 1);
        assert_eq!(color_of(&narrowed, "C"), 2);
        // A new series takes no color anyone has had, not A's.
        let added = next(&["B", "C", "D"], &narrowed);
        assert_eq!(color_of(&added, "D"), 3);
        // Everything back, each as it left.
        let restored = next(&["D", "C", "B", "A"], &added);
        for (name, slot) in [("A", 0), ("B", 1), ("C", 2), ("D", 3)] {
            assert_eq!(color_of(&restored, name), slot, "{name}");
        }
    }

    /// `open --state` says what the chart does: thinned from how many, cut
    /// short or not, and the same words it shows.
    #[test]
    fn the_state_says_what_a_plot_draws_of_its_query() {
        let rows = MAX_SCATTER_POINTS + 10;
        let numbers: Vec<String> = (0..rows).map(|n| n.to_string()).collect();
        let data = QueryResult {
            truncated: true,
            ..result(
                vec![
                    column("x", ColumnKind::Numeric),
                    column("y", ColumnKind::Numeric),
                ],
                numbers.iter().map(|n| vec![n.as_str(), "1"]).collect(),
            )
        };
        let prepared = prepare(
            &plot("scatter", Some("y"), None),
            Some(&Ok::<_, String>(&data)),
        );
        let scope = prepared.scope(Some(&data));
        assert_eq!(scope["source_rows"], rows);
        assert_eq!(scope["query_truncated"], true);
        assert!(scope["plotted_points"].as_u64().unwrap() <= MAX_SCATTER_POINTS as u64);
        assert_eq!(scope["x_scale"], "number");
        assert_eq!(scope["series"], 1);
        assert_eq!(scope["notice"].as_str(), prepared.notice.as_deref());
        assert!(scope["notice"]
            .as_str()
            .unwrap()
            .contains(&rows.to_string()));

        // Before a result, or for a plot with nothing to plot, nothing claimed.
        let table = prepare(&plot("table", None, None), Some(&Ok::<_, String>(&data)));
        assert_eq!(
            table.scope(Some(&data))["plotted_points"],
            serde_json::Value::Null
        );
        assert_eq!(table.scope(None)["source_rows"], serde_json::Value::Null);
    }

    /// Loading has no plot yet; these are the two a finished query can be.
    #[test]
    fn an_empty_result_is_not_a_failure_and_a_failure_is_not_empty() {
        let empty = y_over(ColumnKind::Text, vec![]);
        for kind in ["line", "bar", "pie", "scatter", "area"] {
            let plot = prepare(&plot(kind, Some("y"), None), Some(&Ok::<_, String>(&empty)));
            assert_eq!(plot.failure, None, "{kind}");
            assert!(plot.points.is_empty() && plot.pie.is_empty(), "{kind}");
            let failed = prepare(
                &self::plot(kind, Some("y"), None),
                Some(&Err::<QueryResult, _>("Binder Error".to_string())),
            );
            assert_eq!(failed.failure.as_deref(), Some("Binder Error"), "{kind}");
        }
    }

    fn y_over(x: ColumnKind, rows: Vec<Vec<&str>>) -> QueryResult {
        result(vec![column("x", x), column("y", ColumnKind::Numeric)], rows)
    }

    #[test]
    fn a_line_over_times_is_placed_by_the_time_between_points() {
        let data = y_over(
            ColumnKind::Temporal,
            vec![
                vec!["2026-01-10", "3"],
                vec!["2026-01-01", "1"],
                vec!["2026-01-02", "2"],
            ],
        );
        let prepared = prepare(&plot("line", Some("y"), None), Some(&Ok(data)));
        let day = 86_400.;
        assert_eq!(
            prepared.x_values.as_deref().map(Vec::as_slice),
            Some(&[0., day, 9. * day][..])
        );
        assert!(prepared.x_is_time);
        // In time order, whatever order the query gave.
        let bands: Vec<&str> = prepared.points.iter().map(|p| p.band.as_ref()).collect();
        assert_eq!(bands, ["2026-01-01", "2026-01-02", "2026-01-10"]);
    }

    /// The forms DuckDB really renders, through the real conversion.
    #[test]
    fn every_time_duckdb_renders_has_a_place_on_the_axis() {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch("SET TimeZone = 'UTC'").ok();
        for (sql, expected) in [
            ("SELECT DATE '2026-01-02'", 1_767_312_000.),
            ("SELECT TIMESTAMP '2026-01-02 03:04:05'", 1_767_323_045.),
            ("SELECT TIMESTAMP '2026-01-02 03:04:05.5'", 1_767_323_045.5),
            ("SELECT TIME '03:04:05'", 11_045.),
            ("SELECT TIMESTAMP_S '2026-01-02 03:04:05'", 1_767_323_045.),
            ("SELECT TIMESTAMP_MS '2026-01-02 03:04:05'", 1_767_323_045.),
            ("SELECT TIMESTAMP_NS '2026-01-02 03:04:05'", 1_767_323_045.),
            (
                "SELECT TIMESTAMPTZ '2026-01-02 03:04:05+00'",
                1_767_323_045.,
            ),
        ] {
            let crate::query::QueryOutcome::Rows(rows) = crate::query::run_of(&conn, sql).unwrap()
            else {
                panic!("{sql} returns rows");
            };
            let cell = &rows.rows[0][0];
            assert_eq!(time_of(cell), Some(expected), "{sql} rendered as {cell}");
        }
        // Different offsets for one instant are one time.
        assert_eq!(
            time_of("2026-01-02 08:00:00+08"),
            time_of("2026-01-02 00:00:00+00")
        );
        assert_eq!(time_of("3 days"), None);
        assert_eq!(time_of("NULL"), None);
    }

    #[test]
    fn a_band_that_is_not_a_time_keeps_the_line_evenly_spaced() {
        let data = y_over(
            ColumnKind::Temporal,
            vec![vec!["3 days", "1"], vec!["1 day", "2"]],
        );
        let prepared = prepare(&plot("line", Some("y"), None), Some(&Ok(data)));
        assert_eq!(prepared.x_values, None);
        assert!(!prepared.x_is_time);
        assert_eq!(prepared.points[0].band.as_ref(), "3 days", "query order");
    }

    #[test]
    fn a_null_reading_keeps_its_time_and_breaks_the_line() {
        let data = y_over(
            ColumnKind::Temporal,
            vec![
                vec!["2026-01-01", "1"],
                vec!["2026-01-02", "NULL"],
                vec!["2026-01-03", "0"],
            ],
        );
        let prepared = prepare(&plot("line", Some("y"), None), Some(&Ok(data)));
        assert_eq!(prepared.points.len(), 3);
        assert!(prepared.points[1].is_gap(0));
        assert!(!prepared.points[1].present[0]);
        // A true zero is a reading.
        assert!(prepared.points[2].present[0]);
        assert!(!prepared.points[2].is_gap(0));
        assert_eq!(prepared.points[1].label.as_ref(), "");
        assert_eq!(
            prepared.notice.as_deref(),
            Some(trf("chart.notice.null_breaks", &["1"]).as_str())
        );
    }

    /// Only a line stops at a NULL: on a bar or a pie it stays what it was.
    #[test]
    fn a_null_reading_stays_out_of_every_other_chart() {
        for kind in ["bar", "pie", "area", "heatmap"] {
            let data = y_over(
                ColumnKind::Text,
                vec![vec!["a", "1"], vec!["b", "NULL"], vec!["c", "3"]],
            );
            let prepared = prepare(&plot(kind, Some("y"), None), Some(&Ok(data)));
            assert_eq!(prepared.points.len(), 2, "{kind}");
            assert!(
                prepared.points.iter().all(|p| p.values[0].is_finite()),
                "{kind}"
            );
        }
    }

    #[test]
    fn a_row_with_no_time_says_it_is_left_off_a_time_axis() {
        let data = y_over(
            ColumnKind::Temporal,
            vec![
                vec!["2026-01-01", "1"],
                vec!["NULL", "9"],
                vec!["2026-01-03", "3"],
            ],
        );
        let prepared = prepare(&plot("line", Some("y"), None), Some(&Ok(data)));
        assert_eq!(prepared.points.len(), 2);
        assert!(prepared.x_is_time);
        assert_eq!(
            prepared.notice.as_deref(),
            Some(trf("chart.notice.no_time", &["1"]).as_str())
        );
    }

    #[test]
    fn an_area_over_uneven_times_says_it_is_evenly_spaced() {
        let notice = |rows| {
            prepare(
                &plot("area", Some("y"), None),
                Some(&Ok(y_over(ColumnKind::Temporal, rows))),
            )
            .notice
        };
        assert_eq!(
            notice(vec![
                vec!["2026-01-01", "1"],
                vec!["2026-01-02", "2"],
                vec!["2026-01-10", "3"],
            ])
            .as_deref(),
            Some(tr("chart.notice.even_spacing"))
        );
        assert_eq!(
            notice(vec![
                vec!["2026-01-01", "1"],
                vec!["2026-01-02", "2"],
                vec!["2026-01-03", "3"],
            ]),
            None
        );
    }

    /// Over the real row budget: a share of part of the data is not a share.
    #[test]
    fn every_chart_says_when_its_query_was_cut_short() {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        let crate::query::QueryOutcome::Rows(cut) = crate::query::run_of(
            &conn,
            "SELECT (range % 5)::VARCHAR AS x, 1 AS y FROM range(100001)",
        )
        .unwrap() else {
            panic!("a SELECT returns rows");
        };
        assert!(cut.truncated);
        let expected = trf("chart.notice.truncated", &[&cut.rows.len().to_string()]);
        for kind in [
            "pie",
            "bar",
            "stacked_bar",
            "line",
            "area",
            "heatmap",
            "scatter",
        ] {
            let prepared = prepare(&plot(kind, Some("y"), None), Some(&Ok::<_, String>(&cut)));
            let notice = prepared.notice.unwrap_or_default();
            assert!(notice.contains(&expected), "{kind}: {notice:?}");
        }
        // And a map, and a numeric scatter, which go their own ways.
        let located = QueryResult {
            truncated: true,
            ..result(
                vec![
                    column("lat", ColumnKind::Numeric),
                    column("lng", ColumnKind::Numeric),
                ],
                vec![vec!["1", "2"]],
            )
        };
        let map = prepare(&plot("map", None, None), Some(&Ok(located)));
        assert_eq!(
            map.notice.as_deref(),
            Some(trf("chart.notice.truncated", &["1"]).as_str())
        );
        let data = QueryResult {
            truncated: true,
            ..y_over(ColumnKind::Numeric, vec![vec!["1", "2"]])
        };
        let prepared = prepare(&plot("scatter", Some("y"), None), Some(&Ok(data)));
        assert!(prepared.x_values.is_some());
        assert!(prepared
            .notice
            .unwrap()
            .contains(&trf("chart.notice.truncated", &["1"])));
    }

    #[test]
    fn nan_and_infinity_do_not_reach_a_chart_and_are_counted() {
        for bad in ["NaN", "inf", "-inf"] {
            let data = y_over(ColumnKind::Text, vec![vec!["a", "1"], vec!["a", bad]]);
            let prepared = prepare(&plot("line", Some("y"), None), Some(&Ok(data)));
            assert_eq!(prepared.points[0].values[0], 1., "{bad}");
            assert_eq!(
                prepared.notice.as_deref(),
                Some(trf("chart.notice.nonfinite", &["1"]).as_str())
            );
        }
        let data = y_over(
            ColumnKind::Numeric,
            vec![vec!["1", "2"], vec!["inf", "3"], vec!["3", "NaN"]],
        );
        let prepared = prepare(&plot("scatter", Some("y"), None), Some(&Ok(data)));
        assert_eq!(prepared.points.len(), 1);
        assert_eq!(
            prepared.notice.as_deref(),
            Some(trf("chart.notice.nonfinite", &["2"]).as_str())
        );
    }

    /// More rows of a series past the cap are not more series.
    #[test]
    fn the_series_cap_counts_series_not_rows() {
        for x in [ColumnKind::Text, ColumnKind::Numeric] {
            let mut data = result(
                vec![
                    column("x", x),
                    column("y", ColumnKind::Numeric),
                    column("s", ColumnKind::Text),
                ],
                vec![],
            );
            for i in 0..=MAX_SERIES {
                data.rows
                    .push(vec!["1".into(), "1".into(), format!("s{i}")]);
            }
            for _ in 0..99 {
                data.rows
                    .push(vec!["1".into(), "1".into(), format!("s{MAX_SERIES}")]);
            }
            let prepared = prepare(&plot("scatter", Some("y"), Some("s")), Some(&Ok(data)));
            assert_eq!(
                prepared.notice.as_deref(),
                Some(
                    trf(
                        "chart.notice.series_left_out",
                        &[&MAX_SERIES.to_string(), &(MAX_SERIES + 1).to_string()]
                    )
                    .as_str()
                )
            );
        }
    }

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
            note: None,
            description: None,
            tone: None,
            format: None,
            delta: None,
            delta_format: None,
            down_is_good: false,
            text: None,
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
            text_nulls: Default::default(),
        }
    }

    #[test]
    fn a_numeric_scatter_keeps_every_row_as_a_dot_in_x_order() {
        let data = result(
            vec![
                column("x", ColumnKind::Numeric),
                column("y", ColumnKind::Numeric),
                column("kind", ColumnKind::Text),
            ],
            vec![
                vec!["190", "3500", "Adelie"],
                vec!["181", "3750", "Adelie"],
                vec!["190", "4200", "Gentoo"],
                vec!["NA", "4000", "Gentoo"],
                vec!["190", "3900", "Adelie"],
            ],
        );
        let prepared = prepare(&plot("scatter", Some("y"), Some("kind")), Some(&Ok(data)));
        // Three birds share x = 190 and stay three dots; the row without a
        // number for x is left out, not drawn at zero.
        assert_eq!(
            prepared.x_values.as_deref().map(Vec::as_slice),
            Some([181., 190., 190., 190.].as_slice())
        );
        assert_eq!(prepared.series_names, ["Adelie", "Gentoo"]);
        let drawn: Vec<(usize, f64)> = prepared
            .points
            .iter()
            .map(|p| {
                let s = p.present.iter().position(|present| *present).unwrap();
                (s, p.values[s])
            })
            .collect();
        assert_eq!(drawn[0], (0, 3750.));
        assert_eq!(drawn.len(), 4);
        assert!(prepared.notice.is_none());
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
            Some(&Ok(data(vec![
                vec!["2026-10-02", "3"],
                vec!["2026-10-05", "4"],
            ]))),
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
        assert_eq!(
            days,
            ["2026-10-05 00:00:00", "2026-10-04", "2026-10-03 00:00:00"]
        );

        // A time of day, or days out of order, are left as the query gave them.
        for rows in [
            vec![
                vec!["2026-10-02 12:30:00", "1"],
                vec!["2026-10-05 08:00:00", "2"],
            ],
            vec![
                vec!["2026-10-05", "1"],
                vec!["2026-10-02", "2"],
                vec!["2026-10-09", "3"],
            ],
        ] {
            let prepared = prepare(&bar, Some(&Ok(data(rows))));
            assert!(prepared.points.iter().all(|p| p.present[0]));
        }

        // A text x is a category, however much it looks like a date.
        let text = result(
            vec![
                column("day", ColumnKind::Text),
                column("y", ColumnKind::Numeric),
            ],
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
        // A missing bar says so itself: no line under the title for it.
        assert!(prepared.notice.is_none());
    }

    #[test]
    fn a_heatmap_leaves_a_missing_cell_empty_and_caps_its_columns() {
        let mut rows = vec![vec!["d1", "a", "1"], vec!["d2", "b", "20"]];
        let bands: Vec<String> = (0..MAX_BARS + 5).map(|i| format!("h{i:03}")).collect();
        for band in &bands {
            rows.push(vec![band.as_str(), "a", "3"]);
        }
        let data = result(
            vec![
                column("x", ColumnKind::Text),
                column("svc", ColumnKind::Text),
                column("y", ColumnKind::Numeric),
            ],
            rows,
        );
        let prepared = prepare(&plot("heatmap", Some("y"), Some("svc")), Some(&Ok(data)));
        assert_eq!(prepared.series_names, ["a", "b"]);
        assert_eq!(prepared.points.len(), MAX_BARS, "columns cap as bars do");
        // d1 has no b: an empty cell.
        assert_eq!(prepared.points[0].present, [true, false]);
        let notice = prepared.notice.unwrap();
        assert!(notice.contains(&(MAX_BARS + 7).to_string()), "{notice}");
    }

    #[test]
    fn a_rerun_keeps_each_series_in_its_place() {
        let rows = |first: &str, second: &str| {
            result(
                vec![
                    column("x", ColumnKind::Temporal),
                    column("svc", ColumnKind::Text),
                    column("y", ColumnKind::Numeric),
                ],
                vec![
                    vec!["d1", first, "1"],
                    vec!["d1", second, "10"],
                    vec!["d2", "a", "2"],
                ],
            )
        };
        let spec = plot("line", Some("y"), Some("svc"));
        let before = prepare(&spec, Some(&Ok(rows("a", "b"))));
        // The database put b first this time: a keeps the first color.
        let mut after = prepare(&spec, Some(&Ok(rows("b", "a"))));
        assert_eq!(after.series_names, ["b", "a"]);
        keep_series_order(&mut after, &before.series_names);
        assert_eq!(after.series_names, ["a", "b"]);
        assert_eq!(after.points[0].values, [10.0, 1.0]);
        assert_eq!(after.points[1].present, [true, false]);
        // A series new to the plot comes after the ones it had.
        let mut grown = prepare(&spec, Some(&Ok(rows("c", "a"))));
        keep_series_order(&mut grown, &["a".to_string()]);
        assert_eq!(grown.series_names, ["a", "c"]);
    }

    #[test]
    fn a_dashboard_split_reads_in_name_order() {
        let rows = result(
            vec![
                column("x", ColumnKind::Temporal),
                column("svc", ColumnKind::Text),
                column("y", ColumnKind::Numeric),
            ],
            vec![
                vec!["d1", "web 10", "1"],
                vec!["d1", "Chat", "2"],
                vec!["d1", "web 9", "3"],
            ],
        );
        let mut prepared = prepare(&plot("line", Some("y"), Some("svc")), Some(&Ok(rows)));
        sort_series_by_name(&mut prepared);
        assert_eq!(prepared.series_names, ["Chat", "web 9", "web 10"]);
        assert_eq!(prepared.points[0].values, [2.0, 3.0, 1.0]);
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
            text_nulls: Default::default(),
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
            text_nulls: Default::default(),
        });
        let prepared = prepare(&map_plot(None, None, None), Some(&result));
        assert!(prepared.geo.is_none());
        assert!(prepared.failure.is_some());

        let prepared = prepare(
            &map_plot(Some("nope"), Some("geo_lng"), None),
            Some(&Ok(stations())),
        );
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
            columns: vec![
                column("x", ColumnKind::Text),
                column("y", ColumnKind::Numeric),
            ],
            rows,
            elapsed_ms: 0,
            truncated: false,
            text_nulls: Default::default(),
        });
        let pie = prepare(&plot("pie", Some("y"), None), Some(&result));
        assert_eq!(pie.points.len(), 301, "nothing truncated");
        assert_eq!(pie.pie.len(), 8, "folded to the largest slices");
        let total: f64 = pie.pie.iter().map(|s| s.value() as f64).sum();
        assert_eq!(total, (1..=300).sum::<i64>() as f64);
        assert!(
            pie.notice.unwrap().contains('1'),
            "the negative band is reported"
        );
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
        assert_eq!(
            prepare(&plot("line", Some("y"), None), Some(&data())).width,
            12
        );
    }

    #[test]
    fn a_format_reads_a_cards_value_and_a_plots_labels() {
        let money = Ok(result(
            vec![
                column("x", ColumnKind::Text),
                column("y", ColumnKind::Numeric),
            ],
            vec![vec!["web", "1234.5"], vec!["app", "-25000"]],
        ));
        let card = Plot {
            value: Some("y".into()),
            format: Some("usd".into()),
            ..plot("card", None, None)
        };
        let shown = prepare(&card, Some(&money));
        assert_eq!(shown.card.as_deref(), Some("$1,234.50"));

        let bars = Plot {
            format: Some("usd0".into()),
            ..plot("bar", Some("y"), None)
        };
        let shown = prepare(&bars, Some(&money));
        assert_eq!(shown.format, NumberFormat::parse("usd0"));
        let labels: Vec<&str> = shown.points.iter().map(|p| p.label.as_ref()).collect();
        assert_eq!(labels, ["$1,235", "-$25K"]);

        // A card whose cell is no number shows it as written.
        let words = Ok(result(
            vec![column("x", ColumnKind::Text)],
            vec![vec!["n/a"]],
        ));
        let card = Plot {
            format: Some("pct".into()),
            ..plot("card", None, None)
        };
        assert_eq!(prepare(&card, Some(&words)).card.as_deref(), Some("n/a"));
    }

    #[test]
    fn a_cards_delta_points_and_says_whether_it_is_good() {
        let row = |change: &str| {
            Ok(result(
                vec![
                    column("revenue", ColumnKind::Numeric),
                    column("change", ColumnKind::Numeric),
                ],
                vec![vec!["1000", change]],
            ))
        };
        let card = |down_is_good: bool| Plot {
            value: Some("revenue".into()),
            format: Some("usd0".into()),
            delta: Some("change".into()),
            down_is_good,
            ..plot("card", None, None)
        };
        let delta = |plot: &Plot, change: &str| prepare(plot, Some(&row(change))).delta;

        let up = delta(&card(false), "125").unwrap();
        assert_eq!(
            up.text, "▲ $125",
            "the card's format, the arrow for the sign"
        );
        assert_eq!(up.mood, Mood::Good);
        let down = delta(&card(false), "-125").unwrap();
        assert_eq!(down.text, "▼ $125");
        assert_eq!(down.mood, Mood::Bad);
        // Costs: a fall is good news.
        assert_eq!(delta(&card(true), "-125").unwrap().mood, Mood::Good);
        assert_eq!(delta(&card(true), "125").unwrap().mood, Mood::Bad);
        assert_eq!(delta(&card(false), "0").unwrap().mood, Mood::Flat);

        // A growth rate reads in its own format.
        let rate = Plot {
            delta_format: Some("pct1".into()),
            ..card(false)
        };
        assert_eq!(delta(&rate, "0.123").unwrap().text, "▲ 12.3%");
        // No number, no change shown.
        assert_eq!(delta(&card(false), ""), None);
        let missing = Plot {
            delta: Some("nope".into()),
            ..card(false)
        };
        assert!(prepare(&missing, Some(&row("1"))).failure.is_some());
    }

    #[test]
    fn a_card_note_is_words_or_its_rows_cell() {
        let data = || {
            Ok(result(
                vec![
                    column("n", ColumnKind::Numeric),
                    column("day", ColumnKind::Text),
                ],
                vec![vec!["2999", "Sat, Jul 11"]],
            ))
        };
        let card = |note: Note| Plot {
            value: Some("n".into()),
            note: Some(note),
            tone: Some("accent".into()),
            ..plot("card", None, None)
        };
        let from_column = prepare(&card(Note::Column("day".into())), Some(&data()));
        assert_eq!(from_column.note.as_deref(), Some("Sat, Jul 11"));
        assert_eq!(from_column.tone.as_deref(), Some("accent"));
        let words = prepare(&card(Note::Text("All riders".into())), Some(&data()));
        assert_eq!(words.note.as_deref(), Some("All riders"));
        let missing = prepare(&card(Note::Column("nope".into())), Some(&data()));
        assert!(missing.failure.unwrap().contains("nope"));
    }

    #[test]
    fn a_text_panel_needs_no_result() {
        let text = Plot {
            query: String::new(),
            text: Some("24 stations".into()),
            ..plot("text", None, None)
        };
        let shown = prepare::<QueryResult>(&text, None);
        assert!(shown.failure.is_none(), "{:?}", shown.failure);
        assert_eq!(
            shown.title, "",
            "no title is no heading, not the plot's name"
        );
        assert_eq!(shown.text.as_deref(), Some("24 stations"));
    }

    #[test]
    fn only_plain_numbers_are_grouped() {
        assert_eq!(group_thousands("1234567.891"), "1,234,567.89");
        // A float's noise is rounded away; a decimal's own digits are kept.
        assert_eq!(group_thousands("11439704.020000005"), "11,439,704.02");
        assert_eq!(group_thousands("12.50"), "12.50");
        assert_eq!(group_thousands("79455942.0"), "79,455,942");
        assert!(is_year_column("year") && is_year_column("Fiscal_Year"));
        assert!(is_year_column("yr") && !is_year_column("yearly_total"));
        assert_eq!(
            dedent("Annual CO2.\n  Land use is out.\n"),
            "Annual CO2.\nLand use is out."
        );
        assert_eq!(dedent("a\n    b\n      c"), "a\nb\n  c");
        assert_eq!(group_thousands("12.00"), "12.00");
        assert_eq!(group_thousands("-0.000123456"), "-0.000123");
        assert_eq!(group_thousands("999999.999"), "1,000,000");
        assert_eq!(group_thousands("3.10000001"), "3.1");
        assert_eq!(group_thousands("-1000"), "-1,000");
        assert_eq!(group_thousands("999"), "999");
        assert_eq!(group_thousands("1e10"), "1e10");
        assert_eq!(group_thousands("2026-09-30"), "2026-09-30");
        assert_eq!(group_thousands(""), "");
    }
}
