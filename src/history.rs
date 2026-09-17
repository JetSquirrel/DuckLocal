//! Query history persistence, stored in its own DuckDB file under the app
//! data directory so it survives which database the user connects to.
//! Blocking functions; call via `smol::unblock` from UI code.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use duckdb::{Connection, OptionalExt};
use lazy_static::lazy_static;

const SCHEMA_VERSION: i64 = 3;

lazy_static! {
    static ref HISTORY_CONNECTION: Arc<Mutex<Option<Connection>>> = Arc::new(Mutex::new(None));
}

#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub id: i64,
    pub sql: String,
    pub started_at: String,
    pub duration_ms: i64,
    pub row_count: Option<i64>,
    pub ok: bool,
    pub error: Option<String>,
}

/// A data file registered in the "本地文件" sidebar group. The file itself is
/// re-attached as a view into every new connection.
#[derive(Clone, Debug)]
pub struct AttachedFile {
    pub id: i64,
    pub path: String,
    pub view_name: String,
    pub kind: String,
    pub attached_at: String,
}

fn history_path() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "DuckLocal")
        .ok_or_else(|| anyhow!("Cannot resolve app data directory"))?;
    let data_dir = dirs.data_dir();
    std::fs::create_dir_all(data_dir)?;
    Ok(data_dir.join("history.duckdb"))
}

/// Open (and migrate) the history database. Called once at startup.
pub fn init() -> Result<()> {
    let path = history_path()?;
    let conn = Connection::open(path)?;
    prepare_schema(&conn)?;
    let mut guard = HISTORY_CONNECTION
        .lock()
        .map_err(|e| anyhow!("History lock poisoned: {e}"))?;
    *guard = Some(conn);
    Ok(())
}

#[cfg(test)]
pub(crate) fn with_test_history(f: impl FnOnce()) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            *HISTORY_CONNECTION.lock().unwrap() = None;
        }
    }
    let conn = Connection::open_in_memory().unwrap();
    prepare_schema(&conn).unwrap();
    *HISTORY_CONNECTION.lock().unwrap() = Some(conn);
    let _reset = Reset;
    f();
}

fn prepare_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_version(version BIGINT NOT NULL);")?;
    let version: i64 = conn
        .query_row("SELECT max(version) FROM schema_version", [], |r| {
            r.get::<_, Option<i64>>(0)
        })?
        .unwrap_or(0);

    if version < 1 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS query_history(
                id BIGINT PRIMARY KEY,
                sql VARCHAR NOT NULL,
                started_at VARCHAR NOT NULL,
                duration_ms BIGINT NOT NULL,
                row_count BIGINT,
                ok BOOLEAN NOT NULL,
                error VARCHAR
            );
            CREATE SEQUENCE IF NOT EXISTS query_history_id START 1;",
        )?;
        conn.execute("INSERT INTO schema_version(version) VALUES (?1)", [1])?;
    }
    if version < 2 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS attached_files(
                id BIGINT PRIMARY KEY,
                path VARCHAR NOT NULL,
                view_name VARCHAR NOT NULL,
                kind VARCHAR NOT NULL,
                attached_at VARCHAR NOT NULL
            );
            CREATE SEQUENCE IF NOT EXISTS attached_files_id START 1;",
        )?;
        conn.execute("INSERT INTO schema_version(version) VALUES (?1)", [2])?;
    }
    if version < 3 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings(
                key VARCHAR PRIMARY KEY,
                value VARCHAR
            );",
        )?;
        conn.execute(
            "INSERT INTO schema_version(version) VALUES (?1)",
            [SCHEMA_VERSION],
        )?;
    }
    Ok(())
}

fn with_connection<T>(f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let guard = HISTORY_CONNECTION
        .lock()
        .map_err(|e| anyhow!("History lock poisoned: {e}"))?;
    let conn = guard
        .as_ref()
        .ok_or_else(|| anyhow!("History database not initialized"))?;
    f(conn)
}

pub fn record(entry: &HistoryEntry) -> Result<()> {
    with_connection(|conn| record_to(conn, entry))
}

pub fn record_to(conn: &Connection, entry: &HistoryEntry) -> Result<()> {
    conn.execute(
        "INSERT INTO query_history(id, sql, started_at, duration_ms, row_count, ok, error)
         VALUES (nextval('query_history_id'), ?1, ?2, ?3, ?4, ?5, ?6)",
        duckdb::params![
            entry.sql,
            entry.started_at,
            entry.duration_ms,
            entry.row_count,
            entry.ok,
            entry.error,
        ],
    )?;
    Ok(())
}

/// Most recent first.
pub fn recent(limit: usize) -> Result<Vec<HistoryEntry>> {
    with_connection(|conn| recent_of(conn, limit))
}

