//! The `ducklocal` host module scripts import.
//!
//! The functions are the whole surface between an analysis app and DuckLocal:
//! `catalog()` and `query()` run against the connection the main window is
//! using — the app is a second face on the same database, not a sandbox —
//! `appDir()` tells an app where its own files are (`panelDir()` is the same
//! answer under its old name), and `sqlLiteral()`
//! escapes a value into a SQL string literal so an app does not build SQL by
//! concatenation. The encoding of a cell is the CLI's
//! (`crate::query::run_cli_of`), so a `DECIMAL(38,10)`, a `HUGEINT`, a
//! `TIMESTAMP` or a `BLOB` arrives in JavaScript as the same explicit
//! `{encoding, value}` object `ducklocal query` prints, rather than as a float
//! that lost the digits.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use gpui_shell::{HostError, HostModule, HostObject, HostValue};

use crate::query::{run_cli_of, MAX_ROWS};
use crate::schema::{DatabaseInfo, NodeKind, TableInfo};

/// The module a script imports these from: `import { query } from "ducklocal"`.
pub const MODULE: &str = "ducklocal";

/// Rows `query()` returns when the script does not ask for a number.
pub const DEFAULT_ROW_LIMIT: usize = 1000;

const DECLARATIONS: &str = r#"
export interface Column {
  name: string;
  type: string;
}
export interface CatalogEntry {
  database: string;
  schema: string;
  name: string;
  kind: "table" | "view";
  estimated_rows: number | null;
  comment: string | null;
  columns: Column[];
}
export interface QueryResult {
  columns: Column[];
  rows: any[][];
  row_count: number;
  truncated: boolean;
  elapsed_ms: number;
}
export function catalog(): Promise<CatalogEntry[]>;
export function query(sql: string, limit?: number): Promise<QueryResult>;
export function appDir(): string;
/** @deprecated Use `appDir()` — the same answer under the old name. */
export function panelDir(): string;
export function sqlLiteral(value: string): string;
export function sqlIdentifier(name: string): string;
"#;

