//! DuckDB connection management.
//!
//! One global connection behind a mutex: `duckdb::Connection` is `Send` but
//! not `Sync`, so all access serializes through `with_connection`. Analysis
//! apps take a second connection to the same database instead — see
//! [`APP_CONNECTION`] — so an app's SQL and the window's do not wait on each
//! other.
//! All functions here are blocking; UI code must call them via `smol::unblock`.

use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::{anyhow, Result};
use duckdb::{Connection, OptionalExt};

use crate::i18n::trf;

static CONNECTION: LazyLock<Arc<Mutex<Option<Connection>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(None)));

/// The connection analysis apps run their SQL on.
///
/// A second connection to the same database, not a second database: DuckDB
/// serves many connections from one instance, so an app sees the catalog
/// the window sees — the views DuckLocal registers included — without
/// taking the lock the window's own queries take. Sharing the one
/// connection meant a dashboard refreshing six statements held that lock
/// for as long as it ran, and the SQL editor was frozen for exactly that
/// long.
///
/// What a second connection does not carry is connection-local state:
/// `TEMP` tables and `SET` values belong to the connection they were made
/// on. All apps share this one, so they still serialize among
/// themselves — a host function is handed arguments, not a caller, so
/// nothing at the moment a query runs says which app asked.
///
/// It is cloned on demand and dropped by [`replace`], which is what keeps a
/// closed database closed: a clone left behind would answer queries against
/// a database the window has let go of, and for a file would hold it open.
static APP_CONNECTION: LazyLock<Mutex<Option<Connection>>> = LazyLock::new(|| Mutex::new(None));

/// The window connection's interrupt handle, kept apart from [`CONNECTION`]:
/// a running query holds that lock for as long as it runs, so a Stop button
/// that had to take it would wait for the very query it means to stop.
static INTERRUPT: LazyLock<Mutex<Option<Arc<duckdb::InterruptHandle>>>> =
    LazyLock::new(|| Mutex::new(None));

/// Interrupt whatever the window's connection is running. The query fails
/// with DuckDB's "Interrupted" error; with nothing running, nothing happens.
pub fn interrupt() {
    if let Ok(handle) = INTERRUPT.lock() {
        if let Some(handle) = handle.as_ref() {
            handle.interrupt();
        }
    }
}

/// How the current database was opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DatabaseTarget {
    File(String),
    Memory,
}

impl DatabaseTarget {
    /// The database's path, for the chrome to name — `None` in memory, where
    /// `:memory:` is jargon for the default and says nothing worth a glance.
    pub fn file_label(&self) -> Option<String> {
        match self {
            DatabaseTarget::File(path) => Some(compact_home(path)),
            DatabaseTarget::Memory => None,
        }
    }
}

/// `$HOME`, read once. `file_label` runs on every frame of both the title
/// bar and the status bar, so it should not go back to the environment each
/// time. Windows does not set `HOME` outside a Unix-style shell; its home is
/// `USERPROFILE`.
fn home() -> Option<&'static str> {
    static HOME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()
            .map(|home| home.trim_end_matches(std::path::is_separator).to_string())
            .filter(|home| !home.is_empty())
    })
    .as_deref()
}

/// Display `$HOME` as `~`.
pub(crate) fn compact_home(path: &str) -> String {
    match home() {
        Some(home) => compact_home_under(path, home),
        None => path.to_string(),
    }
}

/// `path` with a leading `home` shown as `~` — only at a path boundary, so a
/// home of `/Users/al` leaves `/Users/alice/x` alone.
fn compact_home_under(path: &str, home: &str) -> String {
    match path.strip_prefix(home) {
        Some(rest) if rest.is_empty() || rest.starts_with(std::path::is_separator) => {
            format!("~{rest}")
        }
        _ => path.to_string(),
    }
}

