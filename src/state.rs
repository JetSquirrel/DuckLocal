//! Shared application state entity: connection status, catalog, history,
//! and status-bar data. Views subscribe to this entity for cross-panel refresh.

use std::rc::Rc;

use gpui_kit::{Context, EventEmitter};

use crate::db::{DatabaseTarget, ServerInfo};
use crate::history::HistoryEntry;
use crate::i18n::trf;
use crate::schema::DatabaseInfo;

/// The window moved to another database (or first connected).
#[derive(Clone, Debug)]
pub struct ConnectionChanged;

/// The catalog was re-read: a query or sidebar action may have created,
/// dropped or altered something. Also emitted alongside `ConnectionChanged`.
#[derive(Clone, Debug)]
pub struct CatalogChanged;

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

/// The sidebar was hidden or shown. The window re-lays itself out, and the
/// title bar offers the way back.
#[derive(Clone, Debug)]
pub struct SidebarToggled;

/// The recent-documents list gained an entry (a dashboard tab opened). The sidebar rebuilds its document groups on this.
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
    /// Where unqualified names resolve, as the last Run left it (a `USE` in
    /// it moves it). `None` until a Run reports it, and after a reconnect,
    /// which starts a fresh session: generated SQL qualifies fully then.
    pub search_path: Option<crate::script::SearchPath>,
    pub attached_files: Vec<AttachedFileView>,
    /// Session-scoped S3 credentials, if configured. Memory only.
    pub s3_config: Option<crate::s3::S3Config>,
    /// Open/attach requests in flight; > 0 while a data source is being opened.
    open_requests: usize,
    /// How many open requests have failed, and the last one's error.
    open_failures: (u64, Option<String>),
    /// Set once startup's open request has landed. Until then the panels have
    /// no opinion: their emptiness is "not loaded yet", not "nothing here".
    is_ready: bool,
    /// Whether the sidebar is hidden. Remembered between launches.
    sidebar_collapsed: bool,
}

/// The `settings` key the sidebar's collapsed state lives under.
const SIDEBAR_COLLAPSED: &str = "sidebar_collapsed";

impl EventEmitter<ConnectionChanged> for AppState {}
impl EventEmitter<CatalogChanged> for AppState {}
impl EventEmitter<HistoryChanged> for AppState {}
impl EventEmitter<QueryStatsChanged> for AppState {}
impl EventEmitter<AttachedFilesChanged> for AppState {}
impl EventEmitter<S3ConfigChanged> for AppState {}
impl EventEmitter<OpenStateChanged> for AppState {}
impl EventEmitter<RecentsChanged> for AppState {}
impl EventEmitter<SidebarToggled> for AppState {}

