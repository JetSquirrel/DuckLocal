//! Query execution and result serialization.
//!
//! Blocking functions; call via `smol::unblock` from UI code.

use std::time::Instant;

use anyhow::Result;
use duckdb::arrow::datatypes::DataType;
use duckdb::types::{TimeUnit, Value, ValueRef};
use duckdb::Connection;

/// Stop materializing rows after this many; the UI shows a truncation notice.
pub const MAX_ROWS: usize = 100_000;

/// ...and after this many cells, whichever limit binds first.
///
/// A row cap alone does not bound the work: `PIVOT` over a high-cardinality
/// column returns a column per distinct value, so a result can be hundreds of
/// columns wide. At 100k rows that is tens of millions of cells to format,
/// hold, and hand to the table on its first frame — enough to look like a
/// hang. Budgeting cells keeps wide results as responsive as tall ones.
pub const MAX_CELLS: usize = 2_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColumnKind {
    Numeric,
    Text,
    Boolean,
    Temporal,
    Other,
}

#[derive(Clone, Debug)]
pub struct ColumnMeta {
    pub name: String,
    pub duck_type: String,
    pub kind: ColumnKind,
}

#[derive(Clone, Debug)]
pub struct QueryResult {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<String>>,
    pub elapsed_ms: u128,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub enum QueryOutcome {
    Rows(QueryResult),
    /// DDL/DML that returned no result set; `0` when the count is unknown.
    Affected { count: u64, elapsed_ms: u128 },
}

impl QueryResult {
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

pub fn run(sql: &str) -> Result<QueryOutcome> {
    crate::db::with_connection(|conn| run_of(conn, sql))
}

pub fn run_of(conn: &Connection, sql: &str) -> Result<QueryOutcome> {
    let started = Instant::now();
    let mut stmt = conn.prepare(sql)?;

    if !returns_rows(sql) {
        let count = stmt.execute([])?;
        return Ok(QueryOutcome::Affected {
            count: count as u64,
            elapsed_ms: started.elapsed().as_millis(),
        });
    }

    // Column metadata is only available after execution, so always go
    // through `query` (which executes the statement) and inspect afterwards.
    let mut rows = stmt.query([])?;
    let executed = rows
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Statement handle unavailable"))?;
    let column_count = executed.column_count();

    if column_count == 0 {
        return Ok(QueryOutcome::Affected {
            count: 0,
            elapsed_ms: started.elapsed().as_millis(),
        });
    }

    let mut columns = Vec::with_capacity(column_count);
    for ix in 0..column_count {
        let name = executed
            .column_name(ix)
            .cloned()
            .unwrap_or_else(|_| format!("col{ix}"));
        let column_type = executed.column_type(ix);
        let duck_type = format!("{column_type:?}");
        let kind = classify(&column_type);
        columns.push(ColumnMeta {
            name,
            duck_type,
            kind,
        });
    }

    // Whichever of the two caps binds for this result's shape.
    let row_budget = MAX_ROWS.min(MAX_CELLS / column_count.max(1)).max(1);

    let mut out_rows = Vec::new();
    let mut truncated = false;
    while let Some(row) = rows.next()? {
        if out_rows.len() >= row_budget {
            truncated = true;
            break;
        }
        let mut cells = Vec::with_capacity(column_count);
        for ix in 0..column_count {
            // `get_ref` borrows out of the result vector; `get::<Value>` would
            // allocate an owned value per cell just to throw it away here.
            cells.push(value_ref_to_string(row.get_ref(ix)?));
        }
        out_rows.push(cells);
    }

    Ok(QueryOutcome::Rows(QueryResult {
        columns,
        rows: out_rows,
        elapsed_ms: started.elapsed().as_millis(),
        truncated,
    }))
}

/// `EXPLAIN <sql>` rendered as plain text lines.
pub fn explain_of(conn: &Connection, sql: &str) -> Result<(Vec<String>, u128)> {
    let started = Instant::now();
    let trimmed = sql.trim().trim_end_matches(';');
    let mut stmt = conn.prepare(&format!("EXPLAIN {trimmed}"))?;
    let mut rows = stmt.query([])?;
    let mut lines = Vec::new();
    while let Some(row) = rows.next()? {
        // EXPLAIN returns (explain_key, explain_value); the value holds the plan.
        let text: String = row
            .get::<_, Value>(1)
            .map(|v| value_to_string(&v))
            .unwrap_or_else(|_| row.get::<_, Value>(0).map(|v| value_to_string(&v)).unwrap_or_default());
        lines.push(text);
    }
    Ok((lines, started.elapsed().as_millis()))
}

/// Export a query as CSV or Parquet via DuckDB `COPY`.
pub fn export(sql: &str, path: &str, format: ExportFormat) -> Result<()> {
    crate::db::with_connection(|conn| export_of(conn, sql, path, format))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Parquet,
}

impl ExportFormat {
    fn duckdb_format(self) -> &'static str {
        match self {
            ExportFormat::Csv => "CSV",
            ExportFormat::Parquet => "PARQUET",
        }
    }
}

pub fn export_of(conn: &Connection, sql: &str, path: &str, format: ExportFormat) -> Result<()> {
    let trimmed = sql.trim().trim_end_matches(';');
    let escaped_path = path.replace('\'', "''");
    let expanded = crate::db::expand_tilde(&escaped_path);
    conn.execute_batch(&format!(
        "COPY ({trimmed}) TO '{expanded}' (FORMAT {}, HEADER true)",
        format.duckdb_format()
    ))?;
    Ok(())
}

/// Heuristic: does this statement produce a result set worth displaying?
/// DuckDB returns a `Count` column for DDL/DML run through `query`, so we
/// decide by the leading keyword instead (leading line comments skipped).
fn returns_rows(sql: &str) -> bool {
    let mut rest = sql.trim_start();
    while let Some(after) = rest.strip_prefix("--") {
        rest = after
            .find('\n')
            .map(|ix| after[ix + 1..].trim_start())
            .unwrap_or("");
    }
    let keyword: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_lowercase();
    matches!(
        keyword.as_str(),
        "select" | "with" | "show" | "describe" | "desc" | "explain" | "pragma" | "summarize"
            | "values" | "from" | "table" | "pivot" | "call"
    )
}

/// Keywords whose presence means the statement may have changed what the
/// schema sidebar shows — the table/view list, their columns, or the row
/// estimates that DML moves.
const MUTATING_KEYWORDS: &[&str] = &[
    "insert", "update", "delete", "create", "drop", "alter", "attach", "detach", "truncate",
    "replace", "copy", "install", "load", "set", "reset", "call", "pragma", "use", "comment",
    "grant", "revoke", "begin", "commit", "rollback", "vacuum", "checkpoint", "export", "import",
    "execute", "analyze",
];

/// Whether running `sql` warrants reloading the catalog.
///
/// Word-wise so it sees mutations wherever they sit — inside a CTE, or after a
/// `;` in pasted text — rather than trusting the leading keyword. It errs
/// toward `true`: a table or literal that happens to contain one of these
/// words costs one redundant catalog load, whereas a missed mutation would
/// leave a stale sidebar until the user hits refresh.
pub fn may_change_catalog(sql: &str) -> bool {
    sql.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|word| {
            MUTATING_KEYWORDS
                .iter()
                .any(|keyword| word.eq_ignore_ascii_case(keyword))
        })
}