thread_local! {
    /// The directory of the app being mounted right now.
    ///
    /// `appDir()` has to answer "where is the app that is asking?", and
    /// nothing in the call reaches back to a view: a host function is handed
    /// arguments, not a caller. What it does have is the moment a host controls
    /// — mounting — so the directory is visible for exactly that call and for
    /// every host call the script's `init` makes inside it. Outside it the
    /// question has no answer that is not a guess, and a guess would hand two
    /// apps the same directory, so it is refused instead.
    static MOUNTING: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Makes `directory` the answer `appDir()` (and its `panelDir()` alias) gives,
/// for the duration of `body`.
///
/// The answer is restored by a guard rather than after `body` returns, so an
/// app whose `init` panics still leaves the next app a clean answer.
pub fn with_panel_directory<T>(directory: &Path, body: impl FnOnce() -> T) -> T {
    struct Restore(Option<PathBuf>);

    impl Drop for Restore {
        fn drop(&mut self) {
            MOUNTING.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }

    let previous = MOUNTING.with(|slot| slot.replace(Some(directory.to_path_buf())));
    let _restore = Restore(previous);
    body()
}

/// The directory of the app being mounted, if one is.
pub fn panel_directory() -> Option<PathBuf> {
    MOUNTING.with(|slot| slot.borrow().clone())
}

pub fn module() -> HostModule {
    HostModule::new(MODULE)
        .declarations(DECLARATIONS)
        .async_function("catalog", |arguments| {
            if !arguments.is_empty() {
                return Err(HostError::new("catalog() takes no arguments"));
            }
            Ok(async { catalog().map_err(|error| HostError::new(error.to_string())) })
        })
        .async_function("query", |arguments| {
            let sql = match arguments.string(0) {
                Ok(sql) => sql.to_string(),
                Err(_) => return Err(HostError::new("query(sql, limit?) needs a SQL string")),
            };
            let limit = arguments.integer(1).unwrap_or(DEFAULT_ROW_LIMIT as i64);
            if limit < 1 {
                return Err(HostError::new("query(sql, limit?) needs limit >= 1"));
            }
            let limit = (limit as usize).min(MAX_ROWS);
            Ok(async move {
                smol::unblock(move || query(&sql, limit))
                    .await
                    .map_err(|error| HostError::new(error.to_string()))
            })
        })
        .function("appDir", |arguments| {
            if !arguments.is_empty() {
                return Err(HostError::new("appDir() takes no arguments"));
            }
            panel_directory()
                .map(|directory| HostValue::from(directory.to_string_lossy().to_string()))
                .ok_or_else(|| {
                    HostError::new(
                        "appDir() is answered while the app loads: call it from init() and keep the result",
                    )
                })
        })
        // The pre-rename name, kept working: existing apps import it. Its
        // errors name `panelDir()`, because that is the function that was
        // called.
        .function("panelDir", |arguments| {
            if !arguments.is_empty() {
                return Err(HostError::new("panelDir() takes no arguments"));
            }
            panel_directory()
                .map(|directory| HostValue::from(directory.to_string_lossy().to_string()))
                .ok_or_else(|| {
                    HostError::new(
                        "panelDir() is answered while the app loads: call it from init() and keep the result",
                    )
                })
        })
        .function("sqlLiteral", |arguments| {
            let value = match arguments.string(0) {
                Ok(value) => value,
                Err(_) => return Err(HostError::new("sqlLiteral(value) needs a string")),
            };
            sql_literal(value)
                .map(HostValue::from)
                .map_err(HostError::new)
        })
        .function("sqlIdentifier", |arguments| {
            let name = match arguments.string(0) {
                Ok(name) => name,
                Err(_) => return Err(HostError::new("sqlIdentifier(name) needs a string")),
            };
            sql_identifier(name)
                .map(HostValue::from)
                .map_err(HostError::new)
        })
}

/// Every table and view of the current database, with its columns.
///
/// On the app connection, like [`query`]: a catalog read of a database with
/// many attached files is not instant either, and it is not worth stopping the
/// window for. Blocking; call through `smol::unblock`.
pub fn catalog() -> anyhow::Result<HostValue> {
    let databases = crate::db::with_app_connection(crate::schema::load_catalog_of)?;
    crate::app_export::capture::catalog(captured_tables(&databases));
    Ok(catalog_value(&databases))
}

/// The catalog in the shape an export writes down: the app's own view of the
/// database, without the bridge's types.
fn captured_tables(databases: &[DatabaseInfo]) -> Vec<crate::app_export::capture::Table> {
    databases
        .iter()
        .flat_map(|database| {
            database
                .tables
                .iter()
                .map(|table| crate::app_export::capture::Table {
                    database: database.name.clone(),
                    schema: table.schema.clone(),
                    name: table.name.clone(),
                    kind: match table.kind {
                        NodeKind::View => "view",
                        _ => "table",
                    },
                    estimated_rows: table.estimated_rows,
                    comment: table.comment.clone(),
                    columns: table
                        .columns
                        .iter()
                        .map(|column| (column.name.clone(), column.data_type.clone()))
                        .collect(),
                })
        })
        .collect()
}

/// Run `sql` on the app connection — the same database the window is on, a
/// different connection, so a slow statement here does not freeze the SQL
/// editor. Blocking; call through `smol::unblock`.
pub fn query(sql: &str, limit: usize) -> anyhow::Result<HostValue> {
    // A statement is recorded when it finishes; the guard is what tells a
    // watching settle loop that a slow query is running, not idle.
    let _in_flight = crate::app_export::capture::track_query();
    match crate::db::with_app_connection(|conn| run_cli_of(conn, sql, limit)) {
        Ok(result) => {
            let value = query_value(&result);
            crate::app_export::capture::query(sql, limit, Ok(&result));
            Ok(value)
        }
        Err(error) => {
            // A statement that failed is worth recording: a report of an app
            // whose query was rejected should say so.
            crate::app_export::capture::query(sql, limit, Err(error.to_string()));
            Err(error)
        }
    }
}

fn catalog_value(databases: &[DatabaseInfo]) -> HostValue {
    let mut entries = Vec::new();
    for database in databases {
        for table in &database.tables {
            entries.push(HostValue::from(catalog_entry(database, table)));
        }
    }
    HostValue::Array(entries)
}

fn catalog_entry(database: &DatabaseInfo, table: &TableInfo) -> HostObject {
    let columns = HostValue::Array(
        table
            .columns
            .iter()
            .map(|column| {
                HostValue::from(
                    HostObject::new()
                        .field("name", column.name.clone())
                        .field("type", column.data_type.clone()),
                )
            })
            .collect(),
    );
    HostObject::new()
        .field("database", database.name.clone())
        .field("schema", table.schema.clone())
        .field("name", table.name.clone())
        .field(
            "kind",
            match table.kind {
                NodeKind::View => "view",
                _ => "table",
            },
        )
        .field(
            "estimated_rows",
            match table.estimated_rows {
                Some(rows) => HostValue::from(rows),
                None => HostValue::Null,
            },
        )
        .field(
            "comment",
            match &table.comment {
                Some(comment) => HostValue::from(comment.clone()),
                None => HostValue::Null,
            },
        )
        .field("columns", columns)
}

fn query_value(result: &crate::query::CliResult) -> HostValue {
    let columns = HostValue::Array(
        result
            .columns
            .iter()
            .map(|column| {
                HostValue::from(
                    HostObject::new()
                        .field("name", column.name.clone())
                        .field("type", column.arrow_type.clone()),
                )
            })
            .collect(),
    );
    let rows = HostValue::Array(
        result
            .rows
            .iter()
            .map(|row| {
                HostValue::Array(row.iter().map(|cell| json_to_host(cell.clone())).collect())
            })
            .collect(),
    );
    HostValue::from(
        HostObject::new()
            .field("columns", columns)
            .field("rows", rows)
            .field("row_count", result.row_count as i64)
            .field("truncated", result.truncated)
            .field("elapsed_ms", elapsed_ms(result.elapsed_ms)),
    )
}

/// `u128` milliseconds do not fit an `i64` in theory; a query would have to run
/// for 292 million years. Saturate rather than wrap.
fn elapsed_ms(elapsed: u128) -> i64 {
    elapsed.min(i64::MAX as u128) as i64
}

/// A JSON value as the plain-data value the script bridge carries.
///
/// Plain JSON numbers only ever appear here for values that already fit an
/// IEEE double exactly — everything else `cli_value` encoded as an object — so
/// this loses nothing the encoder did not already decide to lose.
fn json_to_host(value: serde_json::Value) -> HostValue {
    use serde_json::Value;
    match value {
        Value::Null => HostValue::Null,
        Value::Bool(flag) => HostValue::from(flag),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                HostValue::from(integer)
            } else if let Some(integer) = number.as_u64() {
                // Beyond `i64::MAX` is beyond `2^53` too, which the encoder
                // never leaves as a plain number; keep the branch honest.
                match i64::try_from(integer) {
                    Ok(integer) => HostValue::from(integer),
                    Err(_) => HostValue::from(integer as f64),
                }
            } else {
                HostValue::from(number.as_f64().unwrap_or(f64::NAN))
            }
        }
        Value::String(text) => HostValue::from(text),
        Value::Array(items) => HostValue::Array(items.into_iter().map(json_to_host).collect()),
        Value::Object(fields) => {
            let mut object = HostObject::new();
            for (key, field) in fields {
                object = object.field(key, json_to_host(field));
            }
            HostValue::from(object)
        }
    }
}