impl AppState {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            target: None,
            server: None,
            catalog: Vec::new(),
            history: Rc::new(Vec::new()),
            last_query: None,
            last_sql: None,
            search_path: None,
            attached_files: Vec::new(),
            s3_config: None,
            open_requests: 0,
            open_failures: (0, None),
            is_ready: false,
            // Read once, at construction: the history store is opened before
            // the window, and a store that cannot be read means "shown".
            sidebar_collapsed: crate::history::get_setting(SIDEBAR_COLLAPSED)
                .ok()
                .flatten()
                .is_some_and(|value| value == "true"),
        }
    }

    pub fn is_sidebar_collapsed(&self) -> bool {
        self.sidebar_collapsed
    }

    /// Hide the sidebar if it is shown, show it if it is hidden, and remember
    /// which.
    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        let value = if self.sidebar_collapsed {
            "true"
        } else {
            "false"
        };
        if let Err(e) = crate::history::set_setting(SIDEBAR_COLLAPSED, value) {
            tracing::warn!("Failed to persist the sidebar state: {e}");
        }
        cx.emit(SidebarToggled);
        cx.notify();
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
        !self.attached_files.is_empty() || self.catalog_has_content()
    }

    /// How many tables and views would be lost by quitting now: the ones the
    /// user made in the in-memory database. Read from what the sidebar
    /// already holds, so asking never waits for a running query.
    pub fn unsaved_objects(&self) -> usize {
        unsaved_objects_of(self.target.as_ref(), &self.catalog, &self.attached_files)
    }

    /// Whether the catalog shows anything: a table or view anywhere, or a
    /// database attached beside the open one. The open database is always
    /// listed, so an empty catalog is never literally empty.
    pub fn catalog_has_content(&self) -> bool {
        self.catalog.len() > 1 || self.catalog.iter().any(|db| !db.tables.is_empty())
    }

    /// Whether an open/attach request is in flight.
    pub fn is_opening(&self) -> bool {
        self.open_requests > 0
    }

    /// How many open requests have failed, for telling whether one just did.
    pub fn open_failures(&self) -> u64 {
        self.open_failures.0
    }

    pub fn last_open_failure(&self) -> Option<&str> {
        self.open_failures.1.as_deref()
    }

    pub fn note_open_failure(&mut self, error: String) {
        self.open_failures.0 += 1;
        self.open_failures.1 = Some(error);
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
        self.search_path = None;
        // Session-scoped S3 secrets live in the connection, so a new
        // connection starts unconfigured.
        self.s3_config = None;
        self.is_ready = true;
        // Catalog first: ConnectionChanged then re-runs every dashboard and
        // clears the marks CatalogChanged left on the background ones.
        cx.emit(CatalogChanged);
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
        cx.emit(CatalogChanged);
        cx.notify();
    }

    pub fn set_history(&mut self, history: Vec<HistoryEntry>, cx: &mut Context<Self>) {
        self.history = Rc::new(history);
        cx.emit(HistoryChanged);
        cx.notify();
    }

    /// Record where unqualified names now resolve. The status bar shows it,
    /// and it rides on the stats event it is always set beside.
    pub fn set_search_path(
        &mut self,
        search_path: Option<crate::script::SearchPath>,
        cx: &mut Context<Self>,
    ) {
        if self.search_path != search_path {
            self.search_path = search_path;
            cx.emit(QueryStatsChanged);
            cx.notify();
        }
    }

    pub fn set_last_query(&mut self, stats: QueryStats, sql: String, cx: &mut Context<Self>) {
        self.last_query = Some(stats);
        self.last_sql = Some(sql);
        cx.emit(QueryStatsChanged);
        cx.notify();
    }
}

