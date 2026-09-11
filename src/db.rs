//! DuckDB connection management.
//!
//! Single global connection behind a mutex: `duckdb::Connection` is `Send`
//! but not `Sync`, so all access serializes through `with_connection`.
//! All functions here are blocking; UI code must call them via `smol::unblock`.

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use duckdb::Connection;
use lazy_static::lazy_static;

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
    HOME.get_or_init(|| std::env::var("HOME").ok())
        .as_deref()
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
/// Returns the view name (the file stem).
pub fn attach_data_file(path: &str) -> Result<String> {
    with_connection(|conn| attach_data_file_of(conn, path))
}

pub fn attach_data_file_of(conn: &Connection, path: &str) -> Result<String> {
    let expanded = expand_tilde(path);
    let file = std::path::Path::new(&expanded);
    if !file.exists() {
        return Err(anyhow!("文件不存在: {expanded}"));
    }
    let reader = data_file_reader(&expanded)
        .ok_or_else(|| anyhow!("不支持的文件类型: {expanded}"))?;
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("无法从路径推导视图名: {expanded}"))?;
    let quoted_ident = stem.replace('"', "\"\"");
    let quoted_path = expanded.replace('\'', "''");
    conn.execute_batch(&format!(
        "CREATE OR REPLACE VIEW \"{quoted_ident}\" AS SELECT * FROM {reader}('{quoted_path}')"
    ))?;
    Ok(stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_connection_roundtrip() {
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
}
