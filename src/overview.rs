//! A column overview: for every column of a table, a file or a query, how
//! full it is, how many values it takes, its range, and what its values look
//! like — a histogram for numbers and times, the most common values for the
//! rest.
//!
//! Lazy, so a wide relation costs what is looked at rather than its width:
//! [`shell_of`] names the columns and counts the rows without profiling any
//! of them, and [`profile_columns`] profiles the few that are asked for in
//! two scans — DuckDB's `SUMMARIZE` over just those columns for the
//! statistics, then one `SELECT` that computes their distributions side by
//! side. The second needs the first — a histogram's bins come from the
//! column's range — which is why it is not one. Callers split a long list of
//! columns with [`batch_size`], the bound `ducklocal profile` uses, and can
//! stop between or during batches with a [`Cancel`].
//!
//! Blocking functions; call via `smol::unblock` from UI code.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use duckdb::{Connection, InterruptHandle};

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

/// What an overview knows before it profiles a column: every column's name
/// and type, in the relation's order, and how many rows it has. A `DESCRIBE`
/// and a `count(*)`; neither computes a statistic of any column.
#[derive(Clone, Debug, PartialEq)]
pub struct Shell {
    pub columns: Vec<(String, String)>,
    pub row_count: i64,
}

/// `relation` is anything a `FROM` takes: a quoted table name, a reader call,
/// or a parenthesized query.
pub fn shell_of(conn: &Connection, relation: &str, cancel: &Cancel) -> Result<Shell> {
    cancel.run(conn, || {
        let columns = crate::profile::describe(conn, relation)?;
        if columns.is_empty() {
            return Err(anyhow!("the relation has no columns"));
        }
        let row_count: i64 =
            conn.query_row(&format!("SELECT count(*) FROM {relation}"), [], |row| {
                row.get(0)
            })?;
        Ok(Shell { columns, row_count })
    })
}

/// How many columns of a `row_count`-row relation one [`profile_columns`]
/// call should take: the bound `ducklocal profile` sizes its batches by, so
/// the state a batch holds follows the cells it reads, not the relation's
/// width.
pub fn batch_size(row_count: i64) -> usize {
    crate::profile::batch_size(row_count, 1)
}

/// Every column of `relation`, profiled batch by batch: what the lazy
/// overview ends with once every column has been asked for.
#[cfg(test)]
pub fn overview_of(conn: &Connection, relation: &str) -> Result<(Shell, Vec<ColumnOverview>)> {
    let cancel = Cancel::default();
    let shell = shell_of(conn, relation, &cancel)?;
    let mut columns = Vec::with_capacity(shell.columns.len());
    for batch in shell.columns.chunks(batch_size(shell.row_count)) {
        columns.extend(profile_columns(
            conn,
            relation,
            batch,
            shell.row_count,
            &cancel,
        )?);
    }
    Ok((shell, columns))
}

/// Profile `columns` — some of `relation`'s, as [`shell_of`] named them — in
/// two scans that read only those columns. `row_count` is the shell's: an
/// empty relation has no distributions to compute.
pub fn profile_columns(
    conn: &Connection,
    relation: &str,
    columns: &[(String, String)],
    row_count: i64,
    cancel: &Cancel,
) -> Result<Vec<ColumnOverview>> {
    cancel.run(conn, || summarize(conn, relation, columns, row_count))
}