/// Replace a `~/` prefix (or `~\` on Windows) with `$HOME` expanded.
pub fn expand_tilde(path: &str) -> String {
    let mut chars = path.chars();
    if chars.next() == Some('~') && chars.next().is_some_and(std::path::is_separator) {
        if let Some(home) = home() {
            return format!("{home}{}", &path[1..]);
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
    #[cfg(windows)]
    let opened = open_file_on_windows(&expanded);
    #[cfg(not(windows))]
    let opened = Connection::open(&expanded).map_err(anyhow::Error::from);
    let connection = opened.map_err(|e| {
        anyhow!(crate::storage::explain_error(
            &expanded,
            e,
            crate::i18n::current()
        ))
    })?;
    replace(Some(connection))
}

/// Windows denies a second instance access to a file held by our own
/// database. Reuse the primary instance, or release a matching attachment
/// before opening it as primary. Keep the old primary alive until success.
#[cfg(windows)]
fn open_file_on_windows(path: &str) -> Result<Connection> {
    let mut app = app_lock()?;
    let guard = lock()?;
    let Some(conn) = guard.as_ref() else {
        return Ok(Connection::open(path)?);
    };
    let canonical = crate::sources::canonical_file_path(path).ok();
    let databases = {
        let mut stmt = conn.prepare(
            "SELECT database_name, path, readonly FROM duckdb_databases() \
             WHERE NOT internal ORDER BY database_oid",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(DatabaseAttachment {
                alias: r.get(0)?,
                path: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                read_only: r.get(2)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let existing = databases.iter().position(|database| {
        canonical.is_some() && crate::sources::canonical_file_path(&database.path).ok() == canonical
    });
    match existing {
        Some(0) => Ok(conn.try_clone()?),
        Some(ix) => {
            let attachment = &databases[ix];
            let current: String = conn.query_row("SELECT current_database()", [], |r| r.get(0))?;
            // Wait for app queries (lock order: app, then window), and release
            // their cloned connection before releasing the attached file.
            *app = None;
            detach_database_of(conn, &attachment.alias)?;
            match Connection::open(path) {
                Ok(opened) => Ok(opened),
                Err(error) => {
                    // A failed switch must not leave the old workspace with
                    // a missing attachment or a different default database.
                    attach_database_of(conn, attachment)?;
                    conn.execute_batch(&format!("USE \"{}\"", current.replace('"', "\"\"")))?;
                    Err(error.into())
                }
            }
        }
        None => Ok(Connection::open(path)?),
    }
}

/// Open an in-memory database, replacing any current connection.
pub fn open_memory() -> Result<()> {
    replace(Some(Connection::open_in_memory()?))
}

#[cfg(test)]
pub fn close() -> Result<()> {
    replace(None)
}

/// Put an already-opened connection in place.
///
/// For a caller that opened its own — the CLI does, because it decides access
/// mode, extension auto-installation and the existence check itself, and none
/// of that belongs in a second implementation here.
pub fn install(connection: Connection) -> Result<()> {
    replace(Some(connection))
}

/// Put the process on `connection`, releasing whatever it was on.
///
/// The app connection is dropped first, and both locks are taken in that
/// order everywhere — here and in [`with_app_connection`] — so the two never
/// wait on each other in opposite directions. Opening another database while an
/// app is mid-query therefore waits for that query: a connection cannot be
/// released out from under a statement that is still running.
fn replace(connection: Option<Connection>) -> Result<()> {
    let mut app = app_lock()?;
    *app = None;
    let handle = connection.as_ref().map(Connection::interrupt_handle);
    let mut guard = lock()?;
    *guard = connection;
    if let Ok(mut interrupt) = INTERRUPT.lock() {
        *interrupt = handle;
    }
    Ok(())
}

#[cfg(test)]
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

/// Run `f` on the connection analysis apps use, cloning one if there is none.
///
/// See [`APP_CONNECTION`] for why apps do not use [`with_connection`].
/// Blocking, and the app lock is held for as long as `f` runs, so callers
/// belong off the UI thread like every other caller here.
pub fn with_app_connection<T>(f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let mut app = app_lock()?;
    if app.is_none() {
        let guard = lock()?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| anyhow!("No database connected"))?;
        *app = Some(conn.try_clone()?);
    }
    let conn = app
        .as_ref()
        .expect("an app connection was just cloned into place");
    f(conn)
}

fn lock() -> Result<std::sync::MutexGuard<'static, Option<Connection>>> {
    CONNECTION
        .lock()
        .map_err(|e| anyhow!("Database lock poisoned: {e}"))
}

fn app_lock() -> Result<std::sync::MutexGuard<'static, Option<Connection>>> {
    APP_CONNECTION
        .lock()
        .map_err(|e| anyhow!("App database lock poisoned: {e}"))
}

/// Serializes tests that use the process-global connection: `open_memory` and
/// `close` swap it out from under whoever else is running.
#[cfg(test)]
pub(crate) fn connection_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Server metadata for the status bar.
#[derive(Clone, Debug)]
pub struct ServerInfo {
    pub version: String,
    /// For a file database: the oldest DuckDB that reads it (`v1.0.0+`).
    pub storage: Option<String>,
    /// For a file database: the DuckDB that created it, where its header
    /// says.
    pub created_by: Option<String>,
}

pub fn server_info_of(conn: &Connection) -> Result<ServerInfo> {
    let version: String = conn.query_row("SELECT version()", [], |r| r.get(0))?;
    let (path, storage): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT path, tags['storage_version'] FROM duckdb_databases() \
             WHERE database_name = current_database()",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .unwrap_or_default();
    let created_by = path
        .and_then(|path| crate::storage::file_kind(&path))
        .and_then(|kind| match kind {
            crate::storage::FileKind::Duckdb { created_by, .. } => created_by,
            _ => None,
        });
    Ok(ServerInfo {
        version,
        storage,
        created_by,
    })
}

pub fn server_info() -> Result<ServerInfo> {
    with_connection(server_info_of)
}

/// DuckDB table-function reader for a data file extension, if supported.
pub(crate) fn data_file_reader(path: &str) -> Option<&'static str> {
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

/// Whether `path` is an Excel/ODS workbook, which calamine imports as tables.
pub fn is_excel_file(path: &str) -> bool {
    let ext = std::path::Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase());
    matches!(ext.as_deref(), Some("xlsx" | "xls" | "xlsb" | "ods"))
}

/// Whether `path` is a data file that can be attached.
pub fn is_data_file(path: &str) -> bool {
    data_file_reader(path).is_some() || is_excel_file(path)
}

/// Coarse kind label for a data file extension, stored in the registry.
pub fn data_file_kind(path: &str) -> Option<&'static str> {
    if is_excel_file(path) {
        return Some("excel");
    }
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

/// Expose a data file in the current connection and answer the relations it
/// created as `(sheet, name)` pairs: a CSV/TSV/Parquet/JSON file becomes one
/// view (sheet `None`); a workbook becomes one table per non-empty sheet —
/// the first sheet takes the file stem, the others `{stem}_{sheet name}` —
/// each with a numeric suffix when another file already claimed that name.
pub fn attach_data_file(path: &str) -> Result<Vec<(Option<String>, String)>> {
    with_connection(|conn| attach_data_file_of(conn, path))
}

pub fn attach_data_file_of(conn: &Connection, path: &str) -> Result<Vec<(Option<String>, String)>> {
    let expanded = expand_tilde(path);
    if is_excel_file(&expanded) {
        if !std::path::Path::new(&expanded).exists() {
            return Err(anyhow!(trf("error.file_not_found", &[&expanded])));
        }
        let stem = view_name_for(&expanded)?;
        let mut created = Vec::new();
        for (ix, sheet) in crate::excel::sheets(&expanded)?.iter().enumerate() {
            let base = if ix == 0 {
                stem.clone()
            } else {
                format!("{stem}_{sheet}")
            };
            let name = available_view_name_of(conn, &base);
            if crate::excel::attach_sheet(conn, &expanded, sheet, &name)? {
                created.push((Some(sheet.clone()), name));
            }
        }
        if created.is_empty() {
            return Err(anyhow!(trf("error.excel_empty", &[&expanded])));
        }
        return Ok(created);
    }
    let name = available_view_name_of(conn, &view_name_for(&expanded)?);
    attach_data_file_as_of(conn, &expanded, &name)?;
    Ok(vec![(None, name)])
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
    ensure_replaceable_of(conn, &expanded, view_name)?;
    let quoted_ident = view_name.replace('"', "\"\"");
    let quoted_path = expanded.replace('\'', "''");
    conn.execute_batch(&format!(
        "CREATE OR REPLACE VIEW \"{quoted_ident}\" AS SELECT * FROM {reader}('{quoted_path}');
         COMMENT ON VIEW \"{quoted_ident}\" IS '{OWNED_COMMENT}';"
    ))?;
    Ok(())
}

/// The catalog comment on every table and view DuckLocal creates from a file.
/// It is how a later re-attach tells its own relation, which it may replace,
/// from one the user made under the same name, which it must not.
pub(crate) const OWNED_COMMENT: &str = "ducklocal: attached file";

/// Refuse when `name` is taken in the current schema by a relation DuckLocal
/// did not create. Registered files are re-attached into whatever database is
/// open, and a `CREATE OR REPLACE` there would drop the user's own `orders`
/// table for a workbook's sheet — and in a file database, save that.
pub(crate) fn ensure_replaceable_of(conn: &Connection, path: &str, name: &str) -> Result<()> {
    let foreign: Option<String> = conn
        .query_row(
            "SELECT name FROM (
                 SELECT table_name AS name, comment FROM duckdb_tables()
                 WHERE database_name = current_database() AND schema_name = current_schema()
                 UNION ALL
                 SELECT view_name, comment FROM duckdb_views()
                 WHERE database_name = current_database() AND schema_name = current_schema()
                   AND NOT internal
             )
             WHERE lower(name) = lower(?1) AND coalesce(comment, '') != ?2
             LIMIT 1",
            [name, OWNED_COMMENT],
            |row| row.get(0),
        )
        .optional()?;
    match foreign {
        Some(existing) => Err(anyhow!(trf("error.relation_taken", &[path, &existing]))),
        None => Ok(()),
    }
}

/// Re-import one sheet of a workbook under an explicit relation name; `sheet`
/// of `None` means the first sheet. Used where the name is already known —
/// re-attaching a registered file must keep the name the sidebar shows for it.
pub fn attach_excel_sheet_as(path: &str, sheet: Option<&str>, view_name: &str) -> Result<()> {
    with_connection(|conn| attach_excel_sheet_as_of(conn, path, sheet, view_name))
}

pub fn attach_excel_sheet_as_of(
    conn: &Connection,
    path: &str,
    sheet: Option<&str>,
    view_name: &str,
) -> Result<()> {
    let expanded = expand_tilde(path);
    if !std::path::Path::new(&expanded).exists() {
        return Err(anyhow!(trf("error.file_not_found", &[&expanded])));
    }
    let sheet = match sheet {
        Some(sheet) => sheet.to_string(),
        None => crate::excel::sheets(&expanded)?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!(trf("error.excel_empty", &[&expanded])))?,
    };
    crate::excel::attach_sheet(conn, &expanded, &sheet, view_name)?;
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

/// A database attached beside the open one: its alias and how it was opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseAttachment {
    pub path: String,
    pub alias: String,
    pub read_only: bool,
}

