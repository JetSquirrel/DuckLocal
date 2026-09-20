//! Excel/ODS import: a workbook becomes DuckDB tables, one per sheet.
//!
//! Unlike CSV/Parquet/JSON there is no DuckDB table function in this build —
//! calamine reads the file, infers a column type per column from the cells it
//! holds, and the rows are materialized with a batched INSERT. The first row
//! of a sheet is its header; a sheet with no rows at all is skipped by the
//! caller (`attach_sheet` answers `false`).
//!
//! All functions here are blocking; UI code must call them via `smol::unblock`.

use anyhow::{anyhow, Result};
use calamine::{Data, DataType, Reader};
use duckdb::types::{TimeUnit, Value};
use duckdb::Connection;

/// The sheet names of a workbook, in workbook order.
pub fn sheets(path: &str) -> Result<Vec<String>> {
    let workbook = open(path)?;
    Ok(workbook.sheet_names().to_vec())
}

/// Import `sheet` of the workbook at `path` as the table `table_name`,
/// replacing a table of that name. Answers `false` — and creates nothing —
/// when the sheet has no rows at all, so the caller can skip it rather than
/// register a name for nothing.
pub fn attach_sheet(
    conn: &Connection,
    path: &str,
    sheet: &str,
    table_name: &str,
) -> Result<bool> {
    import(conn, path, sheet, table_name, false)
}

/// The same import as a TEMP table: the CLI profiles a workbook on a
/// connection that may be read-only, which persistent tables cannot be
/// created on.
pub fn attach_temporary_sheet(
    conn: &Connection,
    path: &str,
    sheet: &str,
    table_name: &str,
) -> Result<bool> {
    import(conn, path, sheet, table_name, true)
}

fn import(
    conn: &Connection,
    path: &str,
    sheet: &str,
    table_name: &str,
    temporary: bool,
) -> Result<bool> {
    let mut workbook = open(path)?;
    let range = workbook.worksheet_range(sheet)?;
    let header = match range.rows().next() {
        Some(header) if !header.is_empty() => header.to_vec(),
        _ => return Ok(false),
    };
    let names = column_names(&header);
    let kinds = infer_kinds(range.rows().skip(1), names.len());

    let columns = names
        .iter()
        .zip(&kinds)
        .map(|(name, kind)| format!("{} {}", quote(name), sql_type(*kind)))
        .collect::<Vec<_>>()
        .join(", ");
    let keyword = if temporary { "TEMP TABLE" } else { "TABLE" };
    conn.execute_batch(&format!(
        "CREATE OR REPLACE {keyword} {} ({columns})",
        quote(table_name)
    ))?;

    let placeholders = (1..=names.len())
        .map(|n| format!("?{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(&format!(
            "INSERT INTO {} VALUES ({placeholders})",
            quote(table_name)
        ))?;
        for row in range.rows().skip(1) {
            let values = names
                .iter()
                .enumerate()
                .map(|(ix, _)| cell_value(row.get(ix).unwrap_or(&Data::Empty), kinds[ix]))
                .collect::<Vec<_>>();
            stmt.execute(duckdb::params_from_iter(values))?;
        }
    }
    tx.commit()?;
    Ok(true)
}

fn open(path: &str) -> Result<calamine::Sheets<std::io::BufReader<std::fs::File>>> {
    calamine::open_workbook_auto(path).map_err(|e| anyhow!("Cannot read workbook {path}: {e}"))
}

/// `name` as a quoted SQL identifier.
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The header row, made into usable column names: an empty cell becomes
/// `column_N` by position, and a name already used to its left gets a
/// counter, because DuckDB tables cannot hold two columns of one name.
fn column_names(header: &[Data]) -> Vec<String> {
    let mut names = Vec::with_capacity(header.len());
    for (ix, cell) in header.iter().enumerate() {
        let base = match cell {
            Data::Empty => format!("column_{}", ix + 1),
            other => cell_text(other).unwrap_or_else(|| format!("column_{}", ix + 1)),
        };
        let name = if names.contains(&base) {
            (2..)
                .map(|n| format!("{base}_{n}"))
                .find(|name| !names.contains(name))
                .unwrap_or(base)
        } else {
            base
        };
        names.push(name);
    }
    names
}

/// The DuckDB type a column can hold every non-empty cell of.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ColumnKind {
    BigInt,
    Double,
    Boolean,
    Timestamp,
    Varchar,
}

