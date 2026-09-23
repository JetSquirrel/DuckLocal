//! Shared application state entity: connection status, catalog, history,
//! and status-bar data. Views subscribe to this entity for cross-panel refresh.

use std::rc::Rc;

use gpui_kit::{Context, EventEmitter};

use crate::db::{DatabaseTarget, ServerInfo};
use crate::history::HistoryEntry;
use crate::i18n::trf;
use crate::schema::DatabaseInfo;

#[derive(Clone, Debug)]
pub struct ConnectionChanged;

#[derive(Clone, Debug)]
pub struct HistoryChanged;

#[derive(Clone, Debug)]
pub struct QueryStatsChanged;

#[derive(Clone, Debug)]
pub struct AttachedFilesChanged;

#[derive(Clone, Debug)]
pub struct S3ConfigChanged;

#[derive(Clone, Debug)]
pub struct OpenStateChanged;

/// The recent-documents list gained an entry (an app or dashboard tab
/// opened). The sidebar rebuilds its document groups on this.
#[derive(Clone, Debug)]
pub struct RecentsChanged;

/// Status-bar summary of the most recent query run.
#[derive(Clone, Debug)]
pub struct QueryStats {
    pub elapsed_ms: u128,
    pub rows: usize,
    pub cols: usize,
}

/// A registered data file plus its cached row count for the sidebar.
#[derive(Clone, Debug)]
pub struct AttachedFileView {
    pub id: i64,
    pub path: String,
    pub view_name: String,
    /// `None` when the count has not been loaded or the query failed.
    pub row_count: Option<i64>,
}

pub struct AppState {
    pub target: Option<DatabaseTarget>,
    pub server: Option<ServerInfo>,
    pub catalog: Vec<DatabaseInfo>,
    /// Shared so a view can hold the list across a `&mut Context` borrow
    /// without copying every entry on each frame.
    pub history: Rc<Vec<HistoryEntry>>,
    pub last_query: Option<QueryStats>,
    /// Last executed query text, for export actions in the results panel.
    pub last_sql: Option<String>,
    pub attached_files: Vec<AttachedFileView>,
    /// Session-scoped S3 credentials, if configured. Memory only.
    pub s3_config: Option<crate::s3::S3Config>,
    /// Open/attach requests in flight; > 0 while a data source is being opened.
    open_requests: usize,
    /// Set once startup's open request has landed. Until then the panels have
    /// no opinion: their emptiness is "not loaded yet", not "nothing here".
    is_ready: bool,
}

impl EventEmitter<ConnectionChanged> for AppState {}
impl EventEmitter<HistoryChanged> for AppState {}
impl EventEmitter<QueryStatsChanged> for AppState {}
impl EventEmitter<AttachedFilesChanged> for AppState {}
impl EventEmitter<S3ConfigChanged> for AppState {}
impl EventEmitter<OpenStateChanged> for AppState {}
impl EventEmitter<RecentsChanged> for AppState {}