fn summarize(
    conn: &Connection,
    relation: &str,
    wanted: &[(String, String)],
    row_count: i64,
) -> Result<Vec<ColumnOverview>> {
    let projection = wanted
        .iter()
        .map(|(name, _)| quote(name))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn.prepare(&format!("SUMMARIZE SELECT {projection} FROM {relation}"))?;
    let mut columns: Vec<ColumnOverview> = Vec::with_capacity(wanted.len());
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get("column_name")?;
        let data_type: String = row.get("column_type")?;
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
    if columns.len() != wanted.len() {
        return Err(anyhow!(
            "SUMMARIZE described {} of {} columns",
            columns.len(),
            wanted.len()
        ));
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

    Ok(columns)
}

/// What [`Cancel::run`] fails with once its work has been cancelled, so a
/// caller can tell a stop it asked for from a failure.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// A way to stop profiling from another thread: between statements, by
/// refusing to start the next one, and during one, by interrupting the
/// connection it runs on.
#[derive(Default)]
pub struct Cancel {
    cancelled: AtomicBool,
    /// The connection [`Cancel::run`] is running statements on, while it is.
    running: Mutex<Option<Arc<InterruptHandle>>>,
}

impl Cancel {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        // Interrupt under the lock `run` clears the handle under: while it is
        // set, `run`'s caller still holds the connection, so the statement
        // interrupted can only be this work's. The window's connection is
        // shared, and an interrupt that landed after the profile let go of it
        // would stop the user's next query instead.
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = running.as_ref() {
            handle.interrupt();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Run `work` — statements on `conn`, which the caller holds — so that
    /// [`Cancel::cancel`] can interrupt them. Fails with [`Cancelled`] once
    /// cancelled, whatever `work` did.
    pub fn run<T>(&self, conn: &Connection, work: impl FnOnce() -> Result<T>) -> Result<T> {
        if self.is_cancelled() {
            return Err(Cancelled.into());
        }
        *self.running.lock().unwrap_or_else(|e| e.into_inner()) = Some(conn.interrupt_handle());
        let done = work();
        *self.running.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if self.is_cancelled() {
            return Err(Cancelled.into());
        }
        done
    }
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

pub(crate) fn quote(name: &str) -> String {
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
             lambda e: {{'n': e.value, 'v': CAST(e.key AS VARCHAR)}}))[1:{TOP_VALUES}]) AS VARCHAR)"
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
                 lambda e: {{'v': CAST(e.key AS VARCHAR), 'n': e.value}})) AS VARCHAR)",
                literal(min),
                literal(max),
            ))
        }
        Shape::Boolean | Shape::Text if column.approx_unique <= EXACT_TOP_LIMIT => {
            Some(ranked(format!("histogram({name})")))
        }
        Shape::Boolean | Shape::Text => Some(format!(
            "CAST(to_json(list_transform(approx_top_k({name}, {TOP_VALUES}), \
             lambda v: {{'v': CAST(v AS VARCHAR)}})) AS VARCHAR)"
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
    fn summarizes_every_column() {
        let (shell, columns) = overview_of(&conn(), "t").unwrap();
        assert_eq!(shell.row_count, 1000);
        let names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["id", "amount", "day", "channel", "flag", "pair", "constant"]
        );
        let col = |name: &str| columns.iter().find(|c| c.name == name).unwrap();

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
        let (overview, _) = overview_of(&conn, "(SELECT channel FROM t WHERE flag)").unwrap();
        assert_eq!(overview.row_count, 334);
        let (empty, columns) = overview_of(&conn, "(SELECT * FROM t WHERE false)").unwrap();
        assert_eq!(empty.row_count, 0);
        assert!(columns.iter().all(|c| c.distribution == Distribution::None));
    }

    #[test]
    fn the_shell_names_every_column_and_one_column_profiles_alone() {
        let conn = conn();
        let cancel = Cancel::default();
        let shell = shell_of(&conn, "t", &cancel).unwrap();
        assert_eq!(shell.row_count, 1000);
        assert_eq!(shell.columns.len(), 7);
        assert_eq!(shell.columns[3], ("channel".into(), "VARCHAR".into()));

        // Asked for out of order and alone, a column profiles as it does
        // among all of them.
        let wanted = [shell.columns[3].clone(), shell.columns[1].clone()];
        let some = profile_columns(&conn, "t", &wanted, shell.row_count, &cancel).unwrap();
        let (_, all) = overview_of(&conn, "t").unwrap();
        assert_eq!(some.len(), 2);
        for column in &some {
            let same = all.iter().find(|c| c.name == column.name).unwrap();
            assert_eq!(column.distribution, same.distribution, "{}", column.name);
            assert_eq!(column.min, same.min);
            assert_eq!(column.null_fraction, same.null_fraction);
        }
        assert_eq!(some[0].name, "channel");
        assert_eq!(some[1].name, "amount");
    }

    #[test]
    fn a_name_that_needs_quoting_profiles() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(r#"CREATE TABLE q AS SELECT 1 AS "a ""b"" c", 2 AS "x""#)
            .unwrap();
        let (shell, columns) = overview_of(&conn, "q").unwrap();
        assert_eq!(shell.columns[0].0, r#"a "b" c"#);
        assert_eq!(columns[0].name, r#"a "b" c"#);
    }

    #[test]
    fn wide_relations_profile_in_batches() {
        let conn = Connection::open_in_memory().unwrap();
        let select = (0..150)
            .map(|i| format!("range + {i} AS c{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        conn.execute_batch(&format!("CREATE TABLE w AS SELECT {select} FROM range(10)"))
            .unwrap();
        assert!(batch_size(10) < 150);
        let (shell, columns) = overview_of(&conn, "w").unwrap();
        assert_eq!(shell.columns.len(), 150);
        let names: Vec<_> = columns.iter().map(|c| c.name.clone()).collect();
        let expected: Vec<_> = (0..150).map(|i| format!("c{i}")).collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn cancelled_work_does_not_start() {
        let conn = conn();
        let cancel = Cancel::default();
        cancel.cancel();
        let error = shell_of(&conn, "t", &cancel).unwrap_err();
        assert!(error.is::<Cancelled>(), "{error}");
    }

    #[test]
    fn cancel_interrupts_a_running_statement_and_only_that() {
        let conn = Connection::open_in_memory().unwrap();
        // Long enough to still be running when the cancel lands.
        conn.execute_batch(
            "CREATE VIEW slow AS SELECT a.range AS x FROM range(100000) a, range(100000) b",
        )
        .unwrap();
        let cancel = Arc::new(Cancel::default());
        let canceller = {
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(200));
                cancel.cancel();
            })
        };
        let started = std::time::Instant::now();
        let error = shell_of(&conn, "slow", &cancel).unwrap_err();
        canceller.join().unwrap();
        assert!(error.is::<Cancelled>(), "{error}");
        assert!(started.elapsed() < std::time::Duration::from_secs(20));

        // The connection is left as it was: the next statement, which no
        // cancel covers, runs.
        let n: i64 = conn.query_row("SELECT 41 + 1", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 42);
        // And a cancel after the work is done touches nothing.
        cancel.cancel();
        let n: i64 = conn.query_row("SELECT 7", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 7);
    }

    #[test]
    fn many_distinct_values_are_estimated_without_counts() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE ids AS SELECT 'user-' || range AS id FROM range({})",
            EXACT_TOP_LIMIT * 3
        ))
        .unwrap();
        let (_, columns) = overview_of(&conn, "ids").unwrap();
        let Distribution::Top(top) = &columns[0].distribution else {
            panic!();
        };
        assert!(!top.is_empty());
        assert!(top.iter().all(|(_, n)| n.is_none()));
    }
}
