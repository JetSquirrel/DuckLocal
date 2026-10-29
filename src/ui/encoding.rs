//! The chart tab's column picks: which column runs along x, which is
//! measured on y and how its rows combine, and which splits the measure into
//! series — the shelves of a drag-and-drop explorer, as three menus.
//!
//! A pick is grouped by DuckDB over the whole result, not over the rows the
//! grid loaded: the result's query is wrapped in a `GROUP BY`, and only the
//! bands and series the chart can show come back. Those rows then go through
//! the same preparation as a dashboard plot, so a pick draws what a `.dash`
//! `plot` block with the same columns would.
//!
//! Which bands come back: along an ordered x (a time or a number) the first
//! ones; along categories the largest, by their total. Which series: the
//! largest by total size. Each answer row also says how many there were in
//! all, so the chart can say what it left out.

use crate::i18n::tr;
use crate::query::{ColumnKind, ColumnMeta};
use crate::spec::model::Plot;
use crate::ui::chart::{ChartKind, MAX_BARS, MAX_SERIES};

/// How many categories come back: a bar chart's worth. A pie folds them
/// further.
pub(crate) const MAX_CATEGORY_BANDS: usize = MAX_BARS;
/// How many points along an ordered x come back, before the chart's own
/// downsampling averages them into a drawable line.
pub(crate) const MAX_ORDERED_BANDS: usize = 50_000;
/// The answer's last three columns: a row's band, counted from 1 in the
/// chart's order, and how many bands and series there were in all.
pub(crate) const BAND_COLUMN: &str = "__ducklocal_band";
pub(crate) const BANDS_COLUMN: &str = "__ducklocal_bands";
pub(crate) const SERIES_COLUMN: &str = "__ducklocal_series";

/// How the rows that share a band combine into its value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Aggregate {
    #[default]
    Sum,
    Avg,
    Min,
    Max,
}

impl Aggregate {
    pub const ALL: [Aggregate; 4] = [
        Aggregate::Sum,
        Aggregate::Avg,
        Aggregate::Min,
        Aggregate::Max,
    ];

    fn function(self) -> &'static str {
        match self {
            Aggregate::Sum => "sum",
            Aggregate::Avg => "avg",
            Aggregate::Min => "min",
            Aggregate::Max => "max",
        }
    }

    pub fn label(self) -> &'static str {
        tr(match self {
            Aggregate::Sum => "chart.agg.sum",
            Aggregate::Avg => "chart.agg.avg",
            Aggregate::Min => "chart.agg.min",
            Aggregate::Max => "chart.agg.max",
        })
    }
}

/// What the user mapped onto the chart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Encoding {
    pub x: String,
    /// The measured column; `None` counts rows.
    pub y: Option<String>,
    pub agg: Aggregate,
    /// The column whose values become series.
    pub series: Option<String>,
}

impl Encoding {
    /// Where a first pick starts: Auto's own x, the first number after it
    /// summed (or rows counted when there is none), no split.
    pub fn starting(columns: &[ColumnMeta]) -> Option<Self> {
        let x = columns.first()?;
        let y = columns
            .iter()
            .skip(1)
            .find(|c| c.kind == ColumnKind::Numeric)
            .map(|c| c.name.clone());
        Some(Self {
            x: x.name.clone(),
            y,
            agg: Aggregate::default(),
            series: None,
        })
    }

    /// The measure as the chart names it: `sum(amount)`, `count(*)`.
    pub fn value_name(&self) -> String {
        match &self.y {
            Some(y) => format!("{}({y})", self.agg.function()),
            None => "count(*)".to_string(),
        }
    }

    /// Whether this pick can be drawn from a result with `columns`: every
    /// column it names is there once, y is a number, and no two of x, the
    /// split and the measure share a name. A pick kept from an earlier
    /// result that does not fit leaves the chart on Auto, without being
    /// forgotten.
    pub fn applies_to(&self, columns: &[ColumnMeta]) -> bool {
        let once = |name: &str| columns.iter().filter(|c| c.name == name).count() == 1;
        if !once(&self.x) {
            return false;
        }
        if let Some(y) = &self.y {
            if !once(y) || !column(columns, y).is_some_and(|c| c.kind == ColumnKind::Numeric) {
                return false;
            }
        }
        if let Some(series) = &self.series {
            if !once(series) {
                return false;
            }
        }
        // `prepare` finds columns ignoring case, so case alone does not tell
        // two apart.
        let value = self.value_name().to_lowercase();
        let mut names = vec![self.x.to_lowercase(), value];
        names.extend(self.series.iter().map(|s| s.to_lowercase()));
        names.sort();
        names.dedup();
        names.len() == 2 + usize::from(self.series.is_some())
    }

