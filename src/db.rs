//! DuckDB connection management.
//!
//! Single global connection behind a mutex: `duckdb::Connection` is `Send`
//! but not `Sync`, so all access serializes through `with_connection`.
//! All functions here are blocking; UI code must call them via `smol::unblock`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use duckdb::Connection;
use lazy_static::lazy_static;

use crate::i18n::trf;

lazy_static! {
    static ref CONNECTION: Arc<Mutex<Option<Connection>>> = Arc::new(Mutex::new(None));
}

/// How the current database was opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DatabaseTarget {
    File(String),
    Memory,
}

impl DatabaseTarget {
    pub fn display_label(&self) -> String {
        match self {
            DatabaseTarget::File(path) => compact_home(path),
            DatabaseTarget::Memory => ":memory:".to_string(),
        }
    }
}

/// `$HOME`, read once. `display_label` runs on every frame of both the title
/// bar and the status bar, so it should not go back to the environment each
/// time.
fn home() -> Option<&'static str> {
    static HOME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HOME.get_or_init(|| std::env::var("HOME").ok()).as_deref()
}

/// Display `$HOME` as `~`.
fn compact_home(path: &str) -> String {
    if let Some(rest) = home().and_then(|home| path.strip_prefix(home)) {
        return format!("~{rest}");
    }
    path.to_string()
}

/// Replace a `~/` prefix with `$HOME` expanded.
pub fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home() {
            return format!("{home}/{rest}");
        }
    }
    path.to_string()
}

/// Open a file-backed database, replacing any current connection.
/// Parent directories are created when missing.
pub fn open_file(path: &str) -> Result<()> {
    let expanded = expand_tilde(path);
    if let Some(parent) = std::path::Path::new(&expanded).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let conn = Connection::open(&expanded)?;
    let mut guard = lock()?;
    *guard = Some(conn);
    Ok(())
}

/// Open an in-memory database, replacing any current connection.
pub fn open_memory() -> Result<()> {
    let mut guard = lock()?;
    *guard = Some(Connection::open_in_memory()?);
    Ok(())
}

pub fn close() -> Result<()> {
    let mut guard = lock()?;
    *guard = None;
    Ok(())
}

pub fn is_connected() -> bool {
    lock().map(|g| g.is_some()).unwrap_or(false)
}

/// Run `f` with the current connection. All blocking DB work goes through here.
pub fn with_connection<T>(f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let guard = lock()?;
    let conn = guard
        .as_ref()
        .ok_or_else(|| anyhow!("No database connected"))?;
    f(conn)
}

fn lock() -> Result<std::sync::MutexGuard<'static, Option<Connection>>> {
    CONNECTION
        .lock()
        .map_err(|e| anyhow!("Database lock poisoned: {e}"))
}

/// Serializes tests that use the process-global connection: `open_memory` and
/// `close` swap it out from under whoever else is running.
#[cfg(test)]
pub(crate) fn connection_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Server metadata for the title bar and status bar.
#[derive(Clone, Debug)]
pub struct ServerInfo {
    pub target: DatabaseTarget,
    pub version: String,
    pub threads: String,
    pub memory_limit: String,
}

pub fn server_info_of(conn: &Connection, target: DatabaseTarget) -> Result<ServerInfo> {
    let version: String = conn.query_row("SELECT version()", [], |r| r.get(0))?;
    let threads = setting_or(conn, "threads", "8");
    let memory_limit = setting_or(conn, "memory_limit", "-");
    Ok(ServerInfo {
        target,
        version,
        threads,
        memory_limit,
    })
}

fn setting_or(conn: &Connection, name: &str, default: &str) -> String {
    conn.query_row(
        "SELECT value FROM duckdb_settings() WHERE name = ?1",
        [name],
        |r| r.get::<_, String>(0),
    )
    .unwrap_or_else(|_| default.to_string())
}

pub fn server_info(target: DatabaseTarget) -> Result<ServerInfo> {
    with_connection(|conn| server_info_of(conn, target))
}

/// DuckDB table-function reader for a data file extension, if supported.
fn data_file_reader(path: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())?;
    match ext.as_str() {
        "csv" | "tsv" | "txt" => Some("read_csv_auto"),
        "parquet" => Some("read_parquet"),
        "json" | "ndjson" | "jsonl" => Some("read_json_auto"),
        _ => None,
    }
}

/// Whether `path` is a data file that can be attached as a view.
pub fn is_data_file(path: &str) -> bool {
    data_file_reader(path).is_some()
}

/// Coarse kind label for a data file extension, stored in the registry.
pub fn data_file_kind(path: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())?;
    match ext.as_str() {
        "csv" | "tsv" | "txt" => Some("csv"),
        "parquet" => Some("parquet"),
        "json" | "ndjson" | "jsonl" => Some("json"),
        _ => None,
    }
}

/// Expose a CSV/TSV/Parquet/JSON file as a view in the current connection.
/// The view is named after the file stem, with a numeric suffix when another
/// file already claimed that name. Returns the view name.
pub fn attach_data_file(path: &str) -> Result<String> {
    with_connection(|conn| attach_data_file_of(conn, path))
}

pub fn attach_data_file_of(conn: &Connection, path: &str) -> Result<String> {
    let name = available_view_name_of(conn, &view_name_for(path)?);
    attach_data_file_as_of(conn, path, &name)?;
    Ok(name)
}

/// Expose the file under an explicit view name, replacing a view of that name.
/// Used where the name is already known — re-attaching a registered file must
/// keep the name the sidebar shows for it.
pub fn attach_data_file_as(path: &str, view_name: &str) -> Result<()> {
    with_connection(|conn| attach_data_file_as_of(conn, path, view_name))
}