/// Classify a result column for right-alignment and chart axis selection.
///
/// Matches the Arrow type directly rather than going through
/// `duckdb::types::Type`, whose `From<&DataType>` conversion panics on types
/// it does not model (`INTERVAL` among them) — which would take down the query
/// thread for an otherwise valid query.
fn classify(ty: &DataType) -> ColumnKind {
    match ty {
        DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64
        | DataType::Float16
        | DataType::Float32
        | DataType::Float64
        | DataType::Decimal32(..)
        | DataType::Decimal64(..)
        | DataType::Decimal128(..)
        | DataType::Decimal256(..) => ColumnKind::Numeric,
        DataType::Boolean => ColumnKind::Boolean,
        DataType::Timestamp(..)
        | DataType::Date32
        | DataType::Date64
        | DataType::Time32(..)
        | DataType::Time64(..)
        | DataType::Duration(..)
        | DataType::Interval(..) => ColumnKind::Temporal,
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => ColumnKind::Text,
        _ => ColumnKind::Other,
    }
}

/// Render a borrowed cell value. Scalars format straight from the borrow so
/// materializing a result set costs one allocation per cell; container types
/// fall through to the owning path, keeping nested formatting in one place.
pub fn value_ref_to_string(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Null => "NULL".to_string(),
        ValueRef::Boolean(v) => v.to_string(),
        ValueRef::TinyInt(v) => v.to_string(),
        ValueRef::SmallInt(v) => v.to_string(),
        ValueRef::Int(v) => v.to_string(),
        ValueRef::BigInt(v) => v.to_string(),
        ValueRef::HugeInt(v) => v.to_string(),
        ValueRef::UHugeInt(v) => v.to_string(),
        ValueRef::UTinyInt(v) => v.to_string(),
        ValueRef::USmallInt(v) => v.to_string(),
        ValueRef::UInt(v) => v.to_string(),
        ValueRef::UBigInt(v) => v.to_string(),
        ValueRef::Float(v) => format_float(v as f64),
        ValueRef::Double(v) => format_float(v),
        ValueRef::Decimal(v) => v.to_string(),
        ValueRef::Text(v) => String::from_utf8_lossy(v).into_owned(),
        ValueRef::Blob(v) => format!("<blob {} bytes>", v.len()),
        ValueRef::Geometry(v) => format!("<geometry {} bytes>", v.len()),
        ValueRef::Date32(days) => format_date(days),
        ValueRef::Time64(unit, v) => format_time(unit, v),
        ValueRef::Timestamp(unit, v) => format_timestamp(unit, v),
        ValueRef::Interval {
            months,
            days,
            nanos,
        } => format_interval(months, days, nanos),
        other => value_to_string(&Value::from(other)),
    }
}