/// The entry file `load_application` is asked for, and the directory a script's
/// imports resolve against.
pub const ENTRY: &str = "main.js";

/// `value` as a SQL string literal, quotes and all.
///
/// The only escape a DuckDB string literal needs is a doubled single quote,
/// which is what makes building one by wrapping a value in quotes so easy to
/// get wrong: a channel named `O'Brien` ends the literal early and the rest of
/// the value is read as SQL. A NUL byte has no representation in a literal and
/// is refused rather than silently truncated.
pub fn sql_literal(value: &str) -> Result<String, String> {
    if value.contains('\0') {
        return Err(
            "sqlLiteral(value): a NUL byte cannot appear in a SQL string literal".to_string(),
        );
    }
    Ok(format!("'{}'", value.replace('\'', "''")))
}

/// `name` as a quoted SQL identifier, quotes and all.
///
/// The counterpart to [`sql_literal`] for the other half of a generated
/// statement. An app builds SQL from names it did not choose — `catalog()`
/// answers with whatever the database holds — and a name is not a string
/// literal: `SELECT * FROM "my table"` needs the quotes, and a name holding a
/// `"` closes them early unless it is doubled. Quoting also settles the case
/// question, because a quoted identifier is matched exactly as written.
pub fn sql_identifier(name: &str) -> Result<String, String> {
    if name.is_empty() {
        return Err("sqlIdentifier(name): an identifier cannot be empty".to_string());
    }
    if name.contains('\0') {
        return Err("sqlIdentifier(name): a NUL byte cannot appear in an identifier".to_string());
    }
    Ok(format!("\"{}\"", name.replace('"', "\"\"")))
}