    /// Whether x runs in order (a time or a number) rather than naming
    /// categories.
    pub fn x_is_ordered(&self, columns: &[ColumnMeta]) -> bool {
        column(columns, &self.x)
            .is_some_and(|c| matches!(c.kind, ColumnKind::Temporal | ColumnKind::Numeric))
    }

    /// Whether a histogram can draw this pick: x is a number to bin, and
    /// no split asks for more than one count per range.
    pub fn binnable(&self, columns: &[ColumnMeta]) -> bool {
        self.series.is_none()
            && column(columns, &self.x).is_some_and(|c| c.kind == ColumnKind::Numeric)
    }

    pub fn x_is_temporal(&self, columns: &[ColumnMeta]) -> bool {
        column(columns, &self.x).is_some_and(|c| c.kind == ColumnKind::Temporal)
    }

    /// SQL that groups `relation` (the result's query, parenthesized) by this
    /// pick: one row per band and series, ordered as the chart draws them,
    /// the kept bands and series only, each row's band in `BAND_COLUMN` and
    /// the totals in `BANDS_COLUMN` and `SERIES_COLUMN`.
    pub fn sql(&self, relation: &str, x_ordered: bool) -> String {
        let quote = crate::overview::quote;
        let x = quote(&self.x);
        let value = quote(&self.value_name());
        let measure = match &self.y {
            Some(y) => format!("{}({})::DOUBLE", self.agg.function(), quote(y)),
            None => "count(*)::DOUBLE".to_string(),
        };
        let series = self.series.as_deref().map(quote);
        let split = series
            .as_deref()
            .map(|s| format!("{s}, "))
            .unwrap_or_default();
        // Along categories the largest bands first; along an order, in order.
        let (band_order, cap) = if x_ordered {
            (format!("{x} NULLS LAST"), MAX_ORDERED_BANDS)
        } else {
            (
                format!("__ducklocal_x_total DESC NULLS LAST, {x}"),
                MAX_CATEGORY_BANDS,
            )
        };
        let (series_total, series_rank, series_count, series_kept, series_order) = match &series {
            Some(s) => (
                format!(", sum(abs({value})) OVER (PARTITION BY {s}) AS __ducklocal_s_total"),
                format!(
                    ", dense_rank() OVER (ORDER BY __ducklocal_s_total DESC NULLS LAST, {s}) \
                     AS __ducklocal_s_rank"
                ),
                "max(__ducklocal_s_rank) OVER ()".to_string(),
                format!(" AND __ducklocal_s_rank <= {MAX_SERIES}"),
                ", __ducklocal_s_rank".to_string(),
            ),
            None => (
                String::new(),
                String::new(),
                "1".to_string(),
                String::new(),
                String::new(),
            ),
        };
        let bands = quote(BANDS_COLUMN);
        let series_column = quote(SERIES_COLUMN);
        format!(
            "WITH grouped AS (\n\
             SELECT {x}, {split}{measure} AS {value}\n\
             FROM {relation}\n\
             GROUP BY ALL\n\
             ), totalled AS (\n\
             SELECT *, sum({value}) OVER (PARTITION BY {x}) AS __ducklocal_x_total{series_total}\n\
             FROM grouped\n\
             ), ranked AS (\n\
             SELECT *, dense_rank() OVER (ORDER BY {band_order}) AS __ducklocal_band{series_rank}\n\
             FROM totalled\n\
             ), counted AS (\n\
             SELECT *, max(__ducklocal_band) OVER () AS {bands}, {series_count} AS {series_column}\n\
             FROM ranked\n\
             )\n\
             SELECT {x}, {split}{value}, __ducklocal_band, {bands}, {series_column}\n\
             FROM counted\n\
             WHERE __ducklocal_band <= {cap}{series_kept}\n\
             ORDER BY __ducklocal_band{series_order}"
        )
    }

