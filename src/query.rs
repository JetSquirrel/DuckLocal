//! Query execution and result serialization.
//!
//! Blocking functions; call via `smol::unblock` from UI code.

use std::ops::Range;
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
/// what any second reader of a result must reuse: a second encoder is how a
/// big integer quietly becomes a float.
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

#[hotpath::measure]
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
        ValueRef::Decimal(v) => tagged([("encoding", json!("decimal")), ("value", json!(v.to_string()))]),
        ValueRef::Text(v) => json!(std::str::from_utf8(v)?),
        ValueRef::Enum(..) => json!(value.as_str().map_err(|e| anyhow::anyhow!("{e:?}"))?),
        ValueRef::Blob(v) | ValueRef::Geometry(v) => {
            use std::fmt::Write as _;
            let mut hex = String::with_capacity(v.len() * 2);
            for byte in v {
                let _ = write!(hex, "{byte:02x}");
            }
            tagged([("encoding", json!("hex")), ("value", json!(hex))])
        }
        ValueRef::Date32(v) => tagged([
            ("encoding", json!("date")),
            ("unit", json!("Day")),
            ("value", json!(v.to_string())),
        ]),
        ValueRef::Timestamp(unit, v) => tagged([
            ("encoding", json!("timestamp")),
            ("unit", json!(unit_name(unit))),
            ("value", json!(v.to_string())),
        ]),
        ValueRef::Time64(unit, v) => tagged([
            ("encoding", json!("time")),
            ("unit", json!(unit_name(unit))),
            ("value", json!(v.to_string())),
        ]),
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