fn sql_type(kind: ColumnKind) -> &'static str {
    match kind {
        ColumnKind::BigInt => "BIGINT",
        ColumnKind::Double => "DOUBLE",
        ColumnKind::Boolean => "BOOLEAN",
        ColumnKind::Timestamp => "TIMESTAMP",
        ColumnKind::Varchar => "VARCHAR",
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Category {
    Int,
    Float,
    Bool,
    Timestamp,
    Varchar,
}

fn category(cell: &Data) -> Category {
    match cell {
        Data::Int(_) => Category::Int,
        // Excel stores every number as a double, and calamine's xlsx reader
        // answers `Float` for all of them; a column of whole numbers is still
        // an integer column.
        Data::Float(f) if as_i64_float(*f).is_some() => Category::Int,
        Data::Float(_) => Category::Float,
        Data::Bool(_) => Category::Bool,
        Data::DateTime(_) | Data::DateTimeIso(_) => Category::Timestamp,
        _ => Category::Varchar,
    }
}

fn as_i64(cell: &Data) -> Option<i64> {
    match cell {
        Data::Int(n) => Some(*n),
        Data::Float(f) => as_i64_float(*f),
        _ => None,
    }
}

fn as_i64_float(f: f64) -> Option<i64> {
    let exact = f.is_finite() && f.fract() == 0.0 && f >= -(2f64.powi(63)) && f < 2f64.powi(63);
    exact.then_some(f as i64)
}

/// Integers widen to DOUBLE when a float shows up; any other mixture — and a
/// column of nothing but empty cells — is VARCHAR.
fn infer_kinds<'a>(rows: impl Iterator<Item = &'a [Data]>, width: usize) -> Vec<ColumnKind> {
    let mut categories: Vec<std::collections::HashSet<Category>> =
        (0..width).map(|_| Default::default()).collect();
    for row in rows {
        for (ix, cell) in row.iter().enumerate().take(width) {
            if !matches!(cell, Data::Empty) {
                categories[ix].insert(category(cell));
            }
        }
    }
    categories
        .iter()
        .map(|seen| {
            if seen.len() == 1 {
                match seen.iter().next() {
                    Some(Category::Int) => ColumnKind::BigInt,
                    Some(Category::Float) => ColumnKind::Double,
                    Some(Category::Bool) => ColumnKind::Boolean,
                    Some(Category::Timestamp) => ColumnKind::Timestamp,
                    _ => ColumnKind::Varchar,
                }
            } else if seen.len() == 2
                && seen.contains(&Category::Int)
                && seen.contains(&Category::Float)
            {
                ColumnKind::Double
            } else {
                ColumnKind::Varchar
            }
        })
        .collect()
}

/// The SQL value one cell contributes under its column's inferred type. An
/// empty cell, or a cell whose kind the column's type cannot hold, is NULL;
/// in a VARCHAR column everything has a text form instead.
fn cell_value(cell: &Data, kind: ColumnKind) -> Value {
    match kind {
        ColumnKind::BigInt => match as_i64(cell) {
            Some(n) => Value::BigInt(n),
            None => Value::Null,
        },
        ColumnKind::Double => match cell {
            Data::Int(n) => Value::Double(*n as f64),
            Data::Float(f) => Value::Double(*f),
            _ => Value::Null,
        },
        ColumnKind::Boolean => match cell {
            Data::Bool(b) => Value::Boolean(*b),
            _ => Value::Null,
        },
        ColumnKind::Timestamp => match as_datetime(cell) {
            Some(dt) => Value::Timestamp(TimeUnit::Microsecond, dt.and_utc().timestamp_micros()),
            None => Value::Null,
        },
        ColumnKind::Varchar => match cell_text(cell) {
            Some(text) => Value::Text(text),
            None => Value::Null,
        },
    }
}

/// The moment a cell names, as a naive datetime: calamine answers
/// `NaiveDateTime`, and the ISO form carries no offset either.
fn as_datetime(cell: &Data) -> Option<chrono::NaiveDateTime> {
    cell.as_datetime().or_else(|| match cell {
        Data::DateTimeIso(text) => chrono::DateTime::parse_from_rfc3339(text)
            .map(|dt| dt.naive_utc())
            .ok(),
        _ => None,
    })
}