/// The tables and views that live only in the in-memory database and that
/// nothing brings back at the next launch. A view over a registered file is
/// not one of them: it is made again from the file every time. Attached
/// databases are files, and keep what they hold.
fn unsaved_objects_of(
    target: Option<&DatabaseTarget>,
    catalog: &[DatabaseInfo],
    attached: &[AttachedFileView],
) -> usize {
    if target != Some(&DatabaseTarget::Memory) {
        return 0;
    }
    // The open database is listed first.
    let Some(memory) = catalog.first().filter(|database| database.path.is_none()) else {
        return 0;
    };
    memory
        .tables
        .iter()
        .filter(|table| {
            !(table.schema == "main"
                && attached
                    .iter()
                    .any(|file| file.view_name.eq_ignore_ascii_case(&table.name)))
        })
        .count()
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

/// Blocking: attach the database at `path` beside the open one, registering
/// it so every later connection attaches it too. Answers its alias.
pub fn attach_database(path: &str, read_only: bool) -> anyhow::Result<String> {
    let expanded = crate::db::expand_tilde(path.trim());
    // A database that does not exist yet is created by the attach, so it has
    // no canonical path to resolve; it still needs an absolute one, or the
    // registration would mean another file from another working directory.
    let path = crate::sources::canonical_file_path(&expanded)
        .or_else(|_| std::path::absolute(&expanded).map(|p| p.to_string_lossy().into_owned()))?;
    let registered = crate::history::attached_databases().unwrap_or_default();
    let attachment = crate::db::with_connection(|conn| {
        if let Some(alias) = database_at(conn, &path)? {
            anyhow::bail!(trf("error.database_already_attached", &[&alias]));
        }
        // Keep the alias it had, so SQL written against it still runs.
        let alias = match registered.iter().find(|d| d.path == path) {
            Some(known) if !database_named(conn, &known.alias)? => known.alias.clone(),
            _ => {
                let reserved: Vec<String> = registered
                    .iter()
                    .filter(|d| d.path != path)
                    .map(|d| d.alias.clone())
                    .collect();
                crate::db::database_alias_of(conn, &path, &reserved)?
            }
        };
        let attachment = crate::db::DatabaseAttachment {
            path: path.clone(),
            alias,
            read_only,
        };
        crate::db::attach_database_of(conn, &attachment)?;
        Ok(attachment)
    })?;
    crate::history::register_attached_database(&crate::history::AttachedDatabase {
        path: attachment.path,
        alias: attachment.alias.clone(),
        read_only: attachment.read_only,
    })?;
    Ok(attachment.alias)
}

/// Blocking: detach a database and forget its registration, so it does not
/// come back with the next connection.
pub fn detach_database(alias: &str) -> anyhow::Result<()> {
    crate::db::with_connection(|conn| crate::db::detach_database_of(conn, alias))?;
    crate::history::remove_attached_database(alias)?;
    Ok(())
}

/// Blocking: attach every registered database into the current connection.
/// One whose file is gone is skipped quietly and keeps its registration —
/// attaching it would create an empty database in its place — and so is one
/// that is already open, as the main database or otherwise.
pub fn reattach_registered_databases() -> Vec<String> {
    let registered = crate::history::attached_databases().unwrap_or_default();
    let mut problems = Vec::new();
    for database in registered {
        let attached = crate::db::with_connection(|conn| {
            let path = registered_path(&database.path)?;
            if !std::path::Path::new(&path).exists() || database_at(conn, &path)?.is_some() {
                return Ok(());
            }
            if database_named(conn, &database.alias)? {
                anyhow::bail!(trf(
                    "error.database_alias_taken",
                    &[&database.path, &database.alias]
                ));
            }
            crate::db::attach_database_of(
                conn,
                &crate::db::DatabaseAttachment {
                    path,
                    alias: database.alias.clone(),
                    read_only: database.read_only,
                },
            )
        });
        if let Err(e) = attached {
            problems.push(e.to_string());
        }
    }
    problems
}

/// The database already open on the file at `path`, if any.
fn database_at(conn: &duckdb::Connection, path: &str) -> anyhow::Result<Option<String>> {
    let canonical = crate::sources::canonical_file_path(path).unwrap_or_else(|_| path.into());
    let mut stmt =
        conn.prepare("SELECT database_name, path FROM duckdb_databases() WHERE path IS NOT NULL")?;
    let found = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .filter_map(|row| row.ok())
        .find(|(_, open)| {
            crate::sources::canonical_file_path(open).unwrap_or_else(|_| open.clone()) == canonical
        })
        .map(|(name, _)| name);
    Ok(found)
}

fn database_named(conn: &duckdb::Connection, alias: &str) -> anyhow::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM duckdb_databases() WHERE lower(database_name) = lower(?1)",
        [alias],
        |r| r.get(0),
    )?;
    Ok(count > 0)
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
                let attached = match &file.sheet {
                    Some(sheet) => {
                        crate::db::attach_excel_sheet_as(&path, Some(sheet), &file.view_name)
                    }
                    None => crate::db::attach_data_file_as(&path, &file.view_name),
                };
                // A missing file stays quiet, as documented above; anything
                // else — a name the open database already uses, above all —
                // is the user's to hear about.
                if let Err(e) = attached {
                    if std::path::Path::new(&path).exists() {
                        problems.push(e.to_string());
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
                    report
                        .created
                        .extend(pairs.into_iter().map(|(_, name)| name));
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
    /// Where unqualified names resolve in the new session.
    pub search_path: Option<crate::script::SearchPath>,
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
#[cfg(test)]
pub fn open_request(paths: &[String], connect_default: bool) -> anyhow::Result<OpenOutcome> {
    open_sources(crate::sources::resolve(paths), connect_default, false)
}

#[cfg(test)]
pub fn open_dialog_path(path: &str) -> anyhow::Result<OpenOutcome> {
    open_sources(crate::sources::resolve_dialog_path(path), false, false)
}

/// Something the window was asked to open, kept whole so it can be asked
/// again once the person has said what becomes of unsaved work.
#[derive(Clone, Debug)]
pub enum OpenRequest {
    /// Paths dropped on the window, picked, or sent by `ducklocal open`.
    Paths(Vec<String>),
    /// What was typed into the "Open data" dialog, which may name a database
    /// to create.
    DialogPath(String),
    /// A new in-memory database.
    Memory,
}

/// Blocking: carry out `request`. Unless `discard_unsaved`, a request that
/// would move the window off an in-memory database holding the user's own
/// tables or views is refused with [`UnsavedWork`], for the window to ask.
pub fn open(request: &OpenRequest, discard_unsaved: bool) -> anyhow::Result<OpenOutcome> {
    match request {
        OpenRequest::Paths(paths) => {
            open_sources(crate::sources::resolve(paths), false, discard_unsaved)
        }
        OpenRequest::DialogPath(path) => open_sources(
            crate::sources::resolve_dialog_path(path),
            false,
            discard_unsaved,
        ),
        OpenRequest::Memory => open_memory_database(discard_unsaved),
    }
}

/// Opening another database would end the in-memory one, and with it `count`
/// tables and views that exist nowhere else.
#[derive(Debug)]
pub struct UnsavedWork {
    pub count: usize,
}

impl std::fmt::Display for UnsavedWork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&trf("error.unsaved_work", &[&self.count.to_string()]))
    }
}

