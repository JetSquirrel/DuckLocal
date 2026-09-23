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
//! Every statistic is one aggregate over the relation, and the whole profile is
//! three statements: `DESCRIBE`, a row count, and one `UNION ALL` of per-column
//! aggregates.

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
/// reader's call; a workbook has no table function, so its first sheet is
/// imported into `conn` as a TEMP table (which a read-only connection still
/// allows) and that table's name is the relation. Anything else is a table or
/// view name, quoted part by part so `main.orders` and a name with a space
/// both work.
pub fn relation_of(conn: &Connection, target: &str) -> Result<String> {
    let expanded = crate::db::expand_tilde(target);
    if let Some(reader) = crate::db::data_file_reader(&expanded) {
        if !std::path::Path::new(&expanded).is_file() {
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

/// One column's aggregates, as one branch of the union.
///
/// Every branch selects the same nine columns in the same types so the branches
/// can be read back as one result; a statistic a shape does not support is a
/// typed `NULL` rather than a missing column.
fn branch(name: &str, data_type: &str, relation: &str) -> String {
    let shape = shape_of(data_type);
    let column = quote(name);
    let literal = format!("'{}'", name.replace('\'', "''"));

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
        // is the empty string, so an integer column answers 0.
        Shape::Numeric => (
            format!(
                "CAST(max(length(split_part(CAST({column} AS VARCHAR), '.', 2))) AS BIGINT)"
            ),
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

    format!(
        "SELECT {literal} AS name, \
         CAST(count(*) - count({column}) AS BIGINT) AS nulls, \
         {distinct} AS distinct_values, \
         {min} AS min_value, \
         {max} AS max_value, \
         {decimals} AS decimals, \
         {median} AS median, \
         {covered} AS covered_days, \
         {span} AS span_days \
         FROM {relation}"
    )
}

/// Profile `relation` on `conn`, as the JSON the CLI prints.
///
/// `target` is what the caller asked for and is echoed back; `relation` is what
/// [`relation_of`] made of it. They are separate arguments because resolving a
/// target is an argument question — a path that is not there is not a SQL
/// error — and the caller answers that one before opening a connection.
pub fn run(conn: &Connection, target: &str, relation: &str) -> Result<Value> {
    let started = std::time::Instant::now();

    let mut describe = conn.prepare(&format!("DESCRIBE SELECT * FROM {relation}"))?;
    let columns: Vec<(String, String)> = describe
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    if columns.is_empty() {
        return Err(anyhow!("{target} has no columns"));
    }

    let row_count: i64 =
        conn.query_row(&format!("SELECT count(*) FROM {relation}"), [], |row| {
            row.get(0)
        })?;

    let union = columns
        .iter()
        .map(|(name, data_type)| branch(name, data_type, relation))
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    let mut statistics = conn.prepare(&union)?;
    let rows: Vec<Statistics> = statistics
        .query_map([], |row| {
            Ok(Statistics {
                name: row.get(0)?,
                nulls: row.get(1)?,
                distinct_values: row.get(2)?,
                min_value: row.get(3)?,
                max_value: row.get(4)?,
                decimals: row.get(5)?,
                median: row.get(6)?,
                covered_days: row.get(7)?,
                span_days: row.get(8)?,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;

    // `UNION ALL` does not promise an order, and a profile read next to a
    // `DESCRIBE` should be in the table's own column order.
    let described: Vec<Value> = columns
        .iter()
        .map(|(name, data_type)| {
            let found = rows.iter().find(|row| &row.name == name);
            column_value(name, data_type, found, row_count)
        })
        .collect();

    Ok(json!({
        "target": target,
        "relation": relation,
        "row_count": row_count,
        "columns": described,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    }))
}

struct Statistics {
    name: String,
    nulls: i64,
    distinct_values: Option<i64>,
    min_value: Option<String>,
    max_value: Option<String>,
    decimals: Option<i64>,
    median: Option<String>,
    covered_days: Option<i64>,
    span_days: Option<i64>,
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