/// Every cell's text form; `None` for an empty one. Dates render ISO so a
/// mixed column still reads as dates.
fn cell_text(cell: &Data) -> Option<String> {
    match cell {
        Data::Empty => None,
        Data::String(s) => Some(s.clone()),
        Data::DateTime(_) | Data::DateTimeIso(_) => {
            as_datetime(cell).map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        }
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_xlsxwriter::{ExcelDateTime, Format, Workbook};

    /// Write a workbook fixture and return its path.
    fn workbook(name: &str, build: impl FnOnce(&mut Workbook)) -> String {
        let path = std::env::temp_dir().join(format!("ducklocal_excel_{name}.xlsx"));
        std::fs::remove_file(&path).ok();
        let mut book = Workbook::new();
        build(&mut book);
        book.save(&path).unwrap();
        path.to_string_lossy().to_string()
    }

    fn column_types(conn: &Connection, table: &str) -> Vec<(String, String)> {
        conn.prepare(&format!("DESCRIBE {}", quote(table)))
            .unwrap()
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn typed_columns_and_nullable_cells_roundtrip() {
        let path = workbook("typed", |book| {
            let sheet = book.add_worksheet().set_name("Orders").unwrap();
            let date = Format::new().set_num_format("yyyy-mm-dd hh:mm:ss");
            for (col, header) in ["id", "amount", "when", "note"].iter().enumerate() {
                sheet.write_string(0, col as u16, *header).unwrap();
            }
            sheet.write_number(1, 0, 1).unwrap();
            sheet.write_number(1, 1, 9.5).unwrap();
            sheet
                .write_datetime_with_format(
                    1,
                    2,
                    ExcelDateTime::from_ymd(2026, 9, 1)
                        .unwrap()
                        .and_hms(12, 0, 0)
                        .unwrap(),
                    &date,
                )
                .unwrap();
            sheet.write_string(1, 3, "hit").unwrap();
            // A second row with holes: empty cells must land as NULL.
            sheet.write_number(2, 0, 2).unwrap();
            sheet.write_number(2, 1, 3).unwrap();
        });

        let conn = Connection::open_in_memory().unwrap();
        assert!(attach_sheet(&conn, &path, "Orders", "orders").unwrap());

        assert_eq!(
            column_types(&conn, "orders"),
            [
                ("id".to_string(), "BIGINT".to_string()),
                ("amount".to_string(), "DOUBLE".to_string()),
                ("when".to_string(), "TIMESTAMP".to_string()),
                ("note".to_string(), "VARCHAR".to_string()),
            ]
        );
        let (id, amount, when, note): (i64, f64, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT id, amount, \"when\"::VARCHAR, note FROM orders WHERE id = 2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(id, 2);
        assert_eq!(amount, 3.0);
        assert_eq!(when, None, "empty cell must be NULL");
        assert_eq!(note, None);
        let first: (f64, String) = conn
            .query_row(
                "SELECT amount, \"when\"::VARCHAR FROM orders WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(first.0, 9.5);
        assert!(first.1.starts_with("2026-09-01 12:00:00"), "{}", first.1);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn empty_and_duplicate_headers_are_named() {
        let path = workbook("headers", |book| {
            let sheet = book.add_worksheet().set_name("Sheet1").unwrap();
            sheet.write_string(0, 0, "name").unwrap();
            sheet.write_string(0, 2, "name").unwrap();
            sheet.write_string(1, 0, "a").unwrap();
            sheet.write_string(1, 1, "b").unwrap();
            sheet.write_string(1, 2, "c").unwrap();
        });

        let conn = Connection::open_in_memory().unwrap();
        assert!(attach_sheet(&conn, &path, "Sheet1", "t").unwrap());

        assert_eq!(
            column_types(&conn, "t"),
            [
                ("name".to_string(), "VARCHAR".to_string()),
                ("column_2".to_string(), "VARCHAR".to_string()),
                ("name_2".to_string(), "VARCHAR".to_string()),
            ]
        );
        let row: (String, String, String) = conn
            .query_row("SELECT name, column_2, name_2 FROM t", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(row, ("a".into(), "b".into(), "c".into()));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn mixed_and_empty_columns_are_varchar() {
        let path = workbook("mixed", |book| {
            let sheet = book.add_worksheet().set_name("Sheet1").unwrap();
            sheet.write_string(0, 0, "mixed").unwrap();
            sheet.write_string(0, 1, "empty").unwrap();
            sheet.write_number(1, 0, 1).unwrap();
            sheet.write_string(2, 0, "text").unwrap();
        });

        let conn = Connection::open_in_memory().unwrap();
        assert!(attach_sheet(&conn, &path, "Sheet1", "t").unwrap());

        let types = column_types(&conn, "t");
        assert_eq!(types[0], ("mixed".to_string(), "VARCHAR".to_string()));
        assert_eq!(types[1], ("empty".to_string(), "VARCHAR".to_string()));
        let values: Vec<Option<String>> = conn
            .prepare("SELECT mixed FROM t ORDER BY mixed")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(values, [Some("1".to_string()), Some("text".to_string())]);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn an_empty_sheet_creates_nothing_and_a_bare_header_an_empty_table() {
        let path = workbook("empty", |book| {
            book.add_worksheet().set_name("Nothing").unwrap();
            let sheet = book.add_worksheet().set_name("Bare").unwrap();
            sheet.write_string(0, 0, "only_header").unwrap();
        });

        assert_eq!(sheets(&path).unwrap(), ["Nothing", "Bare"]);
        let conn = Connection::open_in_memory().unwrap();
        assert!(!attach_sheet(&conn, &path, "Nothing", "n").unwrap());
        assert!(attach_sheet(&conn, &path, "Bare", "b").unwrap());
        assert_eq!(
            column_types(&conn, "b"),
            [("only_header".to_string(), "VARCHAR".to_string())]
        );
        let count: i64 = conn
            .query_row("SELECT count(*) FROM b", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        std::fs::remove_file(&path).ok();
    }
}