impl AppState {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            target: None,
            server: None,
            catalog: Vec::new(),
            history: Rc::new(Vec::new()),
            last_query: None,
            last_sql: None,
            attached_files: Vec::new(),
            s3_config: None,
            open_requests: 0,
            is_ready: false,
        }
    }

    /// Whether startup's open request has landed. A view that renders "there
    /// is nothing here" has to wait for it, or it shows that state while the
    /// data is still being attached.
    pub fn is_ready(&self) -> bool {
        self.is_ready
    }

    /// Whether anything is open to query: at least one attached file, or a
    /// table or view in the current connection. Drives the workspace's
    /// first-run screen.
    pub fn has_data(&self) -> bool {
        !self.attached_files.is_empty() || !self.catalog.is_empty()
    }

    /// Whether an open/attach request is in flight.
    pub fn is_opening(&self) -> bool {
        self.open_requests > 0
    }

    /// Mark the start of an open/attach request; paired with `end_open`.
    pub fn begin_open(&mut self, cx: &mut Context<Self>) {
        self.open_requests += 1;
        cx.emit(OpenStateChanged);
        cx.notify();
    }

    /// Mark the end of an open/attach request started with `begin_open`.
    pub fn end_open(&mut self, cx: &mut Context<Self>) {
        self.open_requests = self.open_requests.saturating_sub(1);
        cx.emit(OpenStateChanged);
        cx.notify();
    }

    /// Refresh catalog + history from the blocking DB layer.
    /// Call inside `smol::unblock`-spawned tasks' completion on the UI thread
    /// after the blocking load has landed.
    pub fn set_connection(
        &mut self,
        target: DatabaseTarget,
        server: ServerInfo,
        catalog: Vec<DatabaseInfo>,
        cx: &mut Context<Self>,
    ) {
        self.target = Some(target);
        self.server = Some(server);
        self.catalog = catalog;
        // Session-scoped S3 secrets live in the connection, so a new
        // connection starts unconfigured.
        self.s3_config = None;
        self.is_ready = true;
        cx.emit(ConnectionChanged);
        cx.notify();
    }

    pub fn set_attached_files(&mut self, files: Vec<AttachedFileView>, cx: &mut Context<Self>) {
        self.attached_files = files;
        cx.emit(AttachedFilesChanged);
        cx.notify();
    }

    pub fn set_s3_configured(
        &mut self,
        config: Option<crate::s3::S3Config>,
        cx: &mut Context<Self>,
    ) {
        self.s3_config = config;
        cx.emit(S3ConfigChanged);
        cx.notify();
    }

    /// Replace just the catalog (e.g. after a query that may have run DDL).
    pub fn set_catalog(&mut self, catalog: Vec<DatabaseInfo>, cx: &mut Context<Self>) {
        self.catalog = catalog;
        cx.emit(ConnectionChanged);
        cx.notify();
    }

    pub fn set_history(&mut self, history: Vec<HistoryEntry>, cx: &mut Context<Self>) {
        self.history = Rc::new(history);
        cx.emit(HistoryChanged);
        cx.notify();
    }

    pub fn set_last_query(&mut self, stats: QueryStats, sql: String, cx: &mut Context<Self>) {
        self.last_query = Some(stats);
        self.last_sql = Some(sql);
        cx.emit(QueryStatsChanged);
        cx.notify();
    }
}

/// Blocking helper: load catalog + history together off the UI thread.
pub fn load_sidebar_data() -> (Vec<DatabaseInfo>, Vec<HistoryEntry>) {
    let catalog = crate::schema::load_catalog().unwrap_or_default();
    let history = crate::history::recent(200).unwrap_or_default();
    (catalog, history)
}

/// Blocking helper: the three panel data sets, reloaded after anything changed
/// the connection or the set of attached files.
pub fn reload_workspace() -> (Vec<DatabaseInfo>, Vec<HistoryEntry>, Vec<AttachedFileView>) {
    let (catalog, history) = load_sidebar_data();
    (catalog, history, load_attached_files())
}

/// Blocking helper: re-attach every registered data file into the current
/// connection, each under the view name it was registered with. Files that no
/// longer exist are skipped (their registration is kept, so they reappear when
/// the file comes back).
pub fn reattach_registered_files() -> Vec<String> {
    reattach_files(&crate::history::attached_files().unwrap_or_default())
}

/// A registration whose path is relative was recorded against a working
/// directory the current process does not know; re-binding it to today's cwd
/// would silently attach a different file, so it is reported instead.
fn registered_path(path: &str) -> anyhow::Result<String> {
    let expanded = crate::db::expand_tilde(path);
    anyhow::ensure!(
        std::path::Path::new(&expanded).is_absolute(),
        trf("error.relative_attachment", &[path])
    );
    Ok(expanded)
}

fn reattach_files(files: &[crate::history::AttachedFile]) -> Vec<String> {
    let mut problems = Vec::new();
    for file in files {
        match registered_path(&file.path) {
            Ok(path) => {
                match &file.sheet {
                    Some(sheet) => {
                        crate::db::attach_excel_sheet_as(&path, Some(sheet), &file.view_name).ok();
                    }
                    None => {
                        crate::db::attach_data_file_as(&path, &file.view_name).ok();
                    }
                }
            }
            Err(e) => problems.push(e.to_string()),
        }
    }
    problems
}

/// What one request to open paths produced, for the UI to report and render.
#[derive(Debug, Default)]
pub struct RequestReport {
    /// Paths that could not be resolved, one message each.
    pub problems: Vec<String>,
    /// Views created, in attachment order.
    pub created: Vec<String>,
    /// Files that could not be attached, one message each.
    pub failed: Vec<String>,
    /// Set when the request carried more files than [`crate::sources::MAX_FILES`].
    pub truncated: bool,
}