/// An alias for the database at `path`: its file stem as a plain identifier
/// (`2026-10 logs.duckdb` → `db_2026_10_logs`), with a numeric suffix when
/// the connection already has a database by that name.
pub fn database_alias_of(conn: &Connection, path: &str) -> Result<String> {
    let stem = view_name_for(path)?;
    let mut alias: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if !alias.starts_with(|c: char| c.is_ascii_alphabetic()) {
        alias = format!("db_{alias}");
    }
    let mut stmt = conn.prepare("SELECT lower(database_name) FROM duckdb_databases()")?;
    let taken: HashSet<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<_, _>>()?;
    // `main` is not a database but names the default schema; an alias by
    // that name would make `main.t` mean two things.
    let free = |name: &str| !taken.contains(&name.to_lowercase()) && name != "main";
    if free(&alias) {
        return Ok(alias);
    }
    Ok((2..)
        .map(|n| format!("{alias}_{n}"))
        .find(|name| free(name))
        .expect("an unbounded range has a free name"))
}

/// `ATTACH` the database at `path` under `alias`. A SQLite file is attached
/// through the sqlite extension; anything else is taken for DuckDB, which
/// creates the file when it does not exist yet (and refuses to, read-only).
pub fn attach_database_of(conn: &Connection, attachment: &DatabaseAttachment) -> Result<()> {
    let expanded = expand_tilde(&attachment.path);
    let mut options = Vec::new();
    if crate::storage::file_kind(&expanded) == Some(crate::storage::FileKind::Sqlite) {
        options.push("TYPE sqlite");
    }
    if attachment.read_only {
        options.push("READ_ONLY");
    }
    let options = if options.is_empty() {
        String::new()
    } else {
        format!(" ({})", options.join(", "))
    };
    let sql = format!(
        "ATTACH '{}' AS \"{}\"{options}",
        expanded.replace('\'', "''"),
        attachment.alias.replace('"', "\"\""),
    );
    conn.execute_batch(&sql).map_err(|e| {
        anyhow!(crate::storage::explain_error(
            &expanded,
            e,
            crate::i18n::current()
        ))
    })
}

