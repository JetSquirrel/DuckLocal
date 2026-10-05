//! A column overview: for every column of a table, a file or a query, how
//! full it is, how many values it takes, its range, and what its values look
//! like — a histogram for numbers and times, the most common values for the
//! rest.
//!
//! Two scans, whatever the width: DuckDB's `SUMMARIZE` for the statistics,
//! then one `SELECT` that computes every column's distribution side by side.
//! The second needs the first — a histogram's bins come from the column's
//! range — which is why it is not one.
//!
//! Blocking functions; call via `smol::unblock` from UI code.

use std::time::Instant;

use anyhow::{anyhow, Result};
use duckdb::Connection;

/// How many bars a histogram gets. Few enough to read as a shape at a glance
/// in a narrow cell.
const BINS: usize = 12;
/// How many common values a text column lists.
const TOP_VALUES: usize = 5;
/// Up to this many distinct values, a column's common values are counted
/// exactly. Above it the counting map would grow with the column, so the
/// values are only estimated (`approx_top_k`) and carry no count.
const EXACT_TOP_LIMIT: i64 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Numeric,
    Temporal,
    Boolean,
    Text,
    /// `LIST`, `STRUCT`, `MAP`, `UNION`: counted, not charted.
    Nested,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Distribution {
    None,
    /// Equal-width bins in column order, each `(upper bound, rows)`.
    Bins(Vec<(String, u64)>),
    /// The most common values, most common first. Counts are `None` when the
    /// column has too many distinct values to count them exactly.
    Top(Vec<(String, Option<u64>)>),
}

#[derive(Clone, Debug)]
pub struct ColumnOverview {
    pub name: String,
    pub data_type: String,
    pub shape: Shape,
    /// Share of rows that are NULL, 0–1.
    pub null_fraction: f64,
    /// DuckDB's estimate of the distinct values.
    pub approx_unique: i64,
    pub min: Option<String>,
    pub max: Option<String>,
    /// The median, for numbers and times.
    pub median: Option<String>,
    pub distribution: Distribution,
}

#[derive(Clone, Debug)]
pub struct Overview {
    /// What was summarized, as the panel names it.
    pub label: String,
    pub row_count: i64,
    pub columns: Vec<ColumnOverview>,
    pub elapsed_ms: u128,
}