/// Blocking helper: attach every path, registering the new ones so they come
/// back with the next launch. A file that is already registered is attached
/// under its registered names — a workbook has one registry row per sheet —
/// and keeps its place in the registry rather than being written again on
/// every open.
pub fn attach_data_files(paths: &[String]) -> RequestReport {
    let registered = crate::history::attached_files().unwrap_or_default();
    let mut report = RequestReport::default();
    let mut seen = std::collections::BTreeSet::new();

    for path in paths {
        let path = match crate::sources::canonical_file_path(path) {
            Ok(path) => path,
            Err(e) => {
                report
                    .failed
                    .push(trf("notify.attach.failed", &[&e.to_string()]));
                continue;
            }
        };
        if !seen.insert(path.clone()) {
            continue;
        }
        let known: Vec<_> = registered
            .iter()
            .filter(|file| {
                registered_path(&file.path)
                    .ok()
                    .and_then(|path| crate::sources::canonical_file_path(&path).ok())
                    .as_ref()
                    == Some(&path)
            })
            .collect();

        if known.is_empty() {
            match crate::db::attach_data_file(&path) {
                Ok(pairs) => {
                    if let Some(kind) = crate::db::data_file_kind(&path) {
                        crate::history::register_attached_sheets(&path, kind, &pairs).ok();
                    }
                    report.created.extend(pairs.into_iter().map(|(_, name)| name));
                }
                Err(e) => report
                    .failed
                    .push(trf("notify.attach.failed", &[&e.to_string()])),
            }
            continue;
        }

        for file in known {
            let attached = match &file.sheet {
                Some(sheet) => {
                    crate::db::attach_excel_sheet_as(&path, Some(sheet), &file.view_name)
                }
                None => crate::db::attach_data_file_as(&path, &file.view_name),
            };
            match attached {
                Ok(()) => report.created.push(file.view_name.clone()),
                Err(e) => report
                    .failed
                    .push(trf("notify.attach.failed", &[&e.to_string()])),
            }
        }
    }

    report
}

/// The panels' share of a request: everything they redraw, plus its report.
pub struct AttachOutcome {
    pub report: RequestReport,
    pub catalog: Vec<DatabaseInfo>,
    pub history: Vec<HistoryEntry>,
    pub attached: Vec<AttachedFileView>,
}

/// A request that opened a database, replacing the previous connection.
pub struct ConnectOutcome {
    pub target: DatabaseTarget,
    pub server: ServerInfo,
    /// Set when the named database could not be opened and the request fell
    /// back to the in-memory connection.
    pub database_error: Option<String>,
    pub attach: AttachOutcome,
}

/// Blocking result of a request to open paths.
pub enum OpenOutcome {
    /// A database was opened and the registered files were re-attached into it.
    Connected(ConnectOutcome),
    /// Data files were added to the connection that was already open.
    Attached(AttachOutcome),
}

/// Blocking: open whatever `paths` name.
///
/// `connect_default` is true at startup, where the request has to end with a
/// usable connection — with no database among `paths` it opens the in-memory
/// one. In a running app it is false, so a request carrying only data files
/// attaches them to the current connection instead of discarding the tables it
/// already holds.
pub fn open_request(paths: &[String], connect_default: bool) -> anyhow::Result<OpenOutcome> {
    open_sources(crate::sources::resolve(paths), connect_default)
}

pub fn open_dialog_path(path: &str) -> anyhow::Result<OpenOutcome> {
    open_sources(crate::sources::resolve_dialog_path(path), false)
}

fn open_sources(
    sources: crate::sources::Sources,
    connect_default: bool,
) -> anyhow::Result<OpenOutcome> {
    if sources.database.is_none() && !connect_default {
        return Ok(OpenOutcome::Attached(attach_outcome(&sources)));
    }

    let mut target = match &sources.database {
        Some(path) => DatabaseTarget::File(path.clone()),
        None => DatabaseTarget::Memory,
    };
    let opened = match &target {
        DatabaseTarget::File(path) => crate::db::open_file(path),
        DatabaseTarget::Memory => crate::db::open_memory(),
    };
    let database_error = match opened {
        Ok(()) => None,
        Err(e) if connect_default => {
            crate::db::open_memory()?;
            target = DatabaseTarget::Memory;
            Some(e.to_string())
        }
        Err(e) => return Err(e),
    };

    let problems = reattach_registered_files();
    let server = crate::db::server_info()?;
    let mut attach = attach_outcome(&sources);
    attach.report.problems.extend(problems);
    Ok(OpenOutcome::Connected(ConnectOutcome {
        target,
        server,
        database_error,
        attach,
    }))
}