    /// This pick as `.dash` blocks to paste into a dashboard: a query that
    /// groups `statement` — the result's SQL, one statement — and a plot of
    /// it drawn as `kind`, titled `title`. The query groups plainly, without
    /// the chart's caps: a dashboard plot applies its own.
    pub fn dash(&self, statement: &str, kind: &str, title: &str, ordered: bool) -> String {
        let quote = crate::overview::quote;
        let x = quote(&self.x);
        // A histogram bins the rows themselves: its query hands them over,
        // and its plot names x alone.
        if kind == "histogram" {
            let sql = format!("SELECT {x}\nFROM (\n{statement}\n)\nWHERE {x} IS NOT NULL");
            return format!(
                "{}\n\nplot \"chart\" {{\n  type  = \"histogram\"\n  query = query.chart\n  \
                 x     = {}\n  title = {}\n}}\n",
                query_block(&sql),
                dash_value(&self.x),
                dash_string(title),
            );
        }
        let value = quote(&self.value_name());
        let measure = match &self.y {
            Some(y) => format!("{}({})", self.agg.function(), quote(y)),
            None => "count(*)".to_string(),
        };
        let series = self.series.as_deref().map(quote);
        let split = series
            .as_deref()
            .map(|s| format!("{s}, "))
            .unwrap_or_default();
        let order = match (&series, ordered) {
            (None, false) => format!("{value} DESC"),
            (None, true) => x.clone(),
            (Some(s), _) => format!("{x}, {s}"),
        };
        let sql = format!(
            "SELECT {x}, {split}{measure} AS {value}\n\
             FROM (\n{statement}\n)\n\
             GROUP BY ALL\n\
             ORDER BY {order}"
        );
        let mut text = format!(
            "{query}\n\n\
             plot \"chart\" {{\n  type   = \"{kind}\"\n  query  = query.chart\n  \
             x      = {x}\n  y      = {y}\n",
            query = query_block(&sql),
            x = dash_value(&self.x),
            y = dash_value(&self.value_name()),
        );
        if let Some(series) = &self.series {
            text.push_str(&format!("  series = {}\n", dash_value(series)));
        }
        text.push_str(&format!("  title  = {}\n}}\n", dash_string(title)));
        text
    }

    /// The `.dash` plot block this pick is, drawn as `kind` (a `.dash` plot
    /// type) over the grouped answer.
    pub(crate) fn plot(&self, kind: &str) -> Plot {
        Plot {
            name: "chart".into(),
            kind: kind.into(),
            query: "result".into(),
            x: self.x.clone(),
            y: Some(self.value_name()),
            series: self.series.clone(),
            title: None,
            lat: None,
            lng: None,
            color: None,
            size: None,
            size_scale: None,
            tooltip: None,
            value: None,
            note: None,
            tone: None,
            format: None,
            delta: None,
            delta_format: None,
            down_is_good: false,
            text: None,
            width: None,
            line: 0,
            col: 0,
            span: (0, 0),
            name_span: (0, 0),
        }
    }
}

/// The `.dash` plot type a pick draws as `kind` (the chart tab's pick): a
/// pie needs categories and one series, a heatmap a split for its rows, a
/// map needs no pick at all; Auto
/// draws an area over time and bars otherwise.
pub(crate) fn dash_kind(kind: ChartKind, temporal: bool, split: bool) -> &'static str {
    match kind {
        ChartKind::Bar => "bar",
        ChartKind::Line => "line",
        ChartKind::Area => "area",
        ChartKind::Scatter => "scatter",
        ChartKind::Pie if !temporal && !split => "pie",
        ChartKind::Heatmap if split => "heatmap",
        ChartKind::Histogram if !split => "histogram",
        _ if temporal => "area",
        _ => "bar",
    }
}

/// `sql` as the `.dash` query block `chart`. The heredoc's closing indent
/// comes off every line, so the statement reads back exactly as written.
fn query_block(sql: &str) -> String {
    let mut delimiter = "SQL".to_string();
    while sql.lines().any(|line| line.trim() == delimiter) {
        delimiter.push('_');
    }
    let body: String = sql.lines().map(|line| format!("    {line}\n")).collect();
    format!("query \"chart\" {{\n  sql = <<{delimiter}\n{body}    {delimiter}\n}}")
}

/// A column name as a `.dash` value: bare when it can be, a string when not.
fn dash_value(name: &str) -> String {
    let mut chars = name.chars();
    let bare = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !matches!(name, "true" | "false");
    if bare {
        name.to_string()
    } else {
        dash_string(name)
    }
}