pub fn value_to_string(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::TinyInt(v) => v.to_string(),
        Value::SmallInt(v) => v.to_string(),
        Value::Int(v) => v.to_string(),
        Value::BigInt(v) => v.to_string(),
        Value::HugeInt(v) => v.to_string(),
        Value::UHugeInt(v) => v.to_string(),
        Value::UTinyInt(v) => v.to_string(),
        Value::USmallInt(v) => v.to_string(),
        Value::UInt(v) => v.to_string(),
        Value::UBigInt(v) => v.to_string(),
        Value::Float(v) => format_float(*v as f64),
        Value::Double(v) => format_float(*v),
        Value::Decimal(v) => v.to_string(),
        Value::Text(v) => v.clone(),
        Value::Blob(v) => format!("<blob {} bytes>", v.len()),
        Value::Date32(days) => format_date(*days),
        Value::Time64(unit, v) => format_time(*unit, *v),
        Value::Timestamp(unit, v) => format_timestamp(*unit, *v),
        Value::Interval {
            months,
            days,
            nanos,
        } => format_interval(*months, *days, *nanos),
        Value::List(values) => {
            let inner: Vec<String> = values.iter().map(value_to_string).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Struct(values) => {
            let inner: Vec<String> = values
                .iter()
                .map(|(k, v)| format!("{k}: {}", value_to_string(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        Value::Map(values) => {
            let inner: Vec<String> = values
                .iter()
                .map(|(k, v)| format!("{}: {}", value_to_string(k), value_to_string(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        Value::Enum(v) => v.clone(),
        Value::Array(values) => {
            let inner: Vec<String> = values.iter().map(value_to_string).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Union(v) => value_to_string(v),
        Value::Geometry(v) => format!("<geometry {} bytes>", v.len()),
        other => format!("{other:?}"),
    }
}

fn format_date(days: i32) -> String {
    chrono::NaiveDate::from_num_days_from_ce_opt(days + 719_163)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| format!("{days} days"))
}

fn format_float(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

fn format_timestamp(unit: TimeUnit, v: i64) -> String {
    let micros = unit.to_micros(v);
    chrono::DateTime::from_timestamp_micros(micros)
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| v.to_string())
}

fn format_time(unit: TimeUnit, v: i64) -> String {
    let micros = unit.to_micros(v);
    let secs = micros.div_euclid(1_000_000);
    let h = secs.div_euclid(3600);
    let m = secs.div_euclid(60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

fn format_interval(months: i32, days: i32, nanos: i64) -> String {
    let mut parts = Vec::new();
    if months != 0 {
        parts.push(format!("{months} months"));
    }
    if days != 0 {
        parts.push(format!("{days} days"));
    }
    if nanos != 0 {
        parts.push(format!("{} ns", nanos));
    }
    if parts.is_empty() {
        "0".to_string()
    } else {
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn select_returns_typed_columns() {
        let conn = mem();
        conn.execute_batch("CREATE TABLE t(a INTEGER, b VARCHAR, c DOUBLE); INSERT INTO t VALUES (1, 'x', 2.5);")
            .unwrap();
        let outcome = run_of(&conn, "SELECT * FROM t").unwrap();
        let QueryOutcome::Rows(result) = outcome else {
            panic!("expected rows");
        };
        assert_eq!(result.columns.len(), 3);
        assert_eq!(result.columns[0].kind, ColumnKind::Numeric);
        assert_eq!(result.columns[1].kind, ColumnKind::Text);
        assert_eq!(result.rows, vec![vec!["1", "x", "2.5"]]);
    }

    #[test]
    fn dml_returns_affected_count() {
        let conn = mem();
        let outcome = run_of(&conn, "CREATE TABLE t(a INTEGER)").unwrap();
        assert!(matches!(outcome, QueryOutcome::Affected { .. }));
    }

    #[test]
    fn explain_returns_plan_text() {
        let conn = mem();
        let (lines, _) = explain_of(&conn, "SELECT 42").unwrap();
        assert!(!lines.is_empty());
    }

    #[test]
    fn export_csv_writes_file() {
        let conn = mem();
        let dir = std::env::temp_dir().join("ducklocal_test_export.csv");
        export_of(&conn, "SELECT 1 AS a, 'x' AS b", dir.to_str().unwrap(), ExportFormat::Csv).unwrap();
        let content = std::fs::read_to_string(&dir).unwrap();
        assert!(content.contains("a,b"));
        std::fs::remove_file(&dir).ok();
    }

    /// `run_of` formats cells from a borrowed `ValueRef`; this pins the output
    /// for the types where that path differs from the owning one, including the
    /// container types that fall back to it.
    #[test]
    fn wide_results_are_bounded_by_cells_not_just_rows() {
        let conn = mem();
        // 400 columns: the row cap alone would allow 40M cells.
        let selects: Vec<String> = (0..400).map(|ix| format!("i + {ix} AS c{ix}")).collect();
        let sql = format!(
            "SELECT {} FROM range(200000) t(i)",
            selects.join(", ")
        );
        let QueryOutcome::Rows(result) = run_of(&conn, &sql).unwrap() else {
            panic!("expected rows");
        };
        assert_eq!(result.columns.len(), 400);
        assert!(result.truncated);
        let cells = result.rows.len() * result.columns.len();
        assert!(
            cells <= MAX_CELLS,
            "{cells} cells exceeds the budget of {MAX_CELLS}"
        );
        // The budget should be spent, not left mostly unused.
        assert!(cells > MAX_CELLS / 2, "only {cells} cells materialized");

        // A narrow result is still governed by the row cap.
        let QueryOutcome::Rows(narrow) = run_of(&conn, "SELECT i FROM range(150000) t(i)").unwrap()
        else {
            panic!("expected rows");
        };
        assert_eq!(narrow.rows.len(), MAX_ROWS);
        assert!(narrow.truncated);
    }

    #[test]
    fn borrowed_and_owned_cell_formatting_agree() {
        let conn = mem();
        let outcome = run_of(
            &conn,
            "SELECT NULL AS a,
                    true AS b,
                    CAST(-7 AS TINYINT) AS c,
                    CAST(2 AS UBIGINT) AS d,
                    CAST(2.5 AS DOUBLE) AS e,
                    CAST(3 AS DOUBLE) AS f,
                    CAST(1.25 AS DECIMAL(5,2)) AS g,
                    'héllo' AS h,
                    DATE '2024-10-04' AS i,
                    TIMESTAMP '2024-10-04 07:08:09' AS j,
                    TIME '07:08:09' AS k,
                    INTERVAL 3 DAY AS l,
                    [1, 2] AS m,
                    {'x': 1} AS n,
                    MAP {'x': 1} AS o,
                    'abc'::BLOB AS p",
        )
        .unwrap();
        let QueryOutcome::Rows(result) = outcome else {
            panic!("expected rows");
        };
        assert_eq!(
            result.rows[0],
            [
                "NULL",
                "true",
                "-7",
                "2",
                "2.5",
                "3.0",
                "1.25",
                "héllo",
                "2024-10-04",
                "2024-10-04 07:08:09",
                "07:08:09",
                "3 days",
                "[1, 2]",
                "{x: 1}",
                "{x: 1}",
                "<blob 3 bytes>",
            ]
        );
        // Classifying an INTERVAL column used to panic before it ever got to
        // formatting, taking the query thread with it.
        assert_eq!(result.columns[11].kind, ColumnKind::Temporal);
        assert_eq!(result.columns[8].kind, ColumnKind::Temporal);
        assert_eq!(result.columns[6].kind, ColumnKind::Numeric);
        assert_eq!(result.columns[7].kind, ColumnKind::Text);
        assert_eq!(result.columns[1].kind, ColumnKind::Boolean);
    }

    #[test]
    fn catalog_reload_is_skipped_for_reads_only() {
        assert!(!may_change_catalog("SELECT * FROM orders WHERE id = 1"));
        assert!(!may_change_catalog(
            "WITH daily AS (SELECT d, sum(x) FROM t GROUP BY d) SELECT * FROM daily"
        ));
        assert!(!may_change_catalog("SUMMARIZE orders"));
        assert!(!may_change_catalog("DESCRIBE orders"));

        assert!(may_change_catalog("CREATE TABLE t(a INT)"));
        assert!(may_change_catalog("insert into t values (1)"));
        assert!(may_change_catalog("DELETE FROM t"));
        assert!(may_change_catalog("ALTER TABLE t ADD COLUMN b INT"));
        // Mutations that a leading-keyword check would miss.
        assert!(may_change_catalog("SELECT 1; DROP TABLE t"));
        assert!(may_change_catalog(
            "WITH x AS (SELECT 1) INSERT INTO t SELECT * FROM x"
        ));
    }

    #[test]
    fn values_format_as_expected() {
        assert_eq!(value_to_string(&Value::Null), "NULL");
        assert_eq!(value_to_string(&Value::Date32(20_000)), "2024-10-04");
        assert_eq!(
            value_to_string(&Value::Timestamp(TimeUnit::Microsecond, 0)),
            "1970-01-01 00:00:00"
        );
    }
}
