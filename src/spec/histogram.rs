//! A `histogram`: how many rows fall in each range of one numeric column.
//!
//! The ranges are bins of one round width — 1, 2, 2.5 or 5 times a power of
//! ten — starting at a multiple of it, so every edge is a number a reader
//! would write. The width is the first round one at least as wide as the
//! square root of the row count of bins would be, that count held between
//! `MIN_BINS` and `MAX_BINS`; starting on a round edge can add one more.
//!
//! A dashboard bins the rows its query returned, here; the chart tab bins
//! the whole result in DuckDB with [`sql`], which works the width out the
//! same way. The two must agree, so a chart copied as a `.dash` block draws
//! the bins it showed.

use std::sync::Arc;

use gpui_kit::SharedString;

use super::format::{short_text, NumberFormat};
use crate::spec::prepare::PlotPoint;

/// Fewer bins than this hides the shape; more than this is noise at a
/// dashboard plot's width.
pub(crate) const MIN_BINS: usize = 5;
pub(crate) const MAX_BINS: usize = 40;
/// How far past a round width a computed one may land and still be it.
const ROUND_SLACK: f64 = 1e-9;

/// The columns [`sql`] answers with: a bin's index from 0, its rows, and the
/// bins' start, width and count, the same on every row.
pub(crate) const BIN_COLUMN: &str = "__ducklocal_bin";
pub(crate) const ROWS_COLUMN: &str = "__ducklocal_rows";
pub(crate) const START_COLUMN: &str = "__ducklocal_start";
pub(crate) const STEP_COLUMN: &str = "__ducklocal_step";
pub(crate) const BINS_COLUMN: &str = "__ducklocal_bins";

/// Equal bins from `start`, each `step` wide.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Bins {
    pub start: f64,
    pub step: f64,
    pub count: usize,
}

impl Bins {
    /// Bins for `n` values from `lo` to `hi`. One value throughout gets one
    /// bin around it.
    pub(crate) fn of(lo: f64, hi: f64, n: usize) -> Self {
        let wanted = ((n as f64).sqrt().round() as usize).clamp(MIN_BINS, MAX_BINS) as f64;
        let rough = if hi > lo {
            (hi - lo) / wanted
        } else {
            lo.abs().max(1.)
        };
        let power = 10f64.powf(rough.log10().floor());
        // Rounded up, never down: a narrower bin than asked for would make
        // more of them than asked for. The slack keeps a width that is
        // already round — 20 read back as 20.000000000000004 — from
        // rounding up to the next.
        let error = rough / power;
        let multiple = [1., 2., 2.5, 5.]
            .into_iter()
            .find(|m| error <= m * (1. + ROUND_SLACK))
            .unwrap_or(10.);
        let step = multiple * power;
        let start = (lo / step).floor() * step;
        let count = (((hi - start) / step).ceil() as usize).max(1);
        Self { start, step, count }
    }

    /// The bin `value` falls in; the top edge belongs to the last bin.
    pub(crate) fn index(&self, value: f64) -> usize {
        (((value - self.start) / self.step).floor().max(0.) as usize).min(self.count - 1)
    }
}

/// The finite values of `values` binned: the bins and each one's rows.
pub(crate) fn bin(values: &[f64]) -> Option<(Bins, Vec<u64>)> {
    let finite = values.iter().copied().filter(|v| v.is_finite());
    let (lo, hi, n) = finite.fold((f64::INFINITY, f64::NEG_INFINITY, 0), |(lo, hi, n), v| {
        (lo.min(v), hi.max(v), n + 1)
    });
    if n == 0 {
        return None;
    }
    let bins = Bins::of(lo, hi, n);
    let mut counts = vec![0u64; bins.count];
    for v in values.iter().filter(|v| v.is_finite()) {
        counts[bins.index(*v)] += 1;
    }
    Some((bins, counts))
}

/// A bar per bin, empty ones included — a range no row falls in is part of
/// the shape — each banded by its range in `format`.
pub(crate) fn points(
    bins: Bins,
    counts: &[u64],
    format: Option<NumberFormat>,
) -> Vec<Arc<PlotPoint>> {
    counts
        .iter()
        .enumerate()
        .map(|(ix, &rows)| {
            let lo = bins.start + bins.step * ix as f64;
            let hi = lo + bins.step;
            Arc::new(PlotPoint {
                band: format!("{}–{}", short_text(format, lo), short_text(format, hi)).into(),
                label: SharedString::from(rows.to_string()),
                values: vec![rows as f64],
                present: vec![true],
                ix,
            })
        })
        .collect()
}