impl std::error::Error for UnsavedWork {}

/// Refuse to leave the current connection while it holds unsaved work. No
/// connection, or one that cannot be asked, holds none.
fn keep_unsaved_work(discard_unsaved: bool) -> anyhow::Result<()> {
    if discard_unsaved {
        return Ok(());
    }
    match crate::db::with_connection(crate::db::unsaved_objects_of) {
        Ok(count) if count > 0 => Err(UnsavedWork { count }.into()),
        _ => Ok(()),
    }
}

/// The `settings` key holding the database file that was open last.
const LAST_DATABASE: &str = "last_database";

/// Blocking: the launch's own open request. With no database among `paths`
/// it goes back to the database file that was open last, the way the query
/// tabs and the registered files come back; a file that has since gone is
/// passed over for the in-memory database, and one that cannot be opened
/// falls back to it with the error said.
pub fn open_at_launch(paths: &[String]) -> anyhow::Result<OpenOutcome> {
    let mut sources = crate::sources::resolve(paths);
    if sources.database.is_none() {
        sources.database = last_database();
    }
    // A launch has no earlier connection to lose anything from.
    open_sources(sources, true, true)
}

/// Blocking: go to a new in-memory database because the user asked for one,
/// so the next launch starts there too.
pub fn open_memory_database(discard_unsaved: bool) -> anyhow::Result<OpenOutcome> {
    keep_unsaved_work(discard_unsaved)?;
    let _ = crate::history::set_setting(LAST_DATABASE, "");
    open_sources(crate::sources::resolve(&[]), true, true)
}

fn last_database() -> Option<String> {
    crate::history::get_setting(LAST_DATABASE)
        .ok()
        .flatten()
        .filter(|path| std::path::Path::new(path).is_file())
}