/// Why a directory cannot be loaded as an app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// The path is not a directory — a file was named, or the path is gone.
    NotADirectory,
    /// There is no [`ENTRY`] in it, so there is nothing to load.
    NoEntryFile,
}

/// Whether `directory` can be loaded, and if not, why.
///
/// Checked before anything is watched or loaded, so a reader learns what is
/// wrong with the directory they named instead of watching an empty window.
pub fn validate_application(directory: &Path) -> Result<(), Rejection> {
    if !directory.is_dir() {
        return Err(Rejection::NotADirectory);
    }
    if !directory.join(ENTRY).is_file() {
        return Err(Rejection::NoEntryFile);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use duckdb::Connection;
    use std::path::PathBuf;

    fn connection() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE orders(id BIGINT, amount DECIMAL(38,10), note VARCHAR, made_at TIMESTAMP);
             CREATE VIEW big_orders AS SELECT * FROM orders WHERE amount > 100;",
        )
        .unwrap();
        conn
    }

    /// One field of a plain-data object, which crosses the bridge as a list of
    /// `(name, value)` pairs rather than as a map.
    fn field<'a>(fields: &'a [(String, HostValue)], key: &str) -> Option<&'a HostValue> {
        fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    fn text<'a>(fields: &'a [(String, HostValue)], key: &str) -> Option<&'a str> {
        field(fields, key).and_then(HostValue::as_str)
    }

    fn entries_of(value: &HostValue) -> Vec<Vec<(String, HostValue)>> {
        value
            .as_array()
            .expect("catalog() answers an array")
            .iter()
            .map(|entry| {
                entry
                    .as_object()
                    .expect("every entry is an object")
                    .to_vec()
            })
            .collect()
    }

    #[test]
    fn catalog_lists_tables_views_and_their_columns() {
        let value = catalog_value(&crate::schema::load_catalog_of(&connection()).unwrap());
        let entries = entries_of(&value);
        assert!(!entries.is_empty());

        let orders = entries
            .iter()
            .find(|entry| text(entry, "name") == Some("orders"))
            .expect("the table is listed");
        assert_eq!(text(orders, "kind"), Some("table"));
        assert_eq!(text(orders, "schema"), Some("main"));
        assert!(field(orders, "comment").is_some_and(|value| value.is_null()));

        let columns = field(orders, "columns")
            .and_then(HostValue::as_array)
            .expect("columns are an array");
        let names: Vec<&str> = columns
            .iter()
            .filter_map(|column| column.as_object().and_then(|fields| text(fields, "name")))
            .collect();
        assert_eq!(names, vec!["id", "amount", "note", "made_at"]);
        assert_eq!(
            columns[1]
                .as_object()
                .and_then(|fields| text(fields, "type")),
            // `catalog()` reports DuckDB's own type string, where `query()`
            // reports the Arrow type it executed with.
            Some("DECIMAL(38,10)")
        );

        let view = entries
            .iter()
            .find(|entry| text(entry, "name") == Some("big_orders"))
            .expect("the view is listed");
        assert_eq!(text(view, "kind"), Some("view"));
    }

    #[test]
    fn a_missing_connection_is_an_error_and_not_an_empty_catalog() {
        let _guard = crate::db::connection_guard();
        crate::db::close().unwrap();
        assert!(catalog().is_err());
        crate::db::open_memory().unwrap();
        assert!(catalog().is_ok());
    }

    fn query_columns(value: &HostValue) -> Vec<String> {
        field_of(value, "columns")
            .and_then(HostValue::as_array)
            .expect("columns")
            .iter()
            .filter_map(|column| column.as_object().and_then(|fields| text(fields, "name")))
            .map(String::from)
            .collect()
    }

    /// A field of a `query()` result object.
    fn field_of<'a>(value: &'a HostValue, key: &str) -> Option<&'a HostValue> {
        value.as_object().and_then(|fields| field(fields, key))
    }

    fn rows(value: &HostValue) -> Vec<Vec<HostValue>> {
        field_of(value, "rows")
            .and_then(HostValue::as_array)
            .expect("rows")
            .iter()
            .map(|row| row.as_array().expect("a row is an array").to_vec())
            .collect()
    }

    fn count(value: &HostValue, key: &str) -> i64 {
        field_of(value, key)
            .and_then(HostValue::as_number)
            .unwrap_or_else(|| panic!("{key} is a number")) as i64
    }

    fn flag(value: &HostValue, key: &str) -> bool {
        field_of(value, key)
            .and_then(HostValue::as_bool)
            .unwrap_or_else(|| panic!("{key} is a boolean"))
    }

    /// A field of an encoded cell, e.g. the `value` inside
    /// `{"encoding": "decimal", "value": "…"}`.
    fn encoded(cell: &HostValue, key: &str) -> Option<String> {
        cell.as_object()
            .and_then(|fields| field(fields, key))
            .and_then(HostValue::as_str)
            .map(String::from)
    }

    fn encoding(cell: &HostValue) -> Option<String> {
        encoded(cell, "encoding")
    }

    #[test]
    fn query_hands_rows_over_as_the_cli_encoding() {
        let _guard = crate::db::connection_guard();
        crate::db::open_memory().unwrap();
        crate::db::with_connection(|conn| {
            Ok(conn.execute_batch(
                "CREATE TABLE t(big HUGEINT, dec DECIMAL(38,10), made_on DATE, made_at TIMESTAMP,
                                 blob BLOB, nothing INTEGER, label VARCHAR);
                 INSERT INTO t VALUES (170141183460469231731687303715884105727,
                                       12345678901234567890.1234567890,
                                       DATE '2024-03-01', TIMESTAMP '2024-03-01 12:34:56.789',
                                       from_hex('00FF'), NULL, 'ok');",
            )?)
        })
        .unwrap();

        let value = query(
            "SELECT big, dec, made_on, made_at, blob, nothing, label FROM t",
            DEFAULT_ROW_LIMIT,
        )
        .unwrap();

        assert_eq!(
            query_columns(&value),
            vec!["big", "dec", "made_on", "made_at", "blob", "nothing", "label"]
        );
        let row = &rows(&value)[0];

        // A HUGEINT larger than 2^53 would silently lose digits as a plain
        // number, so it crosses as the CLI's explicit encoding.
        assert_eq!(encoding(&row[0]).as_deref(), Some("integer"));
        assert_eq!(
            encoded(&row[0], "value").as_deref(),
            Some("170141183460469231731687303715884105727")
        );

        assert_eq!(encoding(&row[1]).as_deref(), Some("decimal"));
        assert_eq!(
            encoded(&row[1], "value").as_deref(),
            Some("12345678901234567890.1234567890")
        );

        assert_eq!(encoding(&row[2]).as_deref(), Some("date"));
        assert_eq!(encoded(&row[2], "value").as_deref(), Some("19783"));

        assert_eq!(encoding(&row[3]).as_deref(), Some("timestamp"));
        assert_eq!(
            encoded(&row[3], "value").as_deref(),
            Some("1709296496789000")
        );

        assert_eq!(encoding(&row[4]).as_deref(), Some("hex"));
        assert_eq!(encoded(&row[4], "value").as_deref(), Some("00ff"));

        assert!(row[5].is_null());
        assert_eq!(row[6].as_str(), Some("ok"));

        crate::db::close().unwrap();
    }

    #[test]
    fn small_numbers_cross_as_numbers() {
        let _guard = crate::db::connection_guard();
        crate::db::open_memory().unwrap();
        let value = query(
            "SELECT 1::INTEGER AS i, 2.5::DOUBLE AS f, TRUE AS b, 'x' AS s",
            DEFAULT_ROW_LIMIT,
        )
        .unwrap();
        let row = &rows(&value)[0];
        assert_eq!(row[0].as_number(), Some(1.));
        assert_eq!(row[1].as_number(), Some(2.5));
        assert_eq!(row[2].as_bool(), Some(true));
        assert_eq!(row[3].as_str(), Some("x"));
        assert_eq!(count(&value, "row_count"), 1);
        assert!(!flag(&value, "truncated"));
        crate::db::close().unwrap();
    }

    #[test]
    fn the_row_limit_bounds_the_result_and_reports_truncation() {
        let _guard = crate::db::connection_guard();
        crate::db::open_memory().unwrap();
        crate::db::with_connection(|conn| {
            Ok(conn.execute_batch(
                "CREATE TABLE n(i INTEGER); INSERT INTO n SELECT * FROM range(50);",
            )?)
        })
        .unwrap();

        let bounded = query("SELECT i FROM n", 10).unwrap();
        assert_eq!(rows(&bounded).len(), 10);
        assert_eq!(count(&bounded, "row_count"), 10);
        assert!(flag(&bounded, "truncated"));

        let whole = query("SELECT i FROM n", 50).unwrap();
        assert_eq!(rows(&whole).len(), 50);
        assert!(!flag(&whole, "truncated"));

        // Past the end is not a truncation: the result simply ends there.
        let generous = query("SELECT i FROM n", 5_000).unwrap();
        assert_eq!(rows(&generous).len(), 50);
        assert!(!flag(&generous, "truncated"));
        crate::db::close().unwrap();
    }

    #[test]
    fn a_broken_statement_is_an_error_not_an_empty_result() {
        let _guard = crate::db::connection_guard();
        crate::db::open_memory().unwrap();
        assert!(query("SELECT * FROM no_such_table", 10).is_err());
        assert!(query("this is not sql", 10).is_err());
        crate::db::close().unwrap();
    }

    #[test]
    fn the_module_registers_exactly_the_documented_functions() {
        let module = module();
        module.validate().unwrap();
        let mut names = module.function_names();
        names.sort();
        assert_eq!(
            names,
            vec!["appDir", "catalog", "panelDir", "query", "sqlIdentifier", "sqlLiteral"]
        );
        assert!(module.is_async("catalog"));
        assert!(module.is_async("query"));
        // The other four answer immediately: none has anything to wait for.
        assert!(!module.is_async("appDir"));
        assert!(!module.is_async("panelDir"));
        assert!(!module.is_async("sqlLiteral"));
        assert!(!module.is_async("sqlIdentifier"));
    }

    #[test]
    fn a_sql_literal_wraps_and_escapes_quotes() {
        assert_eq!(sql_literal("orders").unwrap(), "'orders'");
        assert_eq!(sql_literal("").unwrap(), "''");
        assert_eq!(sql_literal("O'Brien").unwrap(), "'O''Brien'");
        // The value that ends a literal early and lets the rest be read as SQL.
        assert_eq!(
            sql_literal("'; DROP TABLE orders; --").unwrap(),
            "'''; DROP TABLE orders; --'"
        );
        assert_eq!(sql_literal("'").unwrap(), "''''");
        assert_eq!(sql_literal("''").unwrap(), "''''''");
        // A backslash is not an escape in a DuckDB literal, so it is left alone
        // rather than doubled — doubling it would change the value.
        assert_eq!(sql_literal("C:\\data").unwrap(), "'C:\\data'");
        assert!(sql_literal("bad\0value").is_err());
    }

    #[test]
    fn a_sql_literal_is_what_duckdb_reads_back() {
        // The point of escaping is that the round trip returns the same value.
        let _guard = crate::db::connection_guard();
        crate::db::open_memory().unwrap();
        for value in [
            "plain",
            "O'Brien",
            "'; DROP TABLE t; --",
            "back\\slash",
            "quote' and 'quote",
        ] {
            let sql = format!("SELECT {} AS v", sql_literal(value).unwrap());
            let read_back: String = crate::db::with_connection(|conn| {
                conn.query_row(&sql, [], |row| row.get(0))
                    .map_err(Into::into)
            })
            .unwrap();
            assert_eq!(read_back, value, "round trip failed for {value:?}");
        }
        crate::db::close().unwrap();
    }

    #[test]
    fn a_sql_identifier_quotes_and_doubles_inner_quotes() {
        assert_eq!(sql_identifier("orders").unwrap(), "\"orders\"");
        assert_eq!(sql_identifier("my table").unwrap(), "\"my table\"");
        // A name that would close the quotes early and let the rest be read as
        // SQL — the identifier half of the injection `sqlLiteral` refuses.
        assert_eq!(
            sql_identifier("t\"; DROP TABLE orders; --").unwrap(),
            "\"t\"\"; DROP TABLE orders; --\""
        );
        // A single quote is an ordinary character inside a quoted identifier:
        // doubling it would rename the table.
        assert_eq!(sql_identifier("O'Brien").unwrap(), "\"O'Brien\"");
        // Neither of these names a table, and quoting them would only hide that.
        assert!(sql_identifier("").is_err());
        assert!(sql_identifier("bad\0name").is_err());
    }

    #[test]
    fn a_sql_identifier_names_the_table_it_was_read_from() {
        // The round trip that matters: a name out of the catalog, quoted, finds
        // the same table back — including the cases that need the quotes.
        let _guard = crate::db::connection_guard();
        crate::db::open_memory().unwrap();
        for name in ["orders", "my table", "Mixed Case", "sel\"ect", "select"] {
            let quoted = sql_identifier(name).unwrap();
            crate::db::with_connection(|conn| {
                conn.execute_batch(&format!("CREATE TABLE {quoted} (v INTEGER)"))
                    .map_err(Into::into)
            })
            .unwrap();
            let read_back: String = crate::db::with_connection(|conn| {
                conn.query_row(
                    &format!(
                        "SELECT table_name FROM information_schema.tables WHERE table_name = {}",
                        sql_literal(name).unwrap()
                    ),
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
            assert_eq!(read_back, name, "round trip failed for {name:?}");
        }
        crate::db::close().unwrap();
    }

    #[test]
    fn an_app_reads_the_database_the_window_is_on() {
        // The app connection is a second connection, not a second database:
        // what the window created is there, and what the app creates is there
        // for the window.
        let _guard = crate::db::connection_guard();
        crate::db::open_memory().unwrap();
        crate::db::with_connection(|conn| {
            conn.execute_batch("CREATE TABLE orders AS SELECT 1 AS id")
                .map_err(Into::into)
        })
        .unwrap();

        let rows: i64 = crate::db::with_app_connection(|conn| {
            conn.query_row("SELECT count(*) FROM orders", [], |row| row.get(0))
                .map_err(Into::into)
        })
        .unwrap();
        assert_eq!(rows, 1);

        crate::db::with_app_connection(|conn| {
            conn.execute_batch("CREATE TABLE from_an_app AS SELECT 2 AS id")
                .map_err(Into::into)
        })
        .unwrap();
        let seen: i64 = crate::db::with_connection(|conn| {
            conn.query_row("SELECT id FROM from_an_app", [], |row| row.get(0))
                .map_err(Into::into)
        })
        .unwrap();
        assert_eq!(seen, 2);
        crate::db::close().unwrap();
    }

    #[test]
    fn an_app_follows_the_window_to_another_database() {
        // Opening another database has to take the app connection with it.
        // A clone left behind would keep answering from the database the window
        // closed, which is the one failure a second connection could introduce.
        let _guard = crate::db::connection_guard();
        crate::db::open_memory().unwrap();
        crate::db::with_connection(|conn| {
            conn.execute_batch("CREATE TABLE only_in_the_first AS SELECT 1")
                .map_err(Into::into)
        })
        .unwrap();
        crate::db::with_app_connection(|conn| {
            conn.query_row("SELECT count(*) FROM only_in_the_first", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(Into::into)
        })
        .unwrap();

        crate::db::open_memory().unwrap();
        assert!(crate::db::with_app_connection(|conn| {
            conn.query_row("SELECT count(*) FROM only_in_the_first", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(Into::into)
        })
        .is_err());
        crate::db::close().unwrap();
    }

    #[test]
    fn panel_dir_is_answered_while_an_app_is_mounting_and_not_after() {
        let directory = TempDir::new("panel_dir");
        assert_eq!(panel_directory(), None);
        let inside = with_panel_directory(directory.path(), panel_directory);
        assert_eq!(inside.as_deref(), Some(directory.path()));

        // Answering outside a mount would hand every app the same directory,
        // so the question is refused instead.
        assert_eq!(panel_directory(), None);
    }

    #[test]
    fn panel_dir_handles_a_nested_mount() {
        let outer = TempDir::new("panel_dir_outer");
        let inner = TempDir::new("panel_dir_inner");
        with_panel_directory(outer.path(), || {
            assert_eq!(panel_directory().as_deref(), Some(outer.path()));
            with_panel_directory(inner.path(), || {
                assert_eq!(panel_directory().as_deref(), Some(inner.path()));
            });
            assert_eq!(panel_directory().as_deref(), Some(outer.path()));
        });
        assert_eq!(panel_directory(), None);
    }

    #[test]
    fn panel_dir_restores_the_previous_answer_even_when_the_body_panics() {
        let directory = TempDir::new("panel_dir_panic");
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_panel_directory(directory.path(), || panic!("inside a mount"));
        }));
        assert!(caught.is_err());
        assert_eq!(panel_directory(), None);
    }

    /// A directory that removes itself, named for the test that made it.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ducklocal_app_{label}_{}_{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::remove_dir_all(&path).ok();
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn a_directory_with_an_entry_file_is_an_app() {
        let directory = TempDir::new("valid");
        std::fs::write(directory.path().join(ENTRY), "export default class A {}").unwrap();
        assert_eq!(validate_application(directory.path()), Ok(()));
    }

    #[test]
    fn a_directory_without_an_entry_file_is_named_as_such() {
        let directory = TempDir::new("no_entry");
        assert_eq!(
            validate_application(directory.path()),
            Err(Rejection::NoEntryFile)
        );
        // A foreign entry file is not the entry file.
        std::fs::write(directory.path().join("index.js"), "1").unwrap();
        assert_eq!(
            validate_application(directory.path()),
            Err(Rejection::NoEntryFile)
        );
    }

    #[test]
    fn a_file_is_not_a_directory_rather_than_a_missing_entry_file() {
        let directory = TempDir::new("file");
        let file = directory.path().join("main.js");
        std::fs::write(&file, "1").unwrap();
        assert_eq!(validate_application(&file), Err(Rejection::NotADirectory));

        let gone = directory.path().join("gone");
        assert_eq!(validate_application(&gone), Err(Rejection::NotADirectory));
    }
}
