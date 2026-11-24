//! `ducklocal profile` — what a column actually holds, before anything is built
//! on top of it.
//!
//! The questions this answers are the ones that decide a chart, a scale and a
//! number format, and they are all answered wrong by guessing:
//!
//! - **How much time does this cover, and is it continuous?** An app whose
//!   default range is as long as the data has no previous period to compare
//!   against, and one that plots a column with holes in it draws a continuous
//!   line over days that are not there.
//! - **How far apart are the values?** Four series that differ by five orders
//!   of magnitude share a linear axis badly: the small ones round to nothing.
//! - **How many decimals does the column really use?** A `DECIMAL(38,10)`
//!   holding two-decimal money prints ten unless someone decides otherwise.
//!
//! Every statistic is one aggregate over the relation. The profile is a
//! `DESCRIBE`, a row count, and one `SELECT` of aggregates per batch of
//! columns: each batch is a single scan that reads only its own columns, and
//! batches are sized by the row count, so a wide or tall relation costs time
//! rather than memory. (One branch per column joined by `UNION ALL` scanned the
//! relation once per column and held every column's distinct and quantile
//! state at once: 5,000 columns of a million rows ran out of memory.)

use anyhow::{anyhow, Result};
use duckdb::Connection;
use serde_json::{json, Value};

/// What kind of statistics a column's type supports.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Numeric,
    Temporal,
    /// Ordered and countable, but neither of the above: strings, booleans.
    Plain,
    /// `LIST`, `STRUCT`, `MAP`, `UNION`. `min`/`max` are not defined on these,
    /// so the profile counts them and says nothing it cannot support.
    Nested,
}

fn shape_of(data_type: &str) -> Shape {
    let upper = data_type.to_uppercase();
    if upper.contains('[')
        || upper.starts_with("STRUCT")
        || upper.starts_with("MAP")
        || upper.starts_with("UNION")
    {
        return Shape::Nested;
    }
    let base = upper.split('(').next().unwrap_or(&upper).trim().to_string();
    match base.as_str() {
        "TINYINT" | "SMALLINT" | "INTEGER" | "BIGINT" | "HUGEINT" | "UTINYINT" | "USMALLINT"
        | "UINTEGER" | "UBIGINT" | "UHUGEINT" | "FLOAT" | "REAL" | "DOUBLE" | "DECIMAL"
        | "NUMERIC" => Shape::Numeric,
        "DATE" | "TIMESTAMP" | "DATETIME" | "TIMESTAMP_S" | "TIMESTAMP_MS" | "TIMESTAMP_NS"
        | "TIMESTAMPTZ" => Shape::Temporal,
        _ => Shape::Plain,
    }
}

/// `name` as a quoted SQL identifier.
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The relation to profile, as SQL.
///
/// A path that names a CSV/Parquet/JSON file this build can read becomes that
/// reader's call, and so does a glob over such files, read as one; a workbook has no table function, so its first sheet is
/// imported into `conn` as a TEMP table (which a read-only connection still
/// allows) and that table's name is the relation. Anything else is a table or
/// view name, quoted part by part so `main.orders` and a name with a space
/// both work.
pub fn relation_of(conn: &Connection, target: &str) -> Result<String> {
    let expanded = crate::db::expand_tilde(target);
    if let Some(reader) = crate::db::data_file_reader(&expanded) {
        let found = if crate::sources::is_pattern(&expanded) {
            !crate::sources::resolve(std::slice::from_ref(&expanded))
                .files
                .is_empty()
        } else {
            std::path::Path::new(&expanded).is_file()
        };
        if !found {
            return Err(anyhow!("No such file: {target}"));
        }
        return Ok(format!("{reader}('{}')", expanded.replace('\'', "''")));
    }
    if crate::db::is_excel_file(&expanded) {
        if !std::path::Path::new(&expanded).is_file() {
            return Err(anyhow!("No such file: {target}"));
        }
        let sheet = crate::excel::sheets(&expanded)?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("Workbook has no sheets: {target}"))?;
        let name = crate::db::available_view_name_of(conn, &crate::db::view_name_for(&expanded)?);
        crate::excel::attach_temporary_sheet(conn, &expanded, &sheet, &name)?;
        return Ok(quote(&name));
    }
    if target.trim().is_empty() {
        return Err(anyhow!("Name a data file, a table or a view to profile"));
    }
    Ok(target.split('.').map(quote).collect::<Vec<_>>().join("."))
}