/// Absolute, so the next launch means the same file from any directory.
fn remember_database(path: &str) {
    let expanded = crate::db::expand_tilde(path);
    let absolute = crate::sources::canonical_file_path(&expanded)
        .or_else(|_| std::path::absolute(&expanded).map(|p| p.to_string_lossy().into_owned()));
    if let Ok(absolute) = absolute {
        let _ = crate::history::set_setting(LAST_DATABASE, &absolute);
    }
}

/// Blocking: write the in-memory database to a database file at `path` and
/// move the window onto it, so what was built in memory outlives the window
/// and later changes go to the file. A path with no extension gets
/// `.duckdb`.
///
/// A file already at `path` is replaced: the save panel asked first. It is
/// replaced only once the copy is whole, so a save that fails leaves it as it
/// was.
pub fn save_memory_as(path: &str) -> anyhow::Result<OpenOutcome> {
    let mut file = std::path::PathBuf::from(crate::db::expand_tilde(path.trim()));
    if file.extension().is_none() {
        file.set_extension("duckdb");
    }
    let file = std::path::absolute(&file)?;
    let target = file.to_string_lossy().into_owned();
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let staging = format!("{target}.saving");
    let discard_staging = || {
        std::fs::remove_file(&staging).ok();
        std::fs::remove_file(format!("{staging}.wal")).ok();
    };
    // Left by a save that was cut short.
    discard_staging();
    crate::db::with_connection(|conn| {
        // Replacing a file this connection has open would pull it out from
        // under its own attachment.
        if let Some(alias) = database_at(conn, &target)? {
            anyhow::bail!(trf("error.database_already_attached", &[&alias]));
        }
        crate::db::copy_memory_database_of(conn, &staging)
    })?;
    // The log of the database being replaced must not be replayed into the
    // new one.
    std::fs::remove_file(format!("{target}.wal")).ok();
    if let Err(error) = std::fs::rename(&staging, &target) {
        discard_staging();
        return Err(error.into());
    }
    open_sources(
        crate::sources::Sources {
            database: Some(target),
            ..Default::default()
        },
        false,
        // The in-memory database is in the file now.
        true,
    )
}

