//! Shared application state entity: connection status, catalog, history,
//! and status-bar data. Views subscribe to this entity for cross-panel refresh.

use std::rc::Rc;

use gpui_kit::{Context, EventEmitter};

use crate::db::{DatabaseTarget, ServerInfo};
use crate::history::HistoryEntry;
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
    pub kind: String,
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
    /// Endpoint of the session-scoped S3 secret, if configured.
    pub s3_endpoint: Option<String>,
}

impl EventEmitter<ConnectionChanged> for AppState {}
impl EventEmitter<HistoryChanged> for AppState {}
impl EventEmitter<QueryStatsChanged> for AppState {}
impl EventEmitter<AttachedFilesChanged> for AppState {}
impl EventEmitter<S3ConfigChanged> for AppState {}

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
            s3_endpoint: None,
        }
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
        self.s3_endpoint = None;
        cx.emit(ConnectionChanged);
        cx.notify();
    }

    pub fn set_attached_files(&mut self, files: Vec<AttachedFileView>, cx: &mut Context<Self>) {
        self.attached_files = files;
        cx.emit(AttachedFilesChanged);
        cx.notify();
    }

    pub fn set_s3_configured(&mut self, endpoint: Option<String>, cx: &mut Context<Self>) {
        self.s3_endpoint = endpoint;
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

/// Blocking helper: re-attach every registered data file into the current
/// connection. Files that no longer exist are skipped (their registration is
/// kept, so they reappear when the file comes back).
pub fn reattach_registered_files() {
    for file in crate::history::attached_files().unwrap_or_default() {
        crate::db::attach_data_file(&file.path).ok();
    }
}

/// Blocking helper: registered files with row counts resolved against the
/// current connection (`None` when the count query fails).
pub fn load_attached_files() -> Vec<AttachedFileView> {
    crate::history::attached_files()
        .unwrap_or_default()
        .into_iter()
        .map(|file| {
            let row_count = crate::db::with_connection(|conn| {
                let quoted = file.view_name.replace('"', "\"\"");
                conn.query_row(
                    &format!("SELECT count(*) FROM \"{quoted}\""),
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(Into::into)
            })
            .ok();
            AttachedFileView {
                id: file.id,
                path: file.path,
                view_name: file.view_name,
                kind: file.kind,
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