/// How many statistics [`aggregates`] gives each column.
const STATISTICS: usize = 11;

/// One column's aggregates, as SQL expressions over the relation.
///
/// Every column gives the same eleven expressions in the same types, so a
/// batch reads back by position; a statistic a shape does not support is a
/// typed `NULL` rather than a missing expression.
fn aggregates(name: &str, data_type: &str) -> [String; STATISTICS] {
    let shape = shape_of(data_type);
    let column = quote(name);

    let (distinct, min, max) = match shape {
        Shape::Nested => (
            "CAST(NULL AS BIGINT)".to_string(),
            "CAST(NULL AS VARCHAR)".to_string(),
            "CAST(NULL AS VARCHAR)".to_string(),
        ),
        _ => (
            format!("CAST(count(DISTINCT {column}) AS BIGINT)"),
            format!("CAST(min({column}) AS VARCHAR)"),
            format!("CAST(max({column}) AS VARCHAR)"),
        ),
    };

    let (decimals, median) = match shape {
        // The digits after the point that the data actually uses, which is not
        // what the declared scale says. `split_part` of a value with no point
        // is the empty string, so an integer column answers 0 — and since it
        // can answer nothing else, it does so without casting every value to
        // text, which costs as much as the median does. NULL when the column
        // holds no values, as `max` over none would be.
        Shape::Numeric if is_integer(data_type) => (
            format!("CASE WHEN count({column}) > 0 THEN CAST(0 AS BIGINT) END"),
            format!("CAST(quantile_disc({column}, 0.5) AS VARCHAR)"),
        ),
        Shape::Numeric => (
            format!("CAST(max(length(split_part(CAST({column} AS VARCHAR), '.', 2))) AS BIGINT)"),
            // `quantile_disc`, not `quantile_cont`: the middle value the column
            // actually holds, at the precision it holds it. Interpolating
            // between two rows invents digits, and a profile that prints
            // `42234.134999999995` for two-decimal money teaches exactly the
            // formatting mistake it exists to prevent.
            format!("CAST(quantile_disc({column}, 0.5) AS VARCHAR)"),
        ),
        _ => (
            "CAST(NULL AS BIGINT)".to_string(),
            "CAST(NULL AS VARCHAR)".to_string(),
        ),
    };

    let (covered, span) = match shape {
        // Days the column names, against days between its ends. Equal means
        // continuous; fewer means holes, and the difference is how many.
        Shape::Temporal => (
            format!("CAST(count(DISTINCT CAST({column} AS DATE)) AS BIGINT)"),
            format!(
                "CAST(CAST(max({column}) AS DATE) - CAST(min({column}) AS DATE) + 1 AS BIGINT)"
            ),
        ),
        _ => (
            "CAST(NULL AS BIGINT)".to_string(),
            "CAST(NULL AS BIGINT)".to_string(),
        ),
    };

    // A text column of numbers in which one short word stands for "missing" —
    // R's `NA`, a spreadsheet's `#N/A`, a `-`. The word makes the reader type
    // the whole column VARCHAR, so its nulls count 0 and its min and max sort
    // as strings, and nothing else in the report says why. The word and its
    // rows are what to name; the numbers are what makes it a marker.
    let (marker, marker_rows, numbers) = if is_text(data_type) {
        let other = format!("TRY_CAST({column} AS DOUBLE) IS NULL AND {column} IS NOT NULL");
        (
            format!(
                "CASE WHEN count(DISTINCT {column}) FILTER (WHERE {other}) = 1 \
                 THEN CAST(any_value({column}) FILTER (WHERE {other}) AS VARCHAR) END"
            ),
            format!("CAST(count(*) FILTER (WHERE {other}) AS BIGINT)"),
            format!("CAST(count(TRY_CAST({column} AS DOUBLE)) AS BIGINT)"),
        )
    } else {
        (
            "CAST(NULL AS VARCHAR)".to_string(),
            "CAST(NULL AS BIGINT)".to_string(),
            "CAST(NULL AS BIGINT)".to_string(),
        )
    };

    [
        format!("CAST(count(*) - count({column}) AS BIGINT)"),
        distinct,
        min,
        max,
        decimals,
        median,
        covered,
        span,
        marker,
        marker_rows,
        numbers,
    ]
}