/// `relation` is anything a `FROM` takes: a quoted table name, a reader call,
/// or a parenthesized query.
pub fn overview_of(conn: &Connection, relation: &str, label: &str) -> Result<Overview> {
    let started = Instant::now();
    let mut stmt = conn.prepare(&format!("SUMMARIZE SELECT * FROM {relation}"))?;
    let mut columns: Vec<ColumnOverview> = Vec::new();
    let mut row_count = 0;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get("column_name")?;
        let data_type: String = row.get("column_type")?;
        row_count = row.get::<_, Option<i64>>("count")?.unwrap_or(0);
        let null_percentage: Option<f64> = row
            .get::<_, Option<duckdb::types::Value>>("null_percentage")?
            .and_then(|v| crate::query::value_to_string(&v).parse().ok());
        let shape = shape_of(&data_type);
        let has_median = matches!(shape, Shape::Numeric | Shape::Temporal);
        columns.push(ColumnOverview {
            shape,
            null_fraction: null_percentage.unwrap_or(0.0) / 100.0,
            approx_unique: row.get::<_, Option<i64>>("approx_unique")?.unwrap_or(0),
            min: row.get("min")?,
            max: row.get("max")?,
            median: if has_median { row.get("q50")? } else { None },
            name,
            data_type,
            distribution: Distribution::None,
        });
    }
    if columns.is_empty() {
        return Err(anyhow!("{label} has no columns"));
    }

    let expressions: Vec<Option<String>> = columns.iter().map(distribution_sql).collect();
    let wanted: Vec<(usize, &String)> = expressions
        .iter()
        .enumerate()
        .filter_map(|(ix, sql)| sql.as_ref().map(|sql| (ix, sql)))
        .collect();
    if !wanted.is_empty() && row_count > 0 {
        let select = wanted
            .iter()
            .map(|(_, sql)| sql.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = conn.prepare(&format!("SELECT {select} FROM {relation}"))?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            for (slot, (ix, _)) in wanted.iter().enumerate() {
                let json: Option<String> = row.get(slot)?;
                let column = &mut columns[*ix];
                column.distribution = json
                    .as_deref()
                    .map(|json| parse_distribution(column, json))
                    .unwrap_or(Distribution::None);
            }
        }
    }

    Ok(Overview {
        label: label.to_string(),
        row_count,
        columns,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

fn shape_of(data_type: &str) -> Shape {
    let upper = data_type.to_uppercase();
    if upper.contains('[')
        || ["STRUCT", "MAP", "UNION", "LIST"]
            .iter()
            .any(|p| upper.starts_with(p))
    {
        return Shape::Nested;
    }
    if upper == "BOOLEAN" {
        return Shape::Boolean;
    }
    if upper == "DATE" || upper.starts_with("TIMESTAMP") {
        return Shape::Temporal;
    }
    let numeric = [
        "TINYINT",
        "SMALLINT",
        "INTEGER",
        "BIGINT",
        "HUGEINT",
        "UTINYINT",
        "USMALLINT",
        "UINTEGER",
        "UBIGINT",
        "UHUGEINT",
        "FLOAT",
        "REAL",
        "DOUBLE",
        "DECIMAL",
    ];
    if numeric.iter().any(|t| upper.starts_with(t)) {
        return Shape::Numeric;
    }
    Shape::Text
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// The SQL that yields one column's distribution as a JSON string, or
/// `None` when there is nothing to chart.
fn distribution_sql(column: &ColumnOverview) -> Option<String> {
    let name = quote(&column.name);
    // Entries sorted by count, most first, as `[{n, v}, …]`. A struct sorts
    // by its first field, so the count leads.
    let ranked = |histogram: String| {
        format!(
            "CAST(to_json(list_reverse_sort(list_transform(map_entries({histogram}), \
             e -> {{'n': e.value, 'v': CAST(e.key AS VARCHAR)}}))[1:{TOP_VALUES}]) AS VARCHAR)"
        )
    };
    match column.shape {
        Shape::Nested => None,
        Shape::Numeric | Shape::Temporal => {
            let (min, max) = (column.min.as_deref()?, column.max.as_deref()?);
            if min == max {
                // One value: a single bar says less than naming it.
                return Some(ranked(format!("histogram({name})")));
            }
            let cast = match column.shape {
                Shape::Numeric => "DOUBLE",
                _ if column.data_type.eq_ignore_ascii_case("DATE") => "DATE",
                _ => "TIMESTAMP",
            };
            Some(format!(
                "CAST(to_json(list_transform(map_entries(histogram(CAST({name} AS {cast}), \
                 equi_width_bins(CAST({} AS {cast}), CAST({} AS {cast}), {BINS}, true))), \
                 e -> {{'v': CAST(e.key AS VARCHAR), 'n': e.value}})) AS VARCHAR)",
                literal(min),
                literal(max),
            ))
        }
        Shape::Boolean | Shape::Text if column.approx_unique <= EXACT_TOP_LIMIT => {
            Some(ranked(format!("histogram({name})")))
        }
        Shape::Boolean | Shape::Text => Some(format!(
            "CAST(to_json(list_transform(approx_top_k({name}, {TOP_VALUES}), \
             v -> {{'v': CAST(v AS VARCHAR)}})) AS VARCHAR)"
        )),
    }
}

fn parse_distribution(column: &ColumnOverview, json: &str) -> Distribution {
    let Ok(serde_json::Value::Array(entries)) = serde_json::from_str(json) else {
        return Distribution::None;
    };
    let pairs: Vec<(String, Option<u64>)> = entries
        .iter()
        .filter_map(|entry| {
            let value = match entry.get("v")? {
                serde_json::Value::Null => "NULL".to_string(),
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            Some((value, entry.get("n").and_then(|n| n.as_u64())))
        })
        .collect();
    let binned =
        matches!(column.shape, Shape::Numeric | Shape::Temporal) && column.min != column.max;
    if binned {
        Distribution::Bins(
            pairs
                .into_iter()
                .map(|(bound, n)| (bound, n.unwrap_or(0)))
                .collect(),
        )
    } else {
        Distribution::Top(pairs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE t AS SELECT
                range AS id,
                CASE WHEN range % 10 = 0 THEN NULL ELSE range * 1.5 END AS amount,
                DATE '2026-01-01' + CAST(range % 90 AS INTEGER) AS day,
                ['web', 'shop', 'app', 'shop'][range % 4 + 1] AS channel,
                range % 3 = 0 AS flag,
                [range, range] AS pair,
                'same' AS constant
             FROM range(1000)",
        )
        .unwrap();
        conn
    }

    #[test]
    fn summarizes_every_column_in_two_scans() {
        let overview = overview_of(&conn(), "t", "t").unwrap();
        assert_eq!(overview.row_count, 1000);
        let names: Vec<&str> = overview.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["id", "amount", "day", "channel", "flag", "pair", "constant"]
        );
        let col = |name: &str| overview.columns.iter().find(|c| c.name == name).unwrap();

        let amount = col("amount");
        assert_eq!(amount.shape, Shape::Numeric);
        assert!((amount.null_fraction - 0.1).abs() < 1e-9);
        let Distribution::Bins(bins) = &amount.distribution else {
            panic!("{:?}", amount.distribution);
        };
        // `nice` bounds round the range out, so the count is near BINS,
        // not exactly it.
        assert!(bins.len() > 1 && bins.len() <= BINS * 2, "{bins:?}");
        // Every non-NULL row lands in a bin.
        assert_eq!(bins.iter().map(|(_, n)| n).sum::<u64>(), 900);

        assert!(matches!(col("day").distribution, Distribution::Bins(_)));
        assert!(col("day").median.is_some());

        let Distribution::Top(top) = &col("channel").distribution else {
            panic!();
        };
        assert_eq!(top[0], ("shop".to_string(), Some(500)));
        assert_eq!(top.len(), 3);

        let Distribution::Top(flags) = &col("flag").distribution else {
            panic!();
        };
        assert_eq!(flags[0], ("false".to_string(), Some(666)));

        assert_eq!(col("pair").shape, Shape::Nested);
        assert_eq!(col("pair").distribution, Distribution::None);
        assert_eq!(
            col("constant").distribution,
            Distribution::Top(vec![("same".to_string(), Some(1000))])
        );
    }

    #[test]
    fn a_query_and_an_empty_relation_work_too() {
        let conn = conn();
        let overview = overview_of(&conn, "(SELECT channel FROM t WHERE flag)", "query").unwrap();
        assert_eq!(overview.row_count, 334);
        let empty = overview_of(&conn, "(SELECT * FROM t WHERE false)", "empty").unwrap();
        assert_eq!(empty.row_count, 0);
        assert!(empty
            .columns
            .iter()
            .all(|c| c.distribution == Distribution::None));
    }

    #[test]
    fn many_distinct_values_are_estimated_without_counts() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE ids AS SELECT 'user-' || range AS id FROM range({})",
            EXACT_TOP_LIMIT * 3
        ))
        .unwrap();
        let overview = overview_of(&conn, "ids", "ids").unwrap();
        let Distribution::Top(top) = &overview.columns[0].distribution else {
            panic!();
        };
        assert!(!top.is_empty());
        assert!(top.iter().all(|(_, n)| n.is_none()));
    }
}