pub fn recent_of(conn: &Connection, limit: usize) -> Result<Vec<HistoryEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, sql, started_at, duration_ms, row_count, ok, error
         FROM query_history ORDER BY id DESC LIMIT ?1",
    )?;
    let entries = stmt
        .query_map([limit as i64], |row| {
            Ok(HistoryEntry {
                id: row.get(0)?,
                sql: row.get(1)?,
                started_at: row.get(2)?,
                duration_ms: row.get(3)?,
                row_count: row.get(4)?,
                ok: row.get(5)?,
                error: row.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(entries)
}

pub fn now_timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Register a data file (replacing any previous registration of the same
/// path). Called by `state::attach_data_files` the first time a file is
/// attached; later opens reuse the registration, so the sidebar keeps its
/// order instead of moving the file to the end.
pub fn register_attached_file(path: &str, view_name: &str, kind: &str) -> Result<()> {
    with_connection(|conn| register_attached_file_to(conn, path, view_name, kind))
}

pub fn register_attached_file_to(
    conn: &Connection,
    path: &str,
    view_name: &str,
    kind: &str,
) -> Result<()> {
    conn.execute("DELETE FROM attached_files WHERE path = ?1", [path])?;
    conn.execute(
        "INSERT INTO attached_files(id, path, view_name, kind, attached_at)
         VALUES (nextval('attached_files_id'), ?1, ?2, ?3, ?4)",
        duckdb::params![path, view_name, kind, now_timestamp()],
    )?;
    Ok(())
}

/// Registration order (oldest first), matching the sidebar display order.
pub fn attached_files() -> Result<Vec<AttachedFile>> {
    with_connection(attached_files_of)
}

pub fn attached_files_of(conn: &Connection) -> Result<Vec<AttachedFile>> {
    let mut stmt = conn
        .prepare("SELECT id, path, view_name, kind, attached_at FROM attached_files ORDER BY id")?;
    let files = stmt
        .query_map([], |row| {
            Ok(AttachedFile {
                id: row.get(0)?,
                path: row.get(1)?,
                view_name: row.get(2)?,
                kind: row.get(3)?,
                attached_at: row.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(files)
}

pub fn remove_attached_file(id: i64) -> Result<()> {
    with_connection(|conn| remove_attached_file_of(conn, id))
}

pub fn remove_attached_file_of(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM attached_files WHERE id = ?1", [id])?;
    Ok(())
}

/// Read a persisted setting (`None` when the key was never set).
pub fn get_setting(key: &str) -> Result<Option<String>> {
    with_connection(|conn| get_setting_of(conn, key))
}

pub fn get_setting_of(conn: &Connection, key: &str) -> Result<Option<String>> {
    let value = conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
            r.get::<_, String>(0)
        })
        .optional()?;
    Ok(value)
}

/// Persist a setting (replacing any previous value of the same key).
pub fn set_setting(key: &str, value: &str) -> Result<()> {
    with_connection(|conn| set_setting_of(conn, key, value))
}

pub fn set_setting_of(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute("DELETE FROM settings WHERE key = ?1", [key])?;
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2)",
        duckdb::params![key, value],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        prepare_schema(&conn).unwrap();
        record_to(
            &conn,
            &HistoryEntry {
                id: 0,
                sql: "SELECT 1".into(),
                started_at: "2026-09-10 09:41:22".into(),
                duration_ms: 12,
                row_count: Some(1),
                ok: true,
                error: None,
            },
        )
        .unwrap();
        record_to(
            &conn,
            &HistoryEntry {
                id: 0,
                sql: "SELECT broken".into(),
                started_at: "2026-09-10 09:42:00".into(),
                duration_ms: 3,
                row_count: None,
                ok: false,
                error: Some("parser error".into()),
            },
        )
        .unwrap();
        let entries = recent_of(&conn, 10).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].sql, "SELECT broken");
        assert!(!entries[0].ok);
        assert_eq!(entries[1].row_count, Some(1));
    }

    #[test]
    fn attached_files_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        prepare_schema(&conn).unwrap();
        register_attached_file_to(&conn, "~/data/orders.parquet", "orders", "parquet").unwrap();
        register_attached_file_to(&conn, "~/data/customers.csv", "customers", "csv").unwrap();
        // Re-registering the same path replaces the old row.
        register_attached_file_to(&conn, "~/data/orders.parquet", "orders", "parquet").unwrap();

        let files = attached_files_of(&conn).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].view_name, "customers");
        assert_eq!(files[1].view_name, "orders");
        assert_eq!(files[1].kind, "parquet");
        assert!(!files[1].attached_at.is_empty());

        remove_attached_file_of(&conn, files[0].id).unwrap();
        let files = attached_files_of(&conn).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].view_name, "orders");
    }

    #[test]
    fn prepare_schema_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        prepare_schema(&conn).unwrap();
        prepare_schema(&conn).unwrap();
        register_attached_file_to(&conn, "/tmp/a.csv", "a", "csv").unwrap();
        assert_eq!(attached_files_of(&conn).unwrap().len(), 1);
    }

    #[test]
    fn settings_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        prepare_schema(&conn).unwrap();
        assert_eq!(get_setting_of(&conn, "language").unwrap(), None);
        set_setting_of(&conn, "language", "en").unwrap();
        assert_eq!(
            get_setting_of(&conn, "language").unwrap(),
            Some("en".into())
        );
        // Re-setting the same key replaces the old value.
        set_setting_of(&conn, "language", "zh").unwrap();
        assert_eq!(
            get_setting_of(&conn, "language").unwrap(),
            Some("zh".into())
        );
    }
}