fn attach_outcome(sources: &crate::sources::Sources) -> AttachOutcome {
    let report = attach_data_files(&sources.files);
    let (catalog, history, attached) = reload_workspace();
    AttachOutcome {
        report: RequestReport {
            problems: sources.problems.clone(),
            truncated: sources.truncated,
            ..report
        },
        catalog,
        history,
        attached,
    }
}

/// Blocking helper: registered files with row counts resolved against the
/// current connection (`None` when the count query fails).
pub fn load_attached_files() -> Vec<AttachedFileView> {
    crate::history::attached_files()
        .unwrap_or_default()
        .into_iter()
        .map(|file| {
            let row_count = registered_path(&file.path)
                .and_then(|_| {
                    crate::db::with_connection(|conn| {
                        let quoted = file.view_name.replace('"', "\"\"");
                        conn.query_row(&format!("SELECT count(*) FROM \"{quoted}\""), [], |r| {
                            r.get::<_, i64>(0)
                        })
                        .map_err(Into::into)
                    })
                })
                .ok();
            AttachedFileView {
                id: file.id,
                path: file.path,
                view_name: file.view_name,
                row_count,
            }
        })
        .collect()
}

/// Format a duration for compact display: `184 ms` / `1.36 s`.
pub fn format_duration(ms: i64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.2} s", ms as f64 / 1000.0)
    }
}