fn open_sources(
    sources: crate::sources::Sources,
    connect_default: bool,
    discard_unsaved: bool,
) -> anyhow::Result<OpenOutcome> {
    if sources.database.is_none() && !connect_default {
        return Ok(OpenOutcome::Attached(attach_outcome(&sources)));
    }
    // From here on the connection is replaced.
    keep_unsaved_work(discard_unsaved)?;

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
    // Only a database that opened: one that failed this once (held by
    // another process, say) is still the one to come back to.
    if let (DatabaseTarget::File(path), None) = (&target, &database_error) {
        remember_database(path);
    }

    let mut problems = reattach_registered_databases();
    problems.extend(reattach_registered_files());
    let server = crate::db::server_info()?;
    let mut attach = attach_outcome(&sources);
    attach.report.problems.extend(problems);
    let search_path =
        crate::db::with_connection(|conn| Ok(crate::script::search_path_of(conn))).unwrap_or(None);
    Ok(OpenOutcome::Connected(ConnectOutcome {
        target,
        server,
        database_error,
        search_path,
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
///
/// This runs on every startup and after every attach, and a count over a
/// CSV or JSON view reads the whole file. So a count is kept, keyed by the
/// file's size and modification time, and only an unchanged file reuses it:
/// attaching a small file next to three 2 GB CSVs no longer re-scans them.
pub fn load_attached_files() -> Vec<AttachedFileView> {
    crate::history::attached_files()
        .unwrap_or_default()
        .into_iter()
        .map(|file| {
            let row_count = registered_path(&file.path)
                .and_then(|path| {
                    let stamp = file_stamp(&path);
                    if let Some(count) = stamp
                        .as_deref()
                        .and_then(|s| cached_count(&file.view_name, &path, s))
                    {
                        return Ok(count);
                    }
                    let count = crate::db::with_connection(|conn| {
                        let quoted = file.view_name.replace('"', "\"\"");
                        conn.query_row(&format!("SELECT count(*) FROM \"{quoted}\""), [], |r| {
                            r.get::<_, i64>(0)
                        })
                        .map_err(Into::into)
                    })?;
                    if let Some(stamp) = stamp {
                        let _ = crate::history::set_setting(
                            &count_key(&file.view_name, &path),
                            &format!("{stamp}|{count}"),
                        );
                    }
                    Ok(count)
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

/// Per view, not per path: the sheets of one workbook share a path.
fn count_key(view: &str, path: &str) -> String {
    format!("row_count:{view}:{path}")
}

/// Size and modification time, or `None` for a path that is not one file
/// (a glob, a folder): those are counted every time.
fn file_stamp(path: &str) -> Option<String> {
    let meta = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(format!("{}:{modified}", meta.len()))
}

fn cached_count(view: &str, path: &str, stamp: &str) -> Option<i64> {
    let stored = crate::history::get_setting(&count_key(view, path)).ok()??;
    let (stored_stamp, count) = stored.rsplit_once('|')?;
    (stored_stamp == stamp).then(|| count.parse().ok())?
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
    fn an_attached_database_comes_back_with_every_connection_until_detached() {
        let _guard = crate::db::connection_guard();
        crate::history::with_test_history(|| {
            let dir = std::env::temp_dir().join("ducklocal_state_attach_db");
            std::fs::remove_dir_all(&dir).ok();
            std::fs::create_dir_all(&dir).unwrap();
            let logs = dir.join("logs.duckdb").to_string_lossy().to_string();
            let base = dir.join("base.duckdb").to_string_lossy().to_string();

            crate::db::open_memory().unwrap();
            // A path that does not exist yet becomes a new database.
            let alias = attach_database(&logs, false).unwrap();
            assert_eq!(alias, "logs");
            crate::db::with_connection(|conn| {
                conn.execute_batch("CREATE TABLE logs.events AS SELECT 1 AS id")?;
                Ok(())
            })
            .unwrap();
            // Twice is refused, not attached under a second name.
            assert!(attach_database(&logs, false).is_err());

            // Another main database: the attachment follows it, read-only
            // or not as it was registered.
            crate::db::open_file(&base).unwrap();
            assert!(reattach_registered_databases().is_empty());
            let catalog = crate::schema::load_catalog().unwrap();
            let names: Vec<&str> = catalog.iter().map(|d| d.name.as_str()).collect();
            assert_eq!(names, ["base", "logs"]);
            assert_eq!(catalog[1].tables[0].name, "events");
            assert!(!catalog[1].read_only);

            // Opening the attached file as the main database does not try to
            // attach it a second time.
            crate::db::with_connection(|conn| {
                conn.query_row("SELECT count(*) FROM logs.events", [], |row| {
                    row.get::<_, i64>(0)
                })?;
                Ok(())
            })
            .unwrap();
            crate::db::open_file(&logs).unwrap();
            assert!(reattach_registered_databases().is_empty());

            crate::db::open_memory().unwrap();
            assert!(reattach_registered_databases().is_empty());
            detach_database("logs").unwrap();
            assert!(crate::history::attached_databases().unwrap().is_empty());
            crate::db::open_memory().unwrap();
            assert!(reattach_registered_databases().is_empty());
            assert_eq!(crate::schema::load_catalog().unwrap().len(), 1);
            std::fs::remove_dir_all(&dir).ok();
        });
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
    fn row_counts_are_reused_until_the_file_changes() {
        let _guard = crate::db::connection_guard();
        crate::history::with_test_history(|| {
            let path =
                std::env::temp_dir().join(format!("ducklocal_count_{}.csv", std::process::id()));
            std::fs::write(&path, "n\n1\n2\n3\n").unwrap();
            let path = path.to_string_lossy().to_string();
            crate::db::open_memory().unwrap();
            attach_data_files(std::slice::from_ref(&path));
            let count = || load_attached_files()[0].row_count;
            assert_eq!(count(), Some(3));

            // Unchanged file: the stored count is used, not a new scan. A
            // doctored entry with the same stamp proves which one answered.
            let registered = &crate::history::attached_files().unwrap()[0];
            let stamp = file_stamp(&registered.path).unwrap();
            let key = count_key(&registered.view_name, &registered.path);
            crate::history::set_setting(&key, &format!("{stamp}|42")).unwrap();
            assert_eq!(count(), Some(42));

            // A changed file is counted again.
            std::fs::write(&path, "n\n1\n2\n3\n4\n").unwrap();
            assert_eq!(count(), Some(4));

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

    #[test]
    fn only_what_the_user_made_in_memory_counts_as_unsaved() {
        let table = |schema: &str, name: &str| crate::schema::TableInfo {
            database: "memory".into(),
            schema: schema.into(),
            name: name.into(),
            kind: crate::schema::NodeKind::View,
            estimated_rows: None,
            columns: Vec::new(),
        };
        let database = |path: Option<&str>, tables| DatabaseInfo {
            name: "memory".into(),
            path: path.map(Into::into),
            kind: "duckdb".into(),
            read_only: false,
            tables,
        };
        let attached = [AttachedFileView {
            id: 1,
            path: "/data/orders.csv".into(),
            view_name: "orders".into(),
            row_count: None,
        }];
        let memory = DatabaseTarget::Memory;
        let catalog = [
            database(
                None,
                vec![
                    // The file's own view: made again at every launch.
                    table("main", "Orders"),
                    table("main", "silver_orders"),
                    // Same name as the file's view, but another schema: the
                    // user's.
                    table("gold", "orders"),
                ],
            ),
            // An attached database is a file, and keeps what it holds.
            database(Some("/data/side.duckdb"), vec![table("main", "kept")]),
        ];
        assert_eq!(unsaved_objects_of(Some(&memory), &catalog, &attached), 2);
        assert_eq!(
            unsaved_objects_of(Some(&memory), &catalog[..0], &attached),
            0
        );
        let file = DatabaseTarget::File("/data/work.duckdb".into());
        assert_eq!(unsaved_objects_of(Some(&file), &catalog, &attached), 0);
        assert_eq!(unsaved_objects_of(None, &catalog, &attached), 0);
    }

    #[test]
    fn work_built_in_memory_is_saved_and_comes_back_at_the_next_launch() {
        let _guard = crate::db::connection_guard();
        crate::history::with_test_history(|| {
            let dir =
                std::env::temp_dir().join(format!("ducklocal_save_as_{}", std::process::id()));
            std::fs::remove_dir_all(&dir).ok();
            std::fs::create_dir_all(&dir).unwrap();
            let csv = dir.join("bronze.csv");
            std::fs::write(&csv, "id,amount\n1,10\n2,20\n").unwrap();
            let csv = csv.to_string_lossy().to_string();

            // A plain launch with nothing remembered is in memory.
            let OpenOutcome::Connected(launch) = open_at_launch(&[]).unwrap() else {
                panic!("a launch connects");
            };
            assert_eq!(launch.target, DatabaseTarget::Memory);
            open_request(std::slice::from_ref(&csv), false).unwrap();
            crate::db::with_connection(|conn| {
                conn.execute_batch(
                    "CREATE TABLE silver AS SELECT id, amount * 2 AS amount FROM bronze;
                     CREATE SCHEMA gold;
                     CREATE VIEW gold.total AS SELECT sum(amount) AS total FROM silver;",
                )
                .map_err(Into::into)
            })
            .unwrap();

            // No extension asked for: it is a DuckDB file all the same.
            // Joined a part at a time: the separator is the platform's, as it
            // is in the path that comes back.
            let asked = dir
                .join("nested")
                .join("model")
                .to_string_lossy()
                .to_string();
            let saved = format!("{asked}.duckdb");
            let OpenOutcome::Connected(connected) = save_memory_as(&asked).unwrap() else {
                panic!("saving moves the window onto the file");
            };
            assert_eq!(connected.target, DatabaseTarget::File(saved.clone()));
            let names = catalog_names(&connected.attach.catalog);
            for name in ["bronze", "silver", "total"] {
                assert!(names.contains(&name.to_string()), "{names:?}");
            }
            assert!(!std::path::Path::new(&format!("{saved}.saving")).exists());
            // The file is the database now: there is nothing left to save.
            let again = dir.join("again.duckdb").to_string_lossy().to_string();
            assert!(save_memory_as(&again).is_err());
            assert!(!std::path::Path::new(&again).exists());

            // Quit, and launch again with nothing on the command line.
            crate::db::close().unwrap();
            let OpenOutcome::Connected(relaunch) = open_at_launch(&[]).unwrap() else {
                panic!("a launch connects");
            };
            assert_eq!(
                std::fs::canonicalize(match &relaunch.target {
                    DatabaseTarget::File(path) => path.clone(),
                    DatabaseTarget::Memory => panic!("the saved database comes back"),
                })
                .unwrap(),
                std::fs::canonicalize(&saved).unwrap()
            );
            assert!(relaunch.database_error.is_none());
            let total: i64 = crate::db::with_connection(|conn| {
                conn.query_row("SELECT total FROM gold.total", [], |r| r.get(0))
                    .map_err(Into::into)
            })
            .unwrap();
            assert_eq!(total, 60);

            // Asking for the in-memory database is remembered too.
            let OpenOutcome::Connected(memory) = open_memory_database(false).unwrap() else {
                panic!("the in-memory database connects");
            };
            assert_eq!(memory.target, DatabaseTarget::Memory);
            let OpenOutcome::Connected(relaunch) = open_at_launch(&[]).unwrap() else {
                panic!("a launch connects");
            };
            assert_eq!(relaunch.target, DatabaseTarget::Memory);

            // Saving over a file replaces it whole.
            crate::db::with_connection(|conn| {
                conn.execute_batch(
                    "CREATE TABLE fresh AS SELECT 1 AS n; CREATE TEMP TABLE t AS SELECT 1",
                )
                .map_err(Into::into)
            })
            .unwrap();
            // With a table of the user's own in memory, nothing moves the
            // window off it unasked: not another database, not a new
            // in-memory one. Adding a data file is not leaving.
            let other = dir.join("other.duckdb").to_string_lossy().to_string();
            for request in [OpenRequest::DialogPath(other.clone()), OpenRequest::Memory] {
                let refused = open(&request, false).err().expect("unsaved work is kept");
                assert_eq!(refused.downcast_ref::<UnsavedWork>().unwrap().count, 1);
            }
            assert!(!std::path::Path::new(&other).exists());
            assert!(matches!(
                open(&OpenRequest::Paths(vec![csv.clone()]), false).unwrap(),
                OpenOutcome::Attached(_)
            ));
            let kept: i64 = crate::db::with_connection(|conn| {
                conn.query_row("SELECT n FROM fresh", [], |r| r.get(0))
                    .map_err(Into::into)
            })
            .unwrap();
            assert_eq!(kept, 1);
            let OpenOutcome::Connected(replaced) = save_memory_as(&saved).unwrap() else {
                panic!("saving moves the window onto the file");
            };
            let names = catalog_names(&replaced.attach.catalog);
            assert!(names.contains(&"fresh".to_string()), "{names:?}");
            assert!(!names.contains(&"silver".to_string()), "{names:?}");

            // A remembered database that has gone is passed over quietly.
            crate::db::close().unwrap();
            std::fs::remove_file(&saved).unwrap();
            let OpenOutcome::Connected(relaunch) = open_at_launch(&[]).unwrap() else {
                panic!("a launch connects");
            };
            assert_eq!(relaunch.target, DatabaseTarget::Memory);
            assert!(relaunch.database_error.is_none());
            crate::db::close().unwrap();
            std::fs::remove_dir_all(&dir).ok();
        });
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