/// SQL that bins `x` over `relation` (parenthesized) the way [`Bins::of`]
/// does: one row per bin that has rows, in order, in the columns above.
pub(crate) fn sql(x: &str, relation: &str) -> String {
    let x = crate::overview::quote(x);
    format!(
        "WITH v AS (\n\
         SELECT {x}::DOUBLE AS v FROM {relation}\n\
         WHERE {x} IS NOT NULL AND isfinite({x}::DOUBLE)\n\
         ), s AS (\n\
         SELECT min(v) AS lo, max(v) AS hi,\n\
         least(greatest(round(sqrt(count(*))), {MIN_BINS}), {MAX_BINS}) AS wanted\n\
         FROM v\n\
         ), r AS (\n\
         SELECT lo, hi, CASE WHEN hi > lo THEN (hi - lo) / wanted ELSE greatest(abs(lo), 1) END AS rough\n\
         FROM s WHERE lo IS NOT NULL\n\
         ), p AS (\n\
         SELECT lo, hi, rough, pow(10, floor(log10(rough))) AS power FROM r\n\
         ), w AS (\n\
         SELECT lo, hi, CASE WHEN rough / power <= 1 * (1 + {ROUND_SLACK}) THEN 1\n\
         WHEN rough / power <= 2 * (1 + {ROUND_SLACK}) THEN 2\n\
         WHEN rough / power <= 2.5 * (1 + {ROUND_SLACK}) THEN 2.5\n\
         WHEN rough / power <= 5 * (1 + {ROUND_SLACK}) THEN 5 ELSE 10 END * power AS step\n\
         FROM p\n\
         ), b AS (\n\
         SELECT floor(lo / step) * step AS start, step, hi FROM w\n\
         ), n AS (\n\
         SELECT start, step, greatest(ceil((hi - start) / step), 1)::BIGINT AS bins FROM b\n\
         )\n\
         SELECT least(greatest(floor((v - start) / step), 0)::BIGINT, bins - 1) AS {BIN_COLUMN},\n\
         count(*) AS {ROWS_COLUMN}, any_value(start) AS {START_COLUMN},\n\
         any_value(step) AS {STEP_COLUMN}, any_value(bins) AS {BINS_COLUMN}\n\
         FROM v, n\n\
         GROUP BY 1\n\
         ORDER BY 1"
    )
}

/// The bins and their rows from [`sql`]'s answer, the bins no row fell in
/// filled with 0. `None` when it holds no bin.
pub(crate) fn from_answer(answer: &crate::query::QueryResult) -> Option<(Bins, Vec<u64>)> {
    let at = |name: &str| answer.columns.iter().position(|c| c.name == name);
    let (bin, rows, start, step, count) = (
        at(BIN_COLUMN)?,
        at(ROWS_COLUMN)?,
        at(START_COLUMN)?,
        at(STEP_COLUMN)?,
        at(BINS_COLUMN)?,
    );
    let first = answer.rows.first()?;
    let number = |row: &Vec<String>, ix: usize| crate::ui::chart::parse_number(row.get(ix)?);
    let bins = Bins {
        start: number(first, start)?,
        step: number(first, step)?,
        count: number(first, count)? as usize,
    };
    if bins.count == 0 || bins.count > MAX_BINS * 10 {
        return None;
    }
    let mut counts = vec![0u64; bins.count];
    for row in &answer.rows {
        let (Some(ix), Some(n)) = (number(row, bin), number(row, rows)) else {
            continue;
        };
        if let Some(slot) = counts.get_mut(ix as usize) {
            *slot = n as u64;
        }
    }
    Some((bins, counts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bins_start_and_step_on_round_numbers() {
        // 100 values from 3 to 97: ten bins of 10 from 0.
        let bins = Bins::of(3., 97., 100);
        assert_eq!((bins.start, bins.step, bins.count), (0., 10., 10));
        assert_eq!(bins.index(3.), 0);
        assert_eq!(bins.index(97.), 9);
        // The top edge is the last bin's, not a bin of its own.
        assert_eq!(Bins::of(0., 100., 100).index(100.), 9);
        // One value throughout: one bin around it.
        let one = Bins::of(42., 42., 1_000);
        assert_eq!(one.count, 1);
        assert!(one.start <= 42. && 42. < one.start + one.step);
    }

    #[test]
    fn binning_skips_what_is_not_finite_and_keeps_empty_bins() {
        let mut values: Vec<f64> = (0..50).map(|i| i as f64).collect();
        values.extend([1000., f64::NAN, f64::INFINITY]);
        let (bins, counts) = bin(&values).unwrap();
        assert_eq!(counts.iter().sum::<u64>(), 51);
        assert_eq!(counts.len(), bins.count);
        assert!(counts.contains(&0), "the gap below 1000 stays");
        assert!(bin(&[f64::NAN]).is_none());
    }

    #[test]
    fn duckdb_bins_the_way_the_dashboard_does() {
        let conn =
            duckdb::Connection::open_in_memory_with_flags(crate::db::config().unwrap()).unwrap();
        for statement in [
            "SELECT (i * 7 % 1000) / 3.0 AS x FROM range(5000) t(i)",
            "SELECT 0.001 * i AS x FROM range(17) t(i)",
            "SELECT -250 + i * 1.5 AS x FROM range(900) t(i) UNION ALL SELECT NULL",
            "SELECT 5 AS x FROM range(30)",
        ] {
            let rows = match crate::query::run_of(&conn, statement).unwrap() {
                crate::query::QueryOutcome::Rows(rows) => rows,
                other => panic!("{other:?}"),
            };
            let values: Vec<f64> = rows
                .rows
                .iter()
                .filter_map(|row| crate::ui::chart::parse_number(&row[0]))
                .collect();
            let here = bin(&values).unwrap();
            let sql = sql("x", &format!("(\n{statement}\n)"));
            let answer = match crate::query::run_of(&conn, &sql).unwrap() {
                crate::query::QueryOutcome::Rows(answer) => answer,
                other => panic!("{other:?}"),
            };
            let there = from_answer(&answer).unwrap();
            assert_eq!(here.0.count, there.0.count, "{statement}");
            assert!((here.0.start - there.0.start).abs() < 1e-9, "{statement}");
            assert!((here.0.step - there.0.step).abs() < 1e-9, "{statement}");
            assert_eq!(here.1, there.1, "{statement}");
        }
    }
}