/// An encoded cell, built without the `json!` macro's per-call work: the map
/// is sized up front (with `preserve_order` it is an `IndexMap`, whose growth
/// reallocates both its table and its entries) and the unit is a static name
/// rather than a `Debug` render. Tagged cells were ~5x the cost of plain ones.
fn tagged<const N: usize>(fields: [(&'static str, serde_json::Value); N]) -> serde_json::Value {
    let mut map = serde_json::Map::with_capacity(N);
    for (key, value) in fields {
        map.insert(key.to_owned(), value);
    }
    serde_json::Value::Object(map)
}

/// `TimeUnit`'s `Debug` name, which is what the encoding has always carried.
fn unit_name(unit: TimeUnit) -> &'static str {
    match unit {
        TimeUnit::Second => "Second",
        TimeUnit::Millisecond => "Millisecond",
        TimeUnit::Microsecond => "Microsecond",
        TimeUnit::Nanosecond => "Nanosecond",
    }
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
#[hotpath::measure]
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

#[hotpath::measure]
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

/// The UTF-8 byte range in `sql` that an error `message` points at, if it
/// points at all.
///
/// Parser and binder errors render their position as a caret under the
/// offending line:
///
/// ```text
/// Catalog Error: Table with name t does not exist!
///
/// LINE 2: from t
///              ^
/// ```
///
/// The caret column counts *characters*, and the caret line is indented by
/// the whole `LINE n: ` label, so the label width comes off before the column
/// means anything. One caret marks where the token starts, so the range
/// extends over the token; a run of carets already spans it. Errors without a
/// position (runtime failures, a batch whose bind error carries none) return
/// `None`, and a rendered line that does not match the SQL — a DuckDB that
/// truncates long lines would produce one — yields `None` rather than a
/// squiggle in the wrong place.
pub fn error_byte_range(sql: &str, message: &str) -> Option<Range<usize>> {
    let mut rendered = message.lines();
    while let Some(header) = rendered.next() {
        let Some(rest) = header.strip_prefix("LINE ") else {
            continue;
        };
        let Some((digits, shown)) = rest.split_once(": ") else {
            continue;
        };
        let Ok(number) = digits.parse::<usize>() else {
            continue;
        };
        let caret = rendered.next().unwrap_or("");
        let padding = caret.len() - caret.trim_start_matches(' ').len();
        let label = "LINE ".len() + digits.len() + ": ".len();
        let column = padding.checked_sub(label)?;
        let carets = caret
            .trim_start_matches(' ')
            .chars()
            .take_while(|&c| c == '^')
            .count();

        // The line the number refers to, in the SQL that was run.
        let mut line_start = 0;
        for _ in 1..number {
            line_start = sql[line_start..].find('\n').map(|ix| line_start + ix + 1)?;
        }
        let line_end = sql[line_start..]
            .find('\n')
            .map(|ix| line_start + ix)
            .unwrap_or(sql.len());
        let line = &sql[line_start..line_end];
        // Trust the position only while the rendered line is the SQL's own;
        // otherwise the column maps to text the user never wrote.
        if shown != line {
            continue;
        }

        let byte_of_char = |index: usize| {
            line.char_indices()
                .nth(index)
                .map(|(ix, _)| ix)
                .unwrap_or(line.len())
        };
        let start = line_start + byte_of_char(column);
        let end = if carets > 1 {
            line_start + byte_of_char(column + carets)
        } else {
            // One caret marks where the token starts; underline the token.
            let mut end = start;
            for ch in sql[start..line_end].chars() {
                if ch.is_whitespace() {
                    break;
                }
                end += ch.len_utf8();
            }
            end
        };
        if start < end {
            return Some(start..end);
        }
        // A caret at or past the line's end marks nothing on its own; fall
        // back to the last character, which is where the caret was read.
        let (ix, ch) = line.char_indices().next_back()?;
        return Some(line_start + ix..line_start + ix + ch.len_utf8());
    }
    None
}

/// The one statement `sql` holds, without its `;`, or why there is not
/// exactly one.
///
/// Explain and Profile wrap the editor's text in a prefix, and DuckDB runs
/// every statement of a prepared text but the last one: wrapping
/// `SELECT 1; DELETE FROM t` would delete before anything is explained.
fn single_statement(sql: &str) -> Result<&str> {
    match crate::script::split(sql).as_slice() {
        [piece] if piece.kind == crate::script::PieceKind::Sql => Ok(&sql[piece.range.clone()]),
        _ => anyhow::bail!(crate::i18n::tr("query.single_statement")),
    }
}

/// Whether `sql` is one statement that only reads, by DuckDB's own parser:
/// `json_serialize_sql` serializes a single SELECT — `FROM`, `VALUES`,
/// `TABLE`, `SHOW`, `DESCRIBE` and `SUMMARIZE` all parse to one — and
/// refuses anything else, including a write behind a `WITH`. Nothing runs.
fn is_read_only_query(conn: &Connection, sql: &str) -> Result<bool> {
    let failed: Option<bool> = conn.query_row(
        "SELECT (json_serialize_sql(?::VARCHAR)::JSON ->> 'error')::BOOLEAN",
        [sql],
        |row| row.get(0),
    )?;
    Ok(failed == Some(false))
}

/// `EXPLAIN <sql>` rendered as plain text lines.
pub fn explain_of(conn: &Connection, sql: &str) -> Result<(Vec<String>, u128)> {
    let started = Instant::now();
    let statement = single_statement(sql)?;
    // On its own line: a trailing `-- comment` stays a comment.
    let mut stmt = conn.prepare(&format!("EXPLAIN {statement}\n"))?;
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

/// One operator of a profiled plan, as `EXPLAIN ANALYZE` measured it.
#[derive(Clone, Debug)]
pub struct PlanNode {
    pub name: String,
    /// Time spent in this operator alone, in seconds — children excluded.
    pub seconds: f64,
    /// Rows the operator produced.
    pub rows: u64,
    /// The optimizer's guess at `rows`, where the plan states one. A guess
    /// that is far off is the usual reason a join picked the wrong side.
    pub estimated: Option<u64>,
    /// The rest of DuckDB's `extra_info`: filters, projections, join keys.
    pub details: Vec<(String, String)>,
    pub children: Vec<PlanNode>,
}

/// A query run under `EXPLAIN ANALYZE`, its plan annotated with what each
/// operator cost.
#[derive(Clone, Debug)]
pub struct QueryProfile {
    /// Usually one tree; a plan whose root DuckDB does not name is split
    /// into its children rather than shown as a nameless node.
    pub roots: Vec<PlanNode>,
    /// Sum of every operator's own time. Operators of one pipeline run
    /// interleaved, so this is what the shares in the tree add up to, not
    /// the wall clock.
    pub operator_seconds: f64,
    pub elapsed_ms: u128,
}

impl QueryProfile {
    /// The operator that cost the most on its own.
    pub fn hottest(&self) -> Option<&PlanNode> {
        fn walk<'a>(node: &'a PlanNode, best: &mut Option<&'a PlanNode>) {
            if best.is_none_or(|b| node.seconds > b.seconds) {
                *best = Some(node);
            }
            for child in &node.children {
                walk(child, best);
            }
        }
        let mut best = None;
        for root in &self.roots {
            walk(root, &mut best);
        }
        best
    }
}

/// Run `sql` under `EXPLAIN (ANALYZE, FORMAT JSON)` and read back the
/// per-operator profile.
///
/// The statement really runs — that is how its operators get timed — so
/// only statements that read are profiled: an `UPDATE` profiled to see why
/// it is slow would also have updated. The first keyword does not say so —
/// `WITH x AS (…) INSERT …` starts like a query — so DuckDB's parser decides.
pub fn profile_of(conn: &Connection, sql: &str) -> Result<QueryProfile> {
    let statement = single_statement(sql)?;
    if !is_read_only_query(conn, statement)? {
        anyhow::bail!(crate::i18n::tr("query.profile.read_only"));
    }
    let started = Instant::now();
    let mut stmt = conn.prepare(&format!("EXPLAIN (ANALYZE, FORMAT JSON) {statement}\n"))?;
    let mut rows = stmt.query([])?;
    let mut json = None;
    while let Some(row) = rows.next()? {
        // (explain_key, explain_value): the value holds the JSON profile.
        if let Ok(text) = row.get::<_, String>(1) {
            json = Some(text);
        }
    }
    let elapsed_ms = started.elapsed().as_millis();
    let json = json.ok_or_else(|| anyhow::anyhow!("EXPLAIN ANALYZE returned no profile"))?;
    parse_profile(&json, elapsed_ms)
}

fn parse_profile(json: &str, elapsed_ms: u128) -> Result<QueryProfile> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    let roots = plan_nodes(&value);
    let operator_seconds = roots.iter().map(subtree_seconds).sum();
    Ok(QueryProfile {
        roots,
        operator_seconds,
        elapsed_ms,
    })
}

fn subtree_seconds(node: &PlanNode) -> f64 {
    node.seconds + node.children.iter().map(subtree_seconds).sum::<f64>()
}

/// The operators under `value`. The profile's root is the query itself and
/// its first child the `EXPLAIN_ANALYZE` wrapper; neither is part of the
/// plan the user wrote, so both give way to their children.
fn plan_nodes(value: &serde_json::Value) -> Vec<PlanNode> {
    let children = || -> Vec<PlanNode> {
        value
            .get("children")
            .and_then(|c| c.as_array())
            .map(|c| c.iter().flat_map(plan_nodes).collect())
            .unwrap_or_default()
    };
    let name = value
        .get("operator_name")
        .or_else(|| value.get("operator_type"))
        .and_then(|n| n.as_str())
        .map(str::trim)
        .unwrap_or("");
    if name.is_empty() || name == "EXPLAIN_ANALYZE" {
        return children();
    }
    let number = |key: &str| value.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let mut estimated = None;
    let mut details = Vec::new();
    if let Some(extra) = value.get("extra_info").and_then(|e| e.as_object()) {
        for (key, item) in extra {
            let text = match item {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Array(items) => items
                    .iter()
                    .map(|i| {
                        i.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| i.to_string())
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                other => other.to_string(),
            };
            if key == "Estimated Cardinality" {
                estimated = text.trim().parse().ok();
            } else if !text.is_empty() {
                details.push((key.clone(), text));
            }
        }
    }
    vec![PlanNode {
        name: name.to_string(),
        seconds: number("operator_timing"),
        rows: number("operator_cardinality") as u64,
        estimated,
        details,
        children: children(),
    }]
}

/// Format an operator's time, which is often well under a millisecond.
pub fn format_seconds(seconds: f64) -> String {
    if seconds < 0.001 {
        format!("{:.0} µs", seconds * 1_000_000.0)
    } else if seconds < 1.0 {
        format!("{:.1} ms", seconds * 1000.0)
    } else {
        format!("{seconds:.2} s")
    }
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
    matches!(
        keyword_of(sql).as_str(),
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

/// Whether `sql` is a query that can sit in a `FROM ( … )` and be run again
/// without doing anything but read: what a column overview of a result
/// re-runs. `CALL`, `PRAGMA` and `SHOW` return rows too, but are neither.
pub fn is_subquery(sql: &str) -> bool {
    matches!(
        keyword_of(sql).as_str(),
        "select" | "with" | "from" | "values" | "table" | "pivot" | "unpivot"
    )
}

/// The statement's leading keyword, lowercased, leading line comments skipped.
fn keyword_of(sql: &str) -> String {
    let mut rest = sql.trim_start();
    while let Some(after) = rest.strip_prefix("--") {
        rest = after
            .find('\n')
            .map(|ix| after[ix + 1..].trim_start())
            .unwrap_or("");
    }
    rest.chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_lowercase()
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

// Dates and timestamps are written field by field rather than through
// chrono's `format("%Y-…")`, which parses the pattern and allocates on every
// call: one allocation per cell instead of three, a third of the time. Years
// outside 0..=9999 keep chrono's rendering (a sign, more digits).
fn format_date(days: i32) -> String {
    use chrono::Datelike as _;
    match chrono::NaiveDate::from_num_days_from_ce_opt(days + 719_163) {
        Some(d) if (0..=9999).contains(&d.year()) => {
            format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
        }
        Some(d) => d.format("%Y-%m-%d").to_string(),
        None => format!("{days} days"),
    }
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
    use chrono::{Datelike as _, Timelike as _};
    match chrono::DateTime::from_timestamp_micros(micros) {
        Some(t) if (0..=9999).contains(&t.year()) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            t.year(),
            t.month(),
            t.day(),
            t.hour(),
            t.minute(),
            t.second()
        ),
        Some(t) => t.format("%Y-%m-%d %H:%M:%S").to_string(),
        None => v.to_string(),
    }
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

    /// The message shapes here are real DuckDB renderings; the assertions are
    /// byte ranges into the SQL the editor holds.
    #[test]
    fn error_positions_map_to_sql_bytes() {
        // A binder error names the column; one caret underlines the token.
        assert_eq!(
            error_byte_range(
                "select frum t",
                "Binder Error: Referenced column \"frum\" was not found\n\nLINE 1: select frum t\n               ^",
            ),
            Some(7..11)
        );

        // A later line, and a label wider than one digit.
        assert_eq!(
            error_byte_range(
                "-- 1\n-- 2\n-- 3\n-- 4\n-- 5\n-- 6\n-- 7\n-- 8\n-- 9\nselect nope",
                "Binder Error: Referenced column \"nope\" was not found\n\nLINE 10: select nope\n                ^",
            ),
            Some(52..56)
        );

        // The caret counts characters, not bytes: `é` is two bytes, so the
        // byte range sits one past the character column.
        assert_eq!(
            error_byte_range(
                "select 'aé' as x, nope",
                "Binder Error: Referenced column \"nope\" was not found\n\nLINE 1: select 'aé' as x, nope\n                          ^",
            ),
            Some(19..23)
        );

        // A run of carets already spans the token, including its last byte.
        assert_eq!(
            error_byte_range(
                "select 1\nfrom missing",
                "Catalog Error: Table with name missing does not exist!\n\nLINE 2: from missing\n             ^^^^^^^",
            ),
            Some(14..21)
        );

        // A caret at the end of the line still marks the last character.
        assert_eq!(
            error_byte_range(
                "select 'aé' +",
                "Binder Error: No function matches ...\n\nLINE 1: select 'aé' +\n                    ^",
            ),
            Some(13..14)
        );
    }

    #[test]
    fn errors_without_a_position_mark_nothing() {
        assert_eq!(
            error_byte_range("select *", "Parser Error: syntax error at end of input"),
            None
        );
        // A rendered line that is not the SQL's own — as a truncation of a
        // long line would read — is refused rather than misplaced.
        assert_eq!(
            error_byte_range(
                "select frum t",
                "Parser Error: ...\n\nLINE 1: select …\n               ^",
            ),
            None
        );
        // The line number must exist in the SQL that was run.
        assert_eq!(
            error_byte_range("select 1", "Parser Error: ...\n\nLINE 9: select 1\n               ^"),
            None
        );
    }

    #[test]
    fn profile_times_each_operator_of_the_plan() {
        let conn = mem();
        conn.execute_batch(
            "CREATE TABLE t AS SELECT range AS i, range % 7 AS g FROM range(100000)",
        )
        .unwrap();
        let profile = profile_of(
            &conn,
            "SELECT g, count(*) FROM t WHERE i > 10 GROUP BY g ORDER BY g;",
        )
        .unwrap();
        // The query and EXPLAIN_ANALYZE wrappers are not part of the plan.
        assert_eq!(profile.roots.len(), 1);
        fn names(node: &PlanNode, out: &mut Vec<String>) {
            out.push(node.name.clone());
            node.children.iter().for_each(|c| names(c, out));
        }
        let mut all = Vec::new();
        names(&profile.roots[0], &mut all);
        assert!(!all.iter().any(|n| n == "EXPLAIN_ANALYZE"), "{all:?}");
        assert!(all.iter().any(|n| n.contains("SCAN")), "{all:?}");
        assert!(all.iter().any(|n| n.contains("GROUP_BY")), "{all:?}");
        assert!(profile.operator_seconds > 0.0);
        assert!(profile.hottest().is_some());
        // The aggregate produces one row per group.
        fn find<'a>(node: &'a PlanNode, name: &str) -> Option<&'a PlanNode> {
            if node.name.contains(name) {
                return Some(node);
            }
            node.children.iter().find_map(|c| find(c, name))
        }
        assert_eq!(find(&profile.roots[0], "GROUP_BY").unwrap().rows, 7);
    }

    #[test]
    fn only_queries_count_as_subqueries() {
        for sql in [
            "SELECT 1",
            "-- c\nwith x as (select 1) from x",
            "FROM t",
            "VALUES (1)",
        ] {
            assert!(is_subquery(sql), "{sql}");
        }
        for sql in [
            "CALL pragma_version()",
            "PRAGMA version",
            "SHOW TABLES",
            "INSERT INTO t VALUES (1)",
        ] {
            assert!(!is_subquery(sql), "{sql}");
        }
    }

    #[test]
    fn profile_refuses_statements_that_write() {
        let conn = mem();
        conn.execute_batch("CREATE TABLE t(i INTEGER)").unwrap();
        for sql in [
            "INSERT INTO t VALUES (1)",
            "DELETE FROM t",
            "CALL pragma_version()",
            "EXPLAIN SELECT 1",
            // A write behind a CTE starts like a query.
            "WITH s AS (SELECT 2) INSERT INTO t SELECT * FROM s",
            // DuckDB would run the DELETE while preparing the last statement.
            "SELECT 1; DELETE FROM t",
            "INSERT INTO t VALUES (1); SELECT 1",
        ] {
            assert!(profile_of(&conn, sql).is_err(), "{sql}");
        }
        let count: i64 = conn
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn profile_reads_estimates_and_details() {
        let json = r#"{"children":[{"operator_name":"EXPLAIN_ANALYZE","children":[
            {"operator_name":"FILTER","operator_timing":0.002,"operator_cardinality":5,
             "extra_info":{"Expression":"(i > 10)","Estimated Cardinality":"400"},
             "children":[{"operator_name":"TABLE_SCAN","operator_timing":0.01,
             "operator_cardinality":400,"extra_info":{"Projections":["i","g"]},"children":[]}]}]}]}"#;
        let profile = parse_profile(json, 3).unwrap();
        let filter = &profile.roots[0];
        assert_eq!(filter.name, "FILTER");
        assert_eq!(filter.estimated, Some(400));
        assert_eq!(
            filter.details,
            vec![("Expression".into(), "(i > 10)".into())]
        );
        let scan = &filter.children[0];
        assert_eq!(scan.details, vec![("Projections".into(), "i, g".into())]);
        assert!((profile.operator_seconds - 0.012).abs() < 1e-9);
        assert_eq!(profile.hottest().unwrap().name, "TABLE_SCAN");
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
        // A trailing comment does not swallow anything.
        assert!(explain_of(&conn, "SELECT 42 -- answer").is_ok());
    }

    #[test]
    fn explain_never_runs_a_statement_before_the_last() {
        let conn = mem();
        conn.execute_batch("CREATE TABLE t AS SELECT 1 AS i").unwrap();
        assert!(explain_of(&conn, "DELETE FROM t; SELECT 1").is_err());
        assert!(explain_of(&conn, "SELECT 1; DELETE FROM t").is_err());
        // Explaining a write plans it without running it.
        assert!(explain_of(&conn, "DELETE FROM t;").is_ok());
        let count: i64 = conn
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn profile_runs_every_kind_of_query() {
        let conn = mem();
        conn.execute_batch("CREATE TABLE t AS SELECT 1 AS i").unwrap();
        for sql in [
            "WITH s AS (SELECT 2 AS i) SELECT * FROM s",
            "FROM t -- trailing comment",
            "SUMMARIZE t",
            "SELECT 1;",
        ] {
            assert!(profile_of(&conn, sql).is_ok(), "{sql}");
        }
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

    /// The crafted messages above pin the parsing; this pins the parsing
    /// against the messages the bundled DuckDB actually renders.
    #[test]
    fn real_duckdb_errors_map_to_the_span_they_render() {
        let conn = mem();
        let sql = "select 1\nfrom missing";
        let error = run_of(&conn, sql).unwrap_err().to_string();
        assert_eq!(error_byte_range(sql, &error), Some(14..21), "{error}");

        // Multibyte text before the caret: the column counts characters.
        let sql = "select 'aé' as x, nope";
        let error = run_of(&conn, sql).unwrap_err().to_string();
        assert_eq!(error_byte_range(sql, &error), Some(19..23), "{error}");

        // A runtime error renders no position, so nothing is marked.
        let sql = "select error('boom')";
        let error = run_of(&conn, sql).unwrap_err().to_string();
        assert_eq!(error_byte_range(sql, &error), None, "{error}");
    }
}