/// Whole-number types, whose values never have digits after a point.
fn is_integer(data_type: &str) -> bool {
    matches!(
        data_type.to_uppercase().as_str(),
        "TINYINT"
            | "SMALLINT"
            | "INTEGER"
            | "BIGINT"
            | "HUGEINT"
            | "UTINYINT"
            | "USMALLINT"
            | "UINTEGER"
            | "UBIGINT"
            | "UHUGEINT"
    )
}

fn is_text(data_type: &str) -> bool {
    matches!(
        data_type.to_uppercase().as_str(),
        "VARCHAR" | "TEXT" | "STRING"
    )
}

/// The longest word read as a missing-value marker. `NA`, `N/A`, `#N/A`,
/// `NULL`, `None`, `-` and `?` fit; a word longer than this is data.
const MARKER_CHARS: usize = 6;

/// `marker` when the column is otherwise numbers, else `None`.
fn null_marker(statistics: &Statistics) -> Option<&str> {
    let marker = statistics.marker.as_deref()?;
    let numbers = statistics.numbers.unwrap_or(0);
    (numbers > 0 && marker.chars().count() <= MARKER_CHARS).then_some(marker)
}

/// What to run instead when columns hold numbers and a marker: read the file
/// with the marker as NULL, or — for a table — cast it away.
fn marker_hint(relation: &str, found: &[(&str, &str)]) -> String {
    let mut markers: Vec<&str> = found.iter().map(|(_, marker)| *marker).collect();
    markers.sort_unstable();
    markers.dedup();
    let names = found
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ");
    let literal = |m: &str| format!("'{}'", m.replace('\'', "''"));
    let nullstr = match markers.as_slice() {
        [one] => literal(one),
        many => format!(
            "[{}]",
            many.iter()
                .map(|m| literal(m))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let file = relation
        .strip_prefix("read_csv_auto(")
        .and_then(|rest| rest.strip_suffix(')'));
    match file {
        Some(path) => format!(
            "{names} hold numbers and {nullstr} for missing values; read them as numbers \
             with read_csv({path}, nullstr = {nullstr})"
        ),
        None => format!(
            "{names} hold numbers and {nullstr} for missing values; TRY_CAST(column AS DOUBLE) \
             reads them as numbers with the marker as NULL"
        ),
    }
}

/// The relation's columns and their types, in its own order.
pub fn describe(conn: &Connection, relation: &str) -> Result<Vec<(String, String)>> {
    let mut describe = conn.prepare(&format!("DESCRIBE SELECT * FROM {relation}"))?;
    let columns = describe
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<_, _>>()?;
    Ok(columns)
}

/// Rows times columns the statements running at once aggregate at most. Exact
/// distinct counts and medians hold state in proportion to the values they
/// see, so memory follows the cells in flight; this keeps it in the hundreds
/// of megabytes for integer columns whatever the relation's shape.
const CELLS_IN_FLIGHT: i64 = 16_000_000;
/// …and columns per statement, however few the rows: past this a statement's
/// width costs more in planning than another scan of a short relation does.
const COLUMNS_PER_STATEMENT: usize = 64;
/// Statements run at once, each on its own connection to the same database.
/// One ungrouped aggregate leaves threads idle — a median is finished on one —
/// and the connections share DuckDB's one thread pool, so running a few side
/// by side fills it rather than oversubscribing the machine. Four profiled
/// 320 columns of a million rows 2.5 times as fast as one.
const WORKERS: usize = 4;

/// How many columns of a `row_count`-row relation one statement profiles
/// when `workers` statements run at once.
pub(crate) fn batch_size(row_count: i64, workers: usize) -> usize {
    let by_rows = CELLS_IN_FLIGHT / row_count.max(1) / workers.max(1) as i64;
    usize::try_from(by_rows)
        .unwrap_or(1)
        .clamp(1, COLUMNS_PER_STATEMENT)
}

/// Profile `columns` of `relation` on `conn`, as the JSON the CLI prints.
///
/// `target` is what the caller asked for and is echoed back; `relation` is what
/// [`relation_of`] made of it. They are separate arguments because resolving a
/// target is an argument question — a path that is not there is not a SQL
/// error — and the caller answers that one before opening a connection.
/// `columns` come from [`describe`], all of them or the ones asked for; when
/// they are a part, `of` is how many the relation has, and the output says so.
#[hotpath::measure]
pub fn run(
    conn: &Connection,
    target: &str,
    relation: &str,
    columns: &[(String, String)],
    of: Option<usize>,
) -> Result<Value> {
    let started = std::time::Instant::now();
    if columns.is_empty() {
        return Err(anyhow!("{target} has no columns"));
    }

    let row_count: i64 =
        conn.query_row(&format!("SELECT count(*) FROM {relation}"), [], |row| {
            row.get(0)
        })?;

    // Every batch reads the relation again, and a Parquet file's footer
    // describes all its columns however few a batch reads: 5,000 columns of
    // footer cost a third of a second per statement before a value is read.
    // Read it once. Nothing else on this connection needs it fresh, and the
    // cache checks the file's modification time anyway.
    let _ = conn.execute_batch(CACHE_FOOTERS);
    let helpers = if columns.len() > 1 {
        helpers_for(conn, relation)
    } else {
        Vec::new()
    };
    let size = batch_size(row_count, helpers.len() + 1);
    let batches: Vec<&[(String, String)]> = columns.chunks(size).collect();
    let found = profile_batches(conn, helpers, relation, &batches, size, columns.len())?;

    let described: Vec<Value> = columns
        .iter()
        .zip(&found)
        .map(|((name, data_type), statistics)| {
            column_value(name, data_type, Some(statistics), row_count)
        })
        .collect();

    let marked: Vec<(&str, &str)> = columns
        .iter()
        .zip(&found)
        .filter_map(|((name, _), statistics)| Some((name.as_str(), null_marker(statistics)?)))
        .collect();

    let mut profile = json!({
        "target": target,
        "relation": relation,
        "row_count": row_count,
        "columns": described,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    });
    if let Some(of) = of {
        profile["column_count"] = json!(of);
    }
    if !marked.is_empty() {
        profile["hint"] = json!(marker_hint(relation, &marked));
    }
    Ok(profile)
}

const CACHE_FOOTERS: &str = "SET parquet_metadata_cache = true";

/// Up to [`WORKERS`]` - 1` more connections to `conn`'s database that can
/// read `relation`. A workbook's sheet is a TEMP table, which only the
/// connection that made it sees; then there are none, and the profile runs
/// on `conn` alone.
fn helpers_for(conn: &Connection, relation: &str) -> Vec<Connection> {
    let mut helpers = Vec::new();
    for _ in 1..WORKERS {
        let Ok(helper) = conn.try_clone() else {
            break;
        };
        if helper
            .prepare(&format!("SELECT 1 FROM {relation} LIMIT 0"))
            .is_err()
        {
            break;
        }
        let _ = helper.execute_batch(CACHE_FOOTERS);
        helpers.push(helper);
    }
    helpers
}

/// Every batch's statistics, in batch order: `conn` and each of `helpers`
/// take the next batch not yet taken until none are left. The first error
/// stops the rest and is the one returned. `size` and `total` only word it.
fn profile_batches(
    conn: &Connection,
    helpers: Vec<Connection>,
    relation: &str,
    batches: &[&[(String, String)]],
    size: usize,
    total: usize,
) -> Result<Vec<Statistics>> {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let done: Mutex<Vec<Option<Result<Vec<Statistics>>>>> =
        Mutex::new(batches.iter().map(|_| None).collect());
    let work = |conn: &Connection| {
        while !failed.load(Ordering::Relaxed) {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(batch) = batches.get(index) else {
                break;
            };
            let read = profile_batch(conn, relation, batch, index * size, total);
            if read.is_err() {
                failed.store(true, Ordering::Relaxed);
            }
            done.lock().unwrap_or_else(|e| e.into_inner())[index] = Some(read);
        }
    };
    std::thread::scope(|scope| {
        for helper in helpers {
            let work = &work;
            scope.spawn(move || work(&helper));
        }
        work(conn);
    });

    let mut found = Vec::with_capacity(total);
    // A batch left unrun means an error stopped the rest; keep looking for it.
    for read in done
        .into_inner()
        .unwrap_or_else(|e| e.into_inner())
        .into_iter()
        .flatten()
    {
        found.extend(read?);
    }
    if found.len() != total {
        return Err(anyhow!("the profile stopped before every column was read"));
    }
    Ok(found)
}

/// One batch's statistics: a single `SELECT` of every column's aggregates.
/// `first` is the batch's first column's index among `total`, for the error.
fn profile_batch(
    conn: &Connection,
    relation: &str,
    batch: &[(String, String)],
    first: usize,
    total: usize,
) -> Result<Vec<Statistics>> {
    let expressions = batch
        .iter()
        .flat_map(|(name, data_type)| aggregates(name, data_type))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SELECT {expressions} FROM {relation}");
    let read = conn.query_row(&sql, [], |row| {
        (0..batch.len())
            .map(|i| {
                let at = i * STATISTICS;
                Ok(Statistics {
                    nulls: row.get(at)?,
                    distinct_values: row.get(at + 1)?,
                    min_value: row.get(at + 2)?,
                    max_value: row.get(at + 3)?,
                    decimals: row.get(at + 4)?,
                    median: row.get(at + 5)?,
                    covered_days: row.get(at + 6)?,
                    span_days: row.get(at + 7)?,
                    marker: row.get(at + 8)?,
                    marker_rows: row.get(at + 9)?,
                    numbers: row.get(at + 10)?,
                })
            })
            .collect::<duckdb::Result<Vec<_>>>()
    });
    read.map_err(|error| {
        let quoted = |(name, _): &(String, String)| format!("{name:?}");
        let names = match batch {
            [one] => quoted(one),
            [head, .., tail] => format!("{} … {}", quoted(head), quoted(tail)),
            [] => String::new(),
        };
        anyhow!(
            "{error} (profiling columns {}–{} of {total}: {names})",
            first + 1,
            first + batch.len(),
        )
    })
}

struct Statistics {
    nulls: i64,
    distinct_values: Option<i64>,
    min_value: Option<String>,
    max_value: Option<String>,
    decimals: Option<i64>,
    median: Option<String>,
    covered_days: Option<i64>,
    span_days: Option<i64>,
    /// The one non-number a text column holds besides numbers, if just one.
    marker: Option<String>,
    marker_rows: Option<i64>,
    /// Values that read as numbers.
    numbers: Option<i64>,
}

fn column_value(
    name: &str,
    data_type: &str,
    statistics: Option<&Statistics>,
    row_count: i64,
) -> Value {
    let mut value = json!({ "name": name, "type": data_type });
    let object = value.as_object_mut().expect("just built as an object");
    let Some(statistics) = statistics else {
        return value;
    };
    object.insert("nulls".into(), json!(statistics.nulls));
    if let Some(distinct) = statistics.distinct_values {
        object.insert("distinct".into(), json!(distinct));
        // The two shapes a column can have that change what to draw: one value
        // is a constant, and one value per row is a key.
        if row_count > 0 && distinct == row_count {
            object.insert("unique".into(), json!(true));
        }
    }
    if let Some(min) = &statistics.min_value {
        object.insert("min".into(), json!(min));
    }
    if let Some(max) = &statistics.max_value {
        object.insert("max".into(), json!(max));
    }
    if let Some(decimals) = statistics.decimals {
        object.insert("decimals".into(), json!(decimals));
    }
    if let Some(median) = &statistics.median {
        object.insert("median".into(), json!(median));
        // How many times the middle value the largest one is. This is the
        // number that decides a linear axis from a logarithmic one, and it is
        // spelled out rather than left to be derived from min/max/median.
        if let Some(ratio) = spread(median, statistics.max_value.as_deref()) {
            object.insert("max_over_median".into(), json!(ratio));
        }
    }
    if let Some(marker) = null_marker(statistics) {
        object.insert("null_marker".into(), json!(marker));
        object.insert(
            "null_marker_rows".into(),
            json!(statistics.marker_rows.unwrap_or(0)),
        );
    }
    if let (Some(covered), Some(span)) = (statistics.covered_days, statistics.span_days) {
        object.insert("covered_days".into(), json!(covered));
        object.insert("span_days".into(), json!(span));
        // Days inside the range that the column never names. Zero is a
        // continuous series; anything else is holes a line would draw over.
        object.insert("missing_days".into(), json!((span - covered).max(0)));
    }
    value
}

/// `max / median`, rounded to one decimal, when both parse and the median is
/// not zero. A ratio against zero is not a number worth printing.
fn spread(median: &str, max: Option<&str>) -> Option<f64> {
    let median: f64 = median.parse().ok()?;
    let max: f64 = max?.parse().ok()?;
    if median == 0.0 || !median.is_finite() || !max.is_finite() {
        return None;
    }
    let ratio = (max / median).abs();
    Some((ratio * 10.0).round() / 10.0)
}