pub fn attach_data_file_as_of(conn: &Connection, path: &str, view_name: &str) -> Result<()> {
    let expanded = expand_tilde(path);
    let file = std::path::Path::new(&expanded);
    if !file.exists() {
        return Err(anyhow!(trf("error.file_not_found", &[&expanded])));
    }
    let reader = data_file_reader(&expanded)
        .ok_or_else(|| anyhow!(trf("error.unsupported_file_type", &[&expanded])))?;
    let quoted_ident = view_name.replace('"', "\"\"");
    let quoted_path = expanded.replace('\'', "''");
    conn.execute_batch(&format!(
        "CREATE OR REPLACE VIEW \"{quoted_ident}\" AS SELECT * FROM {reader}('{quoted_path}')"
    ))?;
    Ok(())
}

/// The view name a file is known by: its stem.
pub fn view_name_for(path: &str) -> Result<String> {
    let expanded = expand_tilde(path);
    std::path::Path::new(&expanded)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| anyhow!(trf("error.view_name_derivation", &[&expanded])))
}

/// `stem`, or `stem_2`, `stem_3`… when the connection already has a relation by
/// that name. Two directories can hold files with the same stem, and the second
/// one must not silently replace the first.
pub fn available_view_name_of(conn: &Connection, stem: &str) -> String {
    let taken = relation_names_of(conn).unwrap_or_default();
    if !taken.contains(&stem.to_lowercase()) {
        return stem.to_string();
    }
    (2..)
        .map(|n| format!("{stem}_{n}"))
        .find(|name| !taken.contains(&name.to_lowercase()))
        .unwrap_or_else(|| stem.to_string())
}

/// Lower-cased names of every table and view in the current database. DuckDB
/// resolves identifiers case-insensitively and keeps both in one namespace, so
/// a view cannot take a table's name either — and the views it reports as
/// `system` or under a catalog schema are not the user's to collide with.
fn relation_names_of(conn: &Connection) -> Result<HashSet<String>> {
    let mut names = HashSet::new();
    for (function, column) in [
        ("duckdb_tables()", "table_name"),
        ("duckdb_views()", "view_name"),
    ] {
        let mut stmt = conn.prepare(&format!(
            "SELECT {column} FROM {function}
             WHERE database_name != 'system'
               AND schema_name NOT IN ('information_schema', 'pg_catalog', 'system')"
        ))?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            names.insert(row?.to_lowercase());
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_connection_roundtrip() {
        let _guard = connection_guard();
        open_memory().unwrap();
        assert!(is_connected());
        let version = with_connection(|conn| {
            conn.query_row("SELECT 1 + 1", [], |r| r.get::<_, i64>(0))
                .map_err(Into::into)
        })
        .unwrap();
        assert_eq!(version, 2);
        close().unwrap();
        assert!(!is_connected());
    }

    #[test]
    fn tilde_expansion() {
        let expanded = expand_tilde("~/data/x.duckdb");
        assert!(!expanded.starts_with("~"));
        assert!(expanded.ends_with("/data/x.duckdb"));
    }

    #[test]
    fn attach_csv_creates_queryable_view() {
        let conn = Connection::open_in_memory().unwrap();
        let csv = std::env::temp_dir().join("ducklocal_attach_test.csv");
        std::fs::write(&csv, "city,amount\n北京,10\n上海,20\n").unwrap();
        let view = attach_data_file_of(&conn, csv.to_str().unwrap()).unwrap();
        assert_eq!(view, "ducklocal_attach_test");
        let total: i64 = conn
            .query_row("SELECT sum(amount) FROM ducklocal_attach_test", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(total, 30);
        std::fs::remove_file(&csv).ok();
    }

    #[test]
    fn data_file_detection() {
        assert!(is_data_file("/tmp/a.csv"));
        assert!(is_data_file("/tmp/a.PARQUET".to_lowercase().as_str()));
        assert!(is_data_file("/tmp/a.ndjson"));
        assert!(!is_data_file("/tmp/a.duckdb"));
        assert!(!is_data_file("/tmp/a"));
    }

    #[test]
    fn same_stem_in_two_directories_gets_two_view_names() {
        let conn = Connection::open_in_memory().unwrap();
        let root = std::env::temp_dir().join("ducklocal_view_name_test");
        std::fs::remove_dir_all(&root).ok();
        let first = root.join("data/events.csv");
        let second = root.join("logs/events.csv");
        for path in [&first, &second] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "n\n1\n").unwrap();
        }

        let first_view = attach_data_file_of(&conn, first.to_str().unwrap()).unwrap();
        let second_view = attach_data_file_of(&conn, second.to_str().unwrap()).unwrap();

        assert_eq!(first_view, "events");
        assert_eq!(second_view, "events_2");
        assert_eq!(user_view_count(&conn), 2);

        // Re-attaching by its registered name replaces that view instead of
        // claiming a third name.
        attach_data_file_as_of(&conn, second.to_str().unwrap(), "events_2").unwrap();
        assert_eq!(user_view_count(&conn), 2);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_table_keeps_its_name_from_an_attached_file() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE events(n INTEGER)")
            .unwrap();
        let root = std::env::temp_dir().join("ducklocal_view_name_table_test");
        std::fs::remove_dir_all(&root).ok();
        let csv = root.join("events.csv");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&csv, "n\n1\n").unwrap();

        let view = attach_data_file_of(&conn, csv.to_str().unwrap()).unwrap();

        assert_eq!(view, "events_2");
        assert_eq!(user_view_count(&conn), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    /// Non-system views only: `duckdb_views()` also lists the catalog's own.
    fn user_view_count(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT count(*) FROM duckdb_views()
             WHERE database_name != 'system'
               AND schema_name NOT IN ('information_schema', 'pg_catalog', 'system')",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }
}
