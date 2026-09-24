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

/// Field visibility is crate-wide because the structured encoding below is
/// what any second reader of a result must reuse: the analysis app's
/// `query()` host function hands these same values to JavaScript, and a
/// second encoder is how a big integer quietly becomes a float.
#[derive(serde::Serialize, Clone, Debug)]
pub struct CliColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub arrow_type: String,
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct CliResult {
    pub columns: Vec<CliColumn>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: usize,
    pub truncated: bool,
    pub elapsed_ms: u128,
}

pub fn run_cli_of(conn: &Connection, sql: &str, limit: usize) -> Result<CliResult> {
    let started = Instant::now();
    let mut stmt = conn.prepare(sql)?;
    let mut rows = query_streaming(&mut stmt)?;
    let executed = rows
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Statement handle unavailable"))?;
    let column_count = executed.column_count();
    let mut columns = Vec::with_capacity(column_count);
    for ix in 0..column_count {
        let ty = executed.column_type(ix);
        columns.push(CliColumn {
            name: executed.column_name(ix)?.clone(),
            arrow_type: format!("{ty:?}"),
        });
    }
    let row_budget = limit.min(MAX_CELLS / column_count.max(1));
    let mut out = Vec::new();
    let mut truncated = false;
    while let Some(row) = rows.next()? {
        if out.len() >= row_budget {
            truncated = true;
            break;
        }
        let mut cells = Vec::with_capacity(column_count);
        for ix in 0..column_count {
            cells.push(cli_value(row.get_ref(ix)?)?);
        }
        out.push(cells);
    }
    Ok(CliResult {
        columns,
        row_count: out.len(),
        rows: out,
        truncated,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

fn cli_integer(value: i128) -> serde_json::Value {
    if (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&value) {
        serde_json::json!(value as i64)
    } else {
        serde_json::json!({"encoding": "integer", "value": value.to_string()})
    }
}

fn cli_value(value: ValueRef<'_>) -> Result<serde_json::Value> {
    use serde_json::json;
    Ok(match value {
        ValueRef::Null => serde_json::Value::Null,
        ValueRef::Boolean(v) => json!(v),
        ValueRef::TinyInt(v) => cli_integer(v.into()),
        ValueRef::SmallInt(v) => cli_integer(v.into()),
        ValueRef::Int(v) => cli_integer(v.into()),
        ValueRef::BigInt(v) => cli_integer(v.into()),
        ValueRef::HugeInt(v) => cli_integer(v),
        ValueRef::UTinyInt(v) => cli_integer(v.into()),
        ValueRef::USmallInt(v) => cli_integer(v.into()),
        ValueRef::UInt(v) => cli_integer(v.into()),
        ValueRef::UBigInt(v) => cli_integer(v.into()),
        ValueRef::UHugeInt(v) => {
            if v <= 9_007_199_254_740_991 {
                json!(v as u64)
            } else {
                json!({"encoding": "integer", "value": v.to_string()})
            }
        }
        ValueRef::Float(v) => cli_float(v.into()),
        ValueRef::Double(v) => cli_float(v),
        ValueRef::Decimal(v) => json!({"encoding": "decimal", "value": v.to_string()}),
        ValueRef::Text(v) => json!(std::str::from_utf8(v)?),
        ValueRef::Enum(..) => json!(value.as_str().map_err(|e| anyhow::anyhow!("{e:?}"))?),
        ValueRef::Blob(v) | ValueRef::Geometry(v) => {
            let hex: String = v.iter().map(|byte| format!("{byte:02x}")).collect();
            json!({"encoding": "hex", "value": hex})
        }
        ValueRef::Date32(v) => json!({"encoding": "date", "unit": "Day", "value": v.to_string()}),
        ValueRef::Timestamp(unit, v) => {
            json!({"encoding": "timestamp", "unit": format!("{unit:?}"), "value": v.to_string()})
        }
        ValueRef::Time64(unit, v) => {
            json!({"encoding": "time", "unit": format!("{unit:?}"), "value": v.to_string()})
        }
        ValueRef::Interval {
            months,
            days,
            nanos,
        } => {
            json!({"encoding": "interval", "months": months, "days": days, "nanos": nanos.to_string()})
        }
        ValueRef::List(..)
        | ValueRef::Array(..)
        | ValueRef::Struct(..)
        | ValueRef::Map(..)
        | ValueRef::Union(..) => cli_owned(&Value::from(value))?,
        _ => anyhow::bail!("Unsupported result type; CAST the value to VARCHAR explicitly"),
    })
}

fn cli_float(value: f64) -> serde_json::Value {
    if value.is_finite() {
        serde_json::json!(value)
    } else {
        serde_json::json!({"encoding": "float", "value": value.to_string()})
    }
}

fn cli_owned(value: &Value) -> Result<serde_json::Value> {
    use serde_json::json;
    Ok(match value {
        Value::List(values) | Value::Array(values) => json!(values.iter().map(cli_owned).collect::<Result<Vec<_>>>()?),
        Value::Struct(fields) => json!({"encoding": "struct", "fields": fields.iter().map(|(name, value)| Ok(json!([name, cli_owned(value)?]))).collect::<Result<Vec<_>>>()?}),
        Value::Map(entries) => json!({"encoding": "map", "entries": entries.iter().map(|(key, value)| Ok(json!([cli_owned(key)?, cli_owned(value)?]))).collect::<Result<Vec<_>>>()?}),
        Value::Union(value) => json!({"encoding": "union-value", "value": cli_owned(value)?}),
        Value::HugeInt(_) => anyhow::bail!("Nested HUGEINT/UHUGEINT/DECIMAL(38,0) has ambiguous Arrow metadata; CAST it to VARCHAR explicitly"),
        other => cli_value(ValueRef::from(other))?,
    })
}

/// Longest a composite cell is allowed to run before it is elided.
///
/// A list of a thousand elements, or a map of as many keys, is one cell in one
/// row: past this it stops being readable text and starts being a wall. The
/// JSON format keeps it whole; this is the rendering, not the contract.
const CELL_CAP: usize = 120;

/// A cell as readable text.
///
/// [`CliResult`] holds every value exactly — a `DECIMAL` as its digits, a
/// `DATE` as days since the epoch — because that JSON is a contract a caller
/// parses. A rendering meant to be *read* wants the same values the results
/// grid shows, so this is the encoded cell read back: the counterpart of
/// [`value_to_string`], for values that have been through the encoder.
pub fn plain_text(value: &serde_json::Value) -> String {
    use serde_json::Value as Json;
    match value {
        Json::Null => "NULL".to_string(),
        Json::Bool(v) => v.to_string(),
        Json::Number(v) => v.to_string(),
        Json::String(v) => v.clone(),
        Json::Array(values) => {
            let inner: Vec<String> = values.iter().map(plain_text).collect();
            elide(format!("[{}]", inner.join(", ")))
        }
        Json::Object(fields) => plain_encoded(fields),
    }
}

/// A cell that arrived as an `encoding` object.
///
/// An unrecognised encoding falls back to its own JSON, which is at least the
/// truth about what arrived, rather than an empty cell that reads like a null.
fn plain_encoded(fields: &serde_json::Map<String, serde_json::Value>) -> String {
    let named = |name: &str| {
        fields
            .get(name)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
    };
    let integer = || named("value").parse::<i64>().ok();
    match named("encoding") {
        "integer" | "decimal" | "float" => named("value").to_string(),
        "date" => integer()
            .and_then(|days| i32::try_from(days).ok())
            .map(format_date)
            .unwrap_or_else(|| named("value").to_string()),
        "timestamp" => match integer() {
            Some(value) => format_precise_timestamp(named("unit"), value),
            None => named("value").to_string(),
        },
        "time" => match integer() {
            Some(value) => format_precise_time(named("unit"), value),
            None => named("value").to_string(),
        },
        "interval" => format_interval(
            fields.get("months").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            fields.get("days").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            named("nanos").parse().unwrap_or(0),
        ),
        "hex" => elide(format!("0x{}", named("value"))),
        "struct" => {
            let inner: Vec<String> = fields
                .get("fields")
                .and_then(|v| v.as_array())
                .map(|fields| {
                    fields
                        .iter()
                        .filter_map(|field| {
                            let pair = field.as_array()?;
                            Some(format!(
                                "{}: {}",
                                plain_text(pair.first()?),
                                plain_text(pair.get(1)?)
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();
            elide(format!("{{{}}}", inner.join(", ")))
        }
        "map" => {
            let inner: Vec<String> = fields
                .get("entries")
                .and_then(|v| v.as_array())
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| {
                            let pair = entry.as_array()?;
                            Some(format!(
                                "{}: {}",
                                plain_text(pair.first()?),
                                plain_text(pair.get(1)?)
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();
            elide(format!("{{{}}}", inner.join(", ")))
        }
        "union-value" => fields.get("value").map(plain_text).unwrap_or_default(),
        _ => serde_json::Value::Object(fields.clone()).to_string(),
    }
}

fn elide(text: String) -> String {
    if text.chars().count() <= CELL_CAP {
        return text;
    }
    let mut out: String = text.chars().take(CELL_CAP).collect();
    out.push('…');
    out
}

fn time_unit(name: &str) -> Option<TimeUnit> {
    // The names the encoder writes are this enum's `Debug` names, so a miss
    // here means the unit came from somewhere else and the raw count is the
    // honest answer.
    match name {
        "Second" => Some(TimeUnit::Second),
        "Millisecond" => Some(TimeUnit::Millisecond),
        "Microsecond" => Some(TimeUnit::Microsecond),
        "Nanosecond" => Some(TimeUnit::Nanosecond),
        _ => None,
    }
}

/// Seconds-resolution text with the fraction the value actually carries.
///
/// The grid shows whole seconds, because a column is a fixed width there. A
/// document is not, and dropping the fraction would make two events in the
/// same second read as one — so the fraction appears only when there is one.
fn format_precise_timestamp(unit: &str, value: i64) -> String {
    let Some(unit) = time_unit(unit) else {
        return value.to_string();
    };
    let Some(moment) = chrono::DateTime::from_timestamp_micros(unit.to_micros(value)) else {
        return value.to_string();
    };
    let seconds = moment.format("%Y-%m-%d %H:%M:%S").to_string();
    fraction(seconds, moment.timestamp_subsec_micros())
}

fn format_precise_time(unit: &str, value: i64) -> String {
    let Some(unit) = time_unit(unit) else {
        return value.to_string();
    };
    let micros = unit.to_micros(value);
    let seconds = micros.div_euclid(1_000_000);
    let text = format!(
        "{:02}:{:02}:{:02}",
        seconds.div_euclid(3_600),
        seconds.div_euclid(60) % 60,
        seconds % 60
    );
    fraction(text, micros.rem_euclid(1_000_000) as u32)
}

fn fraction(prefix: String, micros: u32) -> String {
    if micros == 0 {
        return prefix;
    }
    let digits = format!("{micros:06}");
    format!("{prefix}.{}", digits.trim_end_matches('0'))
}

/// The result as a Markdown table — the format for a document stream.
///
/// Two things a table cannot say are said under it instead: that the result had
/// no rows, and that it was cut short. A preview read as a complete answer is
/// the mistake worth spending a line on.
pub fn format_markdown(result: &CliResult) -> String {
    use std::fmt::Write as _;

    if result.columns.is_empty() {
        return "_No columns were returned._\n".to_string();
    }
    let mut out = String::new();
    for column in &result.columns {
        let _ = write!(out, "| {} ", markdown_cell(&column.name));
    }
    out.push_str("|\n");
    for _ in &result.columns {
        out.push_str("| --- ");
    }
    out.push_str("|\n");
    for row in &result.rows {
        for cell in row {
            let _ = write!(out, "| {} ", markdown_cell(&plain_text(cell)));
        }
        out.push_str("|\n");
    }
    if result.rows.is_empty() {
        out.push_str("_0 rows._\n");
    }
    if result.truncated {
        let _ = writeln!(
            out,
            "_Truncated at {} rows: more rows were available, so this is not the whole answer._",
            result.row_count
        );
    }
    out
}

/// A cell as one line of a table.
///
/// `|` is escaped because an unescaped one ends the cell early and shifts every
/// column after it. A backslash is escaped only where it would otherwise be
/// read as escaping the pipe, so `C:\Users` stays as written.
fn markdown_cell(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if chars.peek() == Some(&'|') => out.push_str("\\\\"),
            '|' => out.push_str("\\|"),
            '\n' => out.push_str("<br>"),
            '\r' => {}
            _ => out.push(ch),
        }
    }
    out
}

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
    Affected {
        count: u64,
        elapsed_ms: u128,
    },
}

impl QueryResult {
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

/// Execute `stmt` and return its rows, fetched from DuckDB a chunk at a time.
///
/// `Statement::query` executes through `duckdb_execute_prepared`, which
/// builds the *whole* result before the first row comes back: a `SELECT *`
/// over a 10M-row file scans and holds all 10M rows even though the caller
/// keeps the first 100k. Streaming execution produces rows as they are
/// fetched, so stopping at the row budget stops the scan too.
///
/// duckdb-rs has no streaming `Rows` constructor, but `stream_arrow` runs
/// the streaming execution and leaves the result on the statement, where
/// `raw_query` reads it row by row (the Arrow iterator itself is unused).
fn query_streaming<'stmt>(stmt: &'stmt mut duckdb::Statement<'_>) -> Result<duckdb::Rows<'stmt>> {
    drop(stmt.stream_arrow([])?);
    Ok(stmt.raw_query())
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

    // Column metadata is only available after execution, so execute first
    // and inspect afterwards.
    let mut rows = query_streaming(&mut stmt)?;
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
            .unwrap_or_else(|_| {
                row.get::<_, Value>(0)
                    .map(|v| value_to_string(&v))
                    .unwrap_or_default()
            });
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

    /// `HEADER` is a CSV-only `COPY` option. The parquet writer does not
    /// declare it, and DuckDB rejects an undeclared option outright, so
    /// sending it unconditionally makes every parquet export fail.
    fn copy_options(self) -> &'static str {
        match self {
            ExportFormat::Csv => ", HEADER true",
            ExportFormat::Parquet => "",
        }
    }
}

pub fn export_of(conn: &Connection, sql: &str, path: &str, format: ExportFormat) -> Result<()> {
    let trimmed = sql.trim().trim_end_matches(';');
    let escaped_path = path.replace('\'', "''");
    let expanded = crate::db::expand_tilde(&escaped_path);
    conn.execute_batch(&format!(
        "COPY ({trimmed}) TO '{expanded}' (FORMAT {}{})",
        format.duckdb_format(),
        format.copy_options()
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
        "select"
            | "with"
            | "show"
            | "describe"
            | "desc"
            | "explain"
            | "pragma"
            | "summarize"
            | "values"
            | "from"
            | "table"
            | "pivot"
            | "call"
    )
}

/// Keywords whose presence means the statement may have changed what the
/// schema sidebar shows — the table/view list, their columns, or the row
/// estimates that DML moves.
const MUTATING_KEYWORDS: &[&str] = &[
    "insert",
    "update",
    "delete",
    "create",
    "drop",
    "alter",
    "attach",
    "detach",
    "truncate",
    "replace",
    "copy",
    "install",
    "load",
    "set",
    "reset",
    "call",
    "pragma",
    "use",
    "comment",
    "grant",
    "revoke",
    "begin",
    "commit",
    "rollback",
    "vacuum",
    "checkpoint",
    "export",
    "import",
    "execute",
    "analyze",
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
        conn.execute_batch(
            "CREATE TABLE t(a INTEGER, b VARCHAR, c DOUBLE); INSERT INTO t VALUES (1, 'x', 2.5);",
        )
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
        export_of(
            &conn,
            "SELECT 1 AS a, 'x' AS b",
            dir.to_str().unwrap(),
            ExportFormat::Csv,
        )
        .unwrap();
        let content = std::fs::read_to_string(&dir).unwrap();
        assert!(content.contains("a,b"));
        std::fs::remove_file(&dir).ok();
    }

    #[test]
    fn export_parquet_writes_readable_file() {
        let conn = mem();
        let path = std::env::temp_dir().join("ducklocal_test_export.parquet");
        let path = path.to_str().unwrap();
        export_of(
            &conn,
            "SELECT 1 AS a, 'x' AS b",
            path,
            ExportFormat::Parquet,
        )
        .unwrap();
        // Reading it back proves the writer accepted the options and produced
        // parquet, rather than the file merely being non-empty.
        let rows: i64 = conn
            .query_row(
                &format!("SELECT count(*) FROM read_parquet('{path}')"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1);
        std::fs::remove_file(path).ok();
    }

    /// `run_of` formats cells from a borrowed `ValueRef`; this pins the output
    /// for the types where that path differs from the owning one, including the
    /// container types that fall back to it.
    #[test]
    fn wide_results_are_bounded_by_cells_not_just_rows() {
        let conn = mem();
        // 400 columns: the row cap alone would allow 40M cells.
        let selects: Vec<String> = (0..400).map(|ix| format!("i + {ix} AS c{ix}")).collect();
        let sql = format!("SELECT {} FROM range(200000) t(i)", selects.join(", "));
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
    fn row_budget_stops_the_scan_not_just_the_copy() {
        // A billion rows: materialized before the cap applied, this held 8 GB
        // and ran for minutes. Streamed, it stops once the budget is full.
        let conn = mem();
        let started = Instant::now();
        let QueryOutcome::Rows(result) =
            run_of(&conn, "SELECT i FROM range(1000000000) t(i)").unwrap()
        else {
            panic!("expected rows");
        };
        assert_eq!(result.rows.len(), MAX_ROWS);
        assert!(result.truncated);
        assert_eq!(result.rows[0], vec!["0".to_string()]);
        assert!(started.elapsed().as_secs() < 30, "the scan ran to the end");

        let cli = run_cli_of(&conn, "SELECT i FROM range(1000000000) t(i)", 10).unwrap();
        assert_eq!(cli.rows.len(), 10);
        assert!(cli.truncated);
    }

    #[test]
    fn errors_while_fetching_still_surface() {
        let conn = mem();
        assert!(run_of(&conn, "SELECT error('boom') FROM range(3)").is_err());
        assert!(run_cli_of(&conn, "SELECT error('boom') FROM range(3)", 10).is_err());
        // A result shorter than one chunk is complete, not truncated.
        let QueryOutcome::Rows(small) = run_of(&conn, "SELECT i FROM range(5) t(i)").unwrap()
        else {
            panic!("expected rows");
        };
        assert_eq!(small.rows.len(), 5);
        assert!(!small.truncated);
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

    /// Read back, an encoded cell says what the grid says — the two renderings
    /// of one value, not two opinions about it.
    #[test]
    fn plain_text_reads_encoded_cells_the_way_the_grid_shows_them() {
        let conn = mem();
        let result = run_cli_of(
            &conn,
            "SELECT NULL AS a,
                    true AS b,
                    CAST(-7 AS TINYINT) AS c,
                    CAST(2.5 AS DOUBLE) AS d,
                    CAST(1.25 AS DECIMAL(5,2)) AS e,
                    'héllo' AS f,
                    DATE '2024-10-04' AS g,
                    TIMESTAMP '2024-10-04 07:08:09' AS h,
                    TIME '07:08:09' AS i,
                    INTERVAL 3 DAY AS j,
                    [1, 2] AS k,
                    {'x': 1} AS l,
                    MAP {'x': 1} AS m",
            10,
        )
        .unwrap();
        let row: Vec<String> = result.rows[0].iter().map(plain_text).collect();
        assert_eq!(
            row,
            [
                "NULL",
                "true",
                "-7",
                "2.5",
                "1.25",
                "héllo",
                "2024-10-04",
                "2024-10-04 07:08:09",
                "07:08:09",
                "3 days",
                "[1, 2]",
                "{x: 1}",
                "{x: 1}",
            ]
        );
    }

    /// The digits the JSON contract exists to protect survive the readable
    /// rendering; nothing is rounded into a neighbour on the way to a document.
    #[test]
    fn plain_text_keeps_digits_json_kept() {
        assert_eq!(
            plain_text(
                &serde_json::json!({"encoding": "integer", "value": "170141183460469231731687303715884105727"})
            ),
            "170141183460469231731687303715884105727"
        );
        assert_eq!(
            plain_text(
                &serde_json::json!({"encoding": "decimal", "value": "1234567890.1234567890"})
            ),
            "1234567890.1234567890"
        );
        assert_eq!(
            plain_text(&serde_json::json!({"encoding": "float", "value": "-inf"})),
            "-inf"
        );
        assert_eq!(
            plain_text(&serde_json::json!(9_007_199_254_740_991i64)),
            "9007199254740991"
        );
        // A date is stored as a count of days, and a document is read by
        // someone who wants the date.
        assert_eq!(
            plain_text(&serde_json::json!({"encoding": "date", "unit": "Day", "value": "19783"})),
            "2024-03-01"
        );
        assert_eq!(
            plain_text(
                &serde_json::json!({"encoding": "timestamp", "unit": "Nanosecond", "value": "1709296496123456000"})
            ),
            "2024-03-01 12:34:56.123456"
        );
        // No fraction, because there is none — not a `.000000` that reads like
        // precision nobody asked for.
        assert_eq!(
            plain_text(
                &serde_json::json!({"encoding": "timestamp", "unit": "Second", "value": "1709296496"})
            ),
            "2024-03-01 12:34:56"
        );
        assert_eq!(
            plain_text(
                &serde_json::json!({"encoding": "time", "unit": "Microsecond", "value": "25536123456"})
            ),
            "07:05:36.123456"
        );
        assert_eq!(
            plain_text(&serde_json::json!({"encoding": "hex", "value": "00ff"})),
            "0x00ff"
        );
    }

    #[test]
    fn plain_text_elides_a_wall_but_not_a_value() {
        let long = "x".repeat(500);
        assert_eq!(plain_text(&serde_json::json!(long.clone())), long);
        let list: Vec<i64> = (0..200).collect();
        let rendered = plain_text(&serde_json::json!(list));
        assert!(rendered.ends_with('…'), "{rendered}");
        assert!(rendered.chars().count() <= CELL_CAP + 1, "{rendered}");
    }

    fn cli_result(
        columns: &[&str],
        rows: Vec<Vec<serde_json::Value>>,
        truncated: bool,
    ) -> CliResult {
        CliResult {
            columns: columns
                .iter()
                .map(|name| CliColumn {
                    name: (*name).to_string(),
                    arrow_type: "Utf8".to_string(),
                })
                .collect(),
            row_count: rows.len(),
            rows,
            truncated,
            elapsed_ms: 1,
        }
    }

    #[test]
    fn markdown_escapes_the_cells_that_would_break_the_table() {
        let result = cli_result(
            &["a|b"],
            vec![
                vec![serde_json::json!("x|y")],
                vec![serde_json::json!("C:\\Users")],
                vec![serde_json::json!("a\\|b")],
                vec![serde_json::json!("one\ntwo")],
            ],
            false,
        );
        let text = format_markdown(&result);
        assert_eq!(
            text,
            "| a\\|b |\n\
             | --- |\n\
             | x\\|y |\n\
             | C:\\Users |\n\
             | a\\\\\\|b |\n\
             | one<br>two |\n"
        );
    }

    #[test]
    fn markdown_says_when_the_answer_is_not_the_whole_answer() {
        let empty = format_markdown(&cli_result(&["n"], vec![], false));
        assert_eq!(empty, "| n |\n| --- |\n_0 rows._\n");

        let truncated =
            format_markdown(&cli_result(&["n"], vec![vec![serde_json::json!(1)]], true));
        assert!(
            truncated.starts_with("| n |\n| --- |\n| 1 |\n"),
            "{truncated}"
        );
        assert!(truncated.contains("_Truncated at 1 rows"), "{truncated}");

        // A statement that returned nothing to tabulate says so rather than
        // printing an empty header.
        assert_eq!(
            format_markdown(&cli_result(&[], vec![], false)),
            "_No columns were returned._\n"
        );
    }
}