/// `DETACH` a database. If it is where unqualified names resolve, the session
/// moves back to the open database first: DuckDB will not detach the
/// database in use.
pub fn detach_database_of(conn: &Connection, alias: &str) -> Result<()> {
    let (current, default): (String, Option<String>) = conn.query_row(
        "SELECT current_database(),
                (SELECT database_name FROM duckdb_databases()
                 WHERE NOT internal AND database_name != ?1
                 ORDER BY database_oid LIMIT 1)",
        [alias],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let quoted = alias.replace('"', "\"\"");
    if current.eq_ignore_ascii_case(alias) {
        let default = default.ok_or_else(|| anyhow!("No other database to switch to"))?;
        conn.execute_batch(&format!("USE \"{}\"", default.replace('"', "\"\"")))?;
    }
    conn.execute_batch(&format!("DETACH \"{quoted}\""))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn databases_attach_under_a_free_alias_and_detach() {
        let conn = Connection::open_in_memory().unwrap();
        let dir = std::env::temp_dir().join("ducklocal_attach_databases");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("2026-10 logs.duckdb");
        let path = path.to_str().unwrap();

        let alias = database_alias_of(&conn, path).unwrap();
        assert_eq!(alias, "db_2026_10_logs");
        let attachment = DatabaseAttachment {
            path: path.to_string(),
            alias: alias.clone(),
            read_only: false,
        };
        attach_database_of(&conn, &attachment).unwrap();
        conn.execute_batch(&format!("CREATE TABLE {alias}.events AS SELECT 1 AS id"))
            .unwrap();
        // Taken now: the next one gets a suffix.
        assert_eq!(database_alias_of(&conn, path).unwrap(), "db_2026_10_logs_2");

        // Detaching the database in use moves the session off it first.
        conn.execute_batch(&format!("USE {alias}")).unwrap();
        detach_database_of(&conn, &alias).unwrap();
        let current: String = conn
            .query_row("SELECT current_database()", [], |r| r.get(0))
            .unwrap();
        assert_eq!(current, "memory");

        // Read-only, it reads and refuses writes.
        attach_database_of(
            &conn,
            &DatabaseAttachment {
                read_only: true,
                ..attachment
            },
        )
        .unwrap();
        let n: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {alias}.events"), [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 1);
        assert!(conn
            .execute_batch(&format!("INSERT INTO {alias}.events VALUES (2)"))
            .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    #[cfg(windows)]
    #[test]
    fn reopening_primary_database_reuses_its_instance() {
        let _guard = connection_guard();
        let path = std::env::temp_dir().join(format!(
            "ducklocal_primary_reopen_{}.duckdb",
            std::process::id()
        ));
        let path = path.to_str().unwrap();
        open_file(path).unwrap();
        with_connection(|conn| {
            conn.execute_batch("CREATE OR REPLACE TABLE retained AS SELECT 42 AS answer")?;
            Ok(())
        })
        .unwrap();
        open_file(path).unwrap();
        let answer: i64 = with_connection(|conn| {
            Ok(conn.query_row("SELECT answer FROM retained", [], |row| row.get(0))?)
        })
        .unwrap();
        assert_eq!(answer, 42);
        close().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn failed_attachment_switch_restores_the_old_workspace() {
        let _guard = connection_guard();
        let path = std::env::temp_dir().join(format!(
            "ducklocal_readonly_switch_{}.duckdb",
            std::process::id()
        ));
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("CREATE OR REPLACE TABLE retained AS SELECT 42 AS answer")
                .unwrap();
        }
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions.clone()).unwrap();
        open_memory().unwrap();
        with_connection(|conn| {
            attach_database_of(
                conn,
                &DatabaseAttachment {
                    path: path.to_string_lossy().into_owned(),
                    alias: "logs".into(),
                    read_only: true,
                },
            )?;
            conn.execute_batch("USE logs")?;
            Ok(())
        })
        .unwrap();
        // The Windows read-only attribute prevents opening this as writable
        // primary, while the original read-only attachment can be restored.
        assert!(open_file(path.to_str().unwrap()).is_err());
        let (current, answer): (String, i64) = with_connection(|conn| {
            Ok(conn.query_row(
                "SELECT current_database(), answer FROM retained",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .unwrap();
        assert_eq!(current, "logs");
        assert_eq!(answer, 42);
        close().unwrap();
        permissions.set_readonly(false);
        std::fs::set_permissions(&path, permissions).unwrap();
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn interrupt_stops_a_running_query_without_its_lock() {
        let _guard = connection_guard();
        open_memory().unwrap();
        let running = std::thread::spawn(|| {
            // A cross join that would run for a very long time.
            crate::query::run("SELECT count(*) FROM range(1000000000) a, range(1000000000) b")
        });
        // The query holds the connection lock the whole time; interrupting
        // must not need it.
        let started = std::time::Instant::now();
        while !running.is_finished() && started.elapsed().as_secs() < 20 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            interrupt();
        }
        let outcome = running.join().unwrap();
        let error = outcome.expect_err("the query should have been interrupted");
        assert!(error.to_string().contains("nterrupt"), "{error}");
        close().unwrap();
    }

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
    fn home_is_compacted_only_at_a_path_boundary() {
        assert_eq!(
            compact_home_under("/Users/al/x.csv", "/Users/al"),
            "~/x.csv"
        );
        assert_eq!(compact_home_under("/Users/al", "/Users/al"), "~");
        assert_eq!(
            compact_home_under("/Users/alice/x", "/Users/al"),
            "/Users/alice/x"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_home_is_compacted_at_a_backslash() {
        assert_eq!(
            compact_home_under(r"C:\Users\al\x.csv", r"C:\Users\al"),
            r"~\x.csv"
        );
        assert_eq!(
            compact_home_under(r"C:\Users\alice\x", r"C:\Users\al"),
            r"C:\Users\alice\x"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_tilde_expansion_takes_a_backslash() {
        let expanded = expand_tilde(r"~\data\x.duckdb");
        assert!(!expanded.starts_with('~'));
        assert!(expanded.ends_with(r"\data\x.duckdb"));
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
        let view = &attach_data_file_of(&conn, csv.to_str().unwrap()).unwrap()[0].1;
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
    fn a_workbook_attaches_a_table_per_non_empty_sheet() {
        let conn = Connection::open_in_memory().unwrap();
        let path = std::env::temp_dir().join("ducklocal_attach_test.xlsx");
        std::fs::remove_file(&path).ok();
        let mut book = rust_xlsxwriter::Workbook::new();
        let orders = book.add_worksheet().set_name("Orders").unwrap();
        orders.write_string(0, 0, "n").unwrap();
        orders.write_number(1, 0, 7).unwrap();
        book.add_worksheet().set_name("Empty").unwrap();
        let extra = book.add_worksheet().set_name("Extra Sheet").unwrap();
        extra.write_string(0, 0, "s").unwrap();
        extra.write_string(1, 0, "x").unwrap();
        book.save(&path).unwrap();

        let created = attach_data_file_of(&conn, path.to_str().unwrap()).unwrap();

        assert_eq!(
            created,
            [
                (
                    Some("Orders".to_string()),
                    "ducklocal_attach_test".to_string()
                ),
                (
                    Some("Extra Sheet".to_string()),
                    "ducklocal_attach_test_Extra Sheet".to_string()
                ),
            ]
        );
        let n: i64 = conn
            .query_row("SELECT n FROM ducklocal_attach_test", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 7);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn data_file_detection() {
        assert!(is_data_file("/tmp/a.csv"));
        assert!(is_data_file("/tmp/a.PARQUET".to_lowercase().as_str()));
        assert!(is_data_file("/tmp/a.ndjson"));
        assert!(is_data_file("/tmp/a.xlsx"));
        assert!(is_data_file("/tmp/a.XLS".to_lowercase().as_str()));
        assert!(is_data_file("/tmp/a.xlsb"));
        assert!(is_data_file("/tmp/a.ods"));
        assert!(!is_data_file("/tmp/a.duckdb"));
        assert!(!is_data_file("/tmp/a"));
        assert_eq!(data_file_kind("/tmp/a.xlsx"), Some("excel"));
        assert_eq!(data_file_kind("/tmp/a.csv"), Some("csv"));
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

        let first_view = attach_data_file_of(&conn, first.to_str().unwrap()).unwrap()[0]
            .1
            .clone();
        let second_view = attach_data_file_of(&conn, second.to_str().unwrap()).unwrap()[0]
            .1
            .clone();

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

        let view = &attach_data_file_of(&conn, csv.to_str().unwrap()).unwrap()[0].1;

        assert_eq!(view, "events_2");
        assert_eq!(user_view_count(&conn), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn re_attaching_never_replaces_a_relation_the_user_made() {
        let conn = Connection::open_in_memory().unwrap();
        let root = std::env::temp_dir().join("ducklocal_reattach_foreign_test");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        let csv = root.join("orders.csv");
        std::fs::write(&csv, "n\n1\n").unwrap();
        let xlsx = root.join("orders.xlsx");
        let mut book = rust_xlsxwriter::Workbook::new();
        let sheet = book.add_worksheet().set_name("Sheet1").unwrap();
        sheet.write_string(0, 0, "n").unwrap();
        sheet.write_number(1, 0, 1).unwrap();
        book.save(&xlsx).unwrap();

        // Registered earlier as `orders`; now the open database has its own.
        conn.execute_batch("CREATE TABLE orders(id INTEGER); INSERT INTO orders VALUES (42);")
            .unwrap();
        let csv_error = attach_data_file_as_of(&conn, csv.to_str().unwrap(), "orders")
            .unwrap_err()
            .to_string();
        let sheet_error =
            attach_excel_sheet_as_of(&conn, xlsx.to_str().unwrap(), Some("Sheet1"), "Orders")
                .unwrap_err()
                .to_string();
        assert!(csv_error.contains("orders"), "{csv_error}");
        assert!(sheet_error.contains("\"orders\""), "{sheet_error}");
        let kept: i64 = conn
            .query_row("SELECT id FROM orders", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 42);

        // Its own relations DuckLocal still replaces on every re-attach.
        attach_data_file_as_of(&conn, csv.to_str().unwrap(), "events").unwrap();
        attach_data_file_as_of(&conn, csv.to_str().unwrap(), "events").unwrap();
        attach_excel_sheet_as_of(&conn, xlsx.to_str().unwrap(), Some("Sheet1"), "sheet").unwrap();
        attach_excel_sheet_as_of(&conn, xlsx.to_str().unwrap(), Some("Sheet1"), "sheet").unwrap();

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