/// Format a row estimate for compact display: `318K` / `2.4M`.
pub fn format_rows(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str, body: &str) -> String {
        let dir = std::env::temp_dir().join("ducklocal_open_request_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path.to_string_lossy().to_string()
    }

    fn catalog_names(catalog: &[DatabaseInfo]) -> Vec<String> {
        catalog
            .iter()
            .flat_map(|database| database.tables.iter().map(|table| table.name.clone()))
            .collect()
    }

    #[test]
    fn a_workbook_registers_every_sheet_and_restores_them() {
        let _guard = crate::db::connection_guard();
        crate::history::with_test_history(|| {
            let path = std::env::temp_dir().join("ducklocal_state_workbook.xlsx");
            std::fs::remove_file(&path).ok();
            let mut book = rust_xlsxwriter::Workbook::new();
            let orders = book.add_worksheet().set_name("Orders").unwrap();
            orders.write_string(0, 0, "n").unwrap();
            orders.write_number(1, 0, 7).unwrap();
            let extra = book.add_worksheet().set_name("Meta").unwrap();
            extra.write_string(0, 0, "key").unwrap();
            extra.write_string(1, 0, "v").unwrap();
            book.save(&path).unwrap();
            let path = path.to_string_lossy().to_string();

            crate::db::open_memory().unwrap();
            let report = attach_data_files(std::slice::from_ref(&path));
            assert_eq!(
                report.created,
                ["ducklocal_state_workbook", "ducklocal_state_workbook_Meta"]
            );
            assert!(report.failed.is_empty());

            let registered = crate::history::attached_files().unwrap();
            assert_eq!(registered.len(), 2);
            assert_eq!(registered[0].sheet.as_deref(), Some("Orders"));
            assert_eq!(registered[1].sheet.as_deref(), Some("Meta"));

            // A repeated open re-attaches under the registered names and does
            // not write the registry again.
            let repeated = attach_data_files(std::slice::from_ref(&path));
            assert_eq!(repeated.created, report.created);
            assert_eq!(crate::history::attached_files().unwrap().len(), 2);

            // A fresh connection restores both sheets from the registry.
            crate::db::open_memory().unwrap();
            assert!(reattach_registered_files().is_empty());
            let n: i64 = crate::db::with_connection(|conn| {
                conn.query_row(
                    "SELECT (SELECT n FROM ducklocal_state_workbook) +
                            (SELECT length(key) FROM ducklocal_state_workbook_Meta)",
                    [],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
            assert_eq!(n, 8);
            crate::db::close().unwrap();
            std::fs::remove_file(&path).ok();
        });
    }

    #[test]
    fn relative_attachments_are_registered_absolutely_and_restored() {
        let _guard = crate::db::connection_guard();
        if let Ok(path) = std::env::var("DUCKLOCAL_TEST_RESTORE_PATH") {
            crate::history::with_test_history(|| {
                crate::history::register_attached_file(&path, "restored", "csv").unwrap();
                let OpenOutcome::Connected(connected) = open_request(&[], true).unwrap() else {
                    panic!("startup must connect");
                };
                assert!(connected.attach.report.problems.is_empty());
                let n: i64 = crate::db::with_connection(|conn| {
                    conn.query_row("SELECT n FROM restored", [], |r| r.get(0))
                        .map_err(Into::into)
                })
                .unwrap();
                assert_eq!(n, 42);
                crate::db::close().unwrap();
            });
            return;
        }
        crate::history::with_test_history(|| {
            let relative = format!("target/attachment-{}.csv", std::process::id());
            std::fs::create_dir_all("target").unwrap();
            std::fs::write(&relative, "n\n42\n").unwrap();
            let absolute = crate::sources::canonical_file_path(&relative).unwrap();
            crate::db::open_memory().unwrap();

            let report = attach_data_files(&[relative.clone(), absolute.clone()]);
            assert_eq!(report.created.len(), 1);
            assert!(report.failed.is_empty());
            let registered = crate::history::attached_files().unwrap();
            assert_eq!(registered.len(), 1);
            assert_eq!(registered[0].path, absolute);
            let repeated = attach_data_files(&[format!("./{relative}")]);
            assert_eq!(repeated.created, report.created);
            assert_eq!(
                crate::history::attached_files().unwrap()[0].id,
                registered[0].id
            );
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "state::tests::relative_attachments_are_registered_absolutely_and_restored",
                ])
                .current_dir("target")
                .env("DUCKLOCAL_TEST_RESTORE_PATH", &registered[0].path)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );

            crate::db::open_memory().unwrap();
            assert!(reattach_registered_files().is_empty());
            let quoted = registered[0].view_name.replace('"', "\"\"");
            let n: i64 = crate::db::with_connection(|conn| {
                conn.query_row(&format!("SELECT n FROM \"{quoted}\""), [], |r| r.get(0))
                    .map_err(Into::into)
            })
            .unwrap();
            assert_eq!(n, 42);
            crate::db::close().unwrap();
            std::fs::remove_file(relative).unwrap();
        });
    }

    #[test]
    fn legacy_relative_attachments_are_unresolved_even_if_the_path_exists() {
        let _guard = crate::db::connection_guard();
        crate::history::with_test_history(|| {
            let relative = format!("target/legacy-{}.csv", std::process::id());
            std::fs::create_dir_all("target").unwrap();
            std::fs::write(&relative, "n\n99\n").unwrap();
            crate::history::register_attached_file(&relative, "legacy", "csv").unwrap();

            let OpenOutcome::Connected(connected) = open_request(&[], true).unwrap() else {
                panic!("startup must connect");
            };
            assert_eq!(connected.attach.report.problems.len(), 1);
            assert!(connected.attach.report.problems[0].contains(&relative));
            assert!(!catalog_names(&connected.attach.catalog).contains(&"legacy".into()));
            assert_eq!(connected.attach.attached.len(), 1);
            assert_eq!(connected.attach.attached[0].row_count, None);
            crate::db::close().unwrap();
            std::fs::remove_file(relative).unwrap();
        });
    }

    #[test]
    fn explicit_dialog_path_creates_database_and_parent_directories() {
        let _guard = crate::db::connection_guard();
        let root =
            std::path::Path::new("target").join(format!("dialog-create-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let database = root.join("nested/created.duckdb");
        let path = database.to_str().unwrap();
        crate::db::open_memory().unwrap();
        let OpenOutcome::Attached(attach) = open_request(&[path.into()], false).unwrap() else {
            panic!("ordinary missing paths must not create databases");
        };
        assert_eq!(attach.report.problems.len(), 1);
        assert!(!database.exists());

        let OpenOutcome::Connected(connected) = open_dialog_path(path).unwrap() else {
            panic!("the explicit dialog path must create a database");
        };
        assert_eq!(connected.target, DatabaseTarget::File(path.into()));
        assert!(connected.database_error.is_none());
        crate::db::with_connection(|conn| {
            conn.execute_batch("CREATE TABLE created AS SELECT 7 AS n")
                .map_err(Into::into)
        })
        .unwrap();
        crate::db::close().unwrap();
        let conn = duckdb::Connection::open(&database).unwrap();
        assert_eq!(
            conn.query_row("SELECT n FROM created", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            7
        );
        drop(conn);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_data_file_joins_the_connection_without_replacing_it() {
        let _guard = crate::db::connection_guard();
        crate::db::open_memory().unwrap();
        crate::db::with_connection(|conn| {
            conn.execute_batch("CREATE TABLE keep(n INTEGER)")
                .map_err(Into::into)
        })
        .unwrap();
        let csv = scratch("open_request_events.csv", "city,amount\n北京,10\n上海,20\n");

        let outcome = open_request(&[csv], false).unwrap();

        let OpenOutcome::Attached(attach) = outcome else {
            panic!("a data file must not replace the connection");
        };
        assert_eq!(attach.report.created, ["open_request_events"]);
        assert!(attach.report.failed.is_empty());
        assert!(attach.report.problems.is_empty());

        let names = catalog_names(&attach.catalog);
        assert!(names.contains(&"keep".to_string()), "{names:?}");
        assert!(
            names.contains(&"open_request_events".to_string()),
            "{names:?}"
        );

        let total: i64 = crate::db::with_connection(|conn| {
            conn.query_row("SELECT sum(amount) FROM open_request_events", [], |r| {
                r.get(0)
            })
            .map_err(Into::into)
        })
        .unwrap();
        assert_eq!(total, 30);
        crate::db::close().unwrap();
    }

    /// Create a real, seeded database file and return its path.
    fn scratch_database() -> String {
        let path =
            std::path::Path::new(&scratch("open_request_warehouse.duckdb", "")).to_path_buf();
        std::fs::remove_file(&path).ok();
        duckdb::Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE seeded(n INTEGER)")
            .unwrap();
        path.to_string_lossy().to_string()
    }

    #[test]
    fn a_database_path_replaces_the_connection() {
        let _guard = crate::db::connection_guard();
        crate::db::open_memory().unwrap();
        let database = scratch_database();
        let requested = [database.clone()];

        let outcome = open_request(&requested, false).unwrap();

        let OpenOutcome::Connected(connected) = outcome else {
            panic!("a database path must replace the connection");
        };
        assert_eq!(connected.target, DatabaseTarget::File(database));
        assert!(connected.database_error.is_none());
        let names = catalog_names(&connected.attach.catalog);
        assert!(names.contains(&"seeded".to_string()), "{names:?}");
        crate::db::close().unwrap();
    }

    #[test]
    fn an_unopenable_database_preserves_the_interactive_connection() {
        let _guard = crate::db::connection_guard();
        crate::db::open_memory().unwrap();
        crate::db::with_connection(|conn| {
            conn.execute_batch("CREATE TABLE keep AS SELECT 42 AS n")
                .map_err(Into::into)
        })
        .unwrap();
        let broken = scratch("open_request_broken_interactive.duckdb", "not a database");

        assert!(open_request(&[broken], false).is_err());

        let n: i64 = crate::db::with_connection(|conn| {
            conn.query_row("SELECT n FROM keep", [], |r| r.get(0))
                .map_err(Into::into)
        })
        .unwrap();
        assert_eq!(n, 42);
        crate::db::close().unwrap();
    }

    #[test]
    fn an_unopenable_database_falls_back_to_memory_at_startup() {
        let _guard = crate::db::connection_guard();
        crate::db::close().unwrap();
        let broken = scratch("open_request_broken.duckdb", "this is not a duckdb file");

        let outcome = open_request(&[broken], true).unwrap();

        let OpenOutcome::Connected(connected) = outcome else {
            panic!("a database path must replace the connection");
        };
        assert!(connected.database_error.is_some());
        assert_eq!(connected.target, DatabaseTarget::Memory);
        assert!(crate::db::is_connected());
        crate::db::close().unwrap();
    }
}