fn dash_string(text: &str) -> String {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t");
    format!("\"{escaped}\"")
}

fn column<'a>(columns: &'a [ColumnMeta], name: &str) -> Option<&'a ColumnMeta> {
    columns.iter().find(|c| c.name == name)
}

#[cfg(test)]
mod tests {
    use super::{Aggregate, Encoding};
    use crate::query::{ColumnKind, ColumnMeta};

    fn column(name: &str, kind: ColumnKind) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            duck_type: format!("{kind:?}"),
            kind,
        }
    }

    #[test]
    fn a_first_pick_starts_from_autos_columns() {
        let columns = [
            column("city", ColumnKind::Text),
            column("note", ColumnKind::Text),
            column("amount", ColumnKind::Numeric),
        ];
        let encoding = Encoding::starting(&columns).unwrap();
        assert_eq!(encoding.x, "city");
        assert_eq!(encoding.y.as_deref(), Some("amount"));
        assert_eq!(encoding.value_name(), "sum(amount)");

        // No number to measure: count the rows.
        let encoding = Encoding::starting(&columns[..2]).unwrap();
        assert_eq!(encoding.value_name(), "count(*)");
    }

    /// `encoding.dash` parsed and validated as a `.dash` file, with its query
    /// run against `statement`: the columns the plot names must come back.
    fn round_trip(encoding: &Encoding, statement: &str, kind: &str) -> Vec<String> {
        let text = encoding.dash(statement, kind, "A \"quoted\" title", false);
        let file = crate::spec::model::parse(&text).unwrap_or_else(|e| panic!("{e:?}\n{text}"));
        let spec = crate::spec::model::validate(&file).unwrap_or_else(|e| panic!("{e:?}\n{text}"));
        assert_eq!(spec.plots[0].kind, kind);
        assert_eq!(spec.plots[0].title.as_deref(), Some("A \"quoted\" title"));
        assert!(spec.queries[0].sql.contains(statement), "{text}");
        let conn =
            duckdb::Connection::open_in_memory_with_flags(crate::db::config().unwrap()).unwrap();
        match crate::query::run_of(&conn, &spec.queries[0].sql).unwrap() {
            crate::query::QueryOutcome::Rows(result) => {
                result.columns.into_iter().map(|c| c.name).collect()
            }
            other => panic!("expected rows, got {other:?}"),
        }
    }

    #[test]
    fn a_pick_copies_as_dash_blocks_that_check_and_run() {
        // A trailing comment, a line that is the heredoc's delimiter, and a
        // string literal spanning lines: the statement must survive as is.
        let statement = "SELECT 'north' AS \"Sales region\", 'a' AS sku, 2.5 AS amount, \
                         '\nSQL\n  x' AS note -- why";
        let encoding = Encoding {
            x: "Sales region".into(),
            y: Some("amount".into()),
            agg: Aggregate::Max,
            series: Some("sku".into()),
        };
        let columns = round_trip(&encoding, statement, "bar");
        assert_eq!(columns, ["Sales region", "sku", "max(amount)"]);
        let columns = round_trip(&encoding, statement, "heatmap");
        assert_eq!(columns, ["Sales region", "sku", "max(amount)"]);

        // A histogram's query hands over the rows of x, to be binned.
        let binned = Encoding {
            x: "amount".into(),
            series: None,
            ..encoding.clone()
        };
        assert_eq!(round_trip(&binned, statement, "histogram"), ["amount"]);

        let counting = Encoding {
            y: None,
            series: None,
            ..encoding
        };
        assert_eq!(
            round_trip(&counting, statement, "pie"),
            ["Sales region", "count(*)"]
        );
    }

    #[test]
    fn a_pick_fits_only_a_result_with_its_columns() {
        let columns = [
            column("city", ColumnKind::Text),
            column("amount", ColumnKind::Numeric),
        ];
        let mut encoding = Encoding {
            x: "city".into(),
            y: Some("amount".into()),
            agg: Aggregate::Avg,
            series: None,
        };
        assert!(encoding.applies_to(&columns));
        encoding.y = Some("city".into());
        assert!(!encoding.applies_to(&columns), "y must be a number");
        encoding.y = None;
        encoding.series = Some("gone".into());
        assert!(!encoding.applies_to(&columns));
        encoding.series = Some("city".into());
        assert!(!encoding.applies_to(&columns), "x cannot split itself");
    }
}
