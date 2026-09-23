//! What an app asked the database, kept so it can be written into a document.
//!
//! The recording is process-global rather than thread-local, and that is not a
//! stylistic choice: an app's `query()` runs inside `smol::unblock`, so the
//! thread that mounts the app is not the thread that answers it. A
//! thread-local would see nothing.
//!
//! Recording is off unless [`start`] was called, so the GUI — which mounts the
//! same host module — pays one relaxed load per call and stores nothing.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, PoisonError};

use crate::query::CliResult;

/// A table or view the app's `catalog()` call saw.
#[derive(Debug)]
pub struct Table {
    pub database: String,
    pub schema: String,
    pub name: String,
    pub kind: &'static str,
    pub estimated_rows: Option<i64>,
    pub comment: Option<String>,
    pub columns: Vec<(String, String)>,
}

/// One host read, and what came back.
#[derive(Debug)]
pub enum Statement {
    Query {
        sql: String,
        limit: usize,
        /// The failure is kept as well as the success: an app that asks for a
        /// table that is not there is exactly what a report should show.
        outcome: Result<CliResult, String>,
    },
    Catalog {
        tables: Vec<Table>,
    },
}

/// One statement, and how many times the app ran it.
#[derive(Debug)]
pub struct Capture {
    pub statement: Statement,
    pub runs: usize,
}

impl Capture {
    /// The SQL, for a query; `None` for a catalog read.
    pub fn sql(&self) -> Option<&str> {
        match &self.statement {
            Statement::Query { sql, .. } => Some(sql),
            Statement::Catalog { .. } => None,
        }
    }

    /// The result, or why there was none.
    pub fn result(&self) -> Option<&Result<CliResult, String>> {
        match &self.statement {
            Statement::Query { outcome, .. } => Some(outcome),
            Statement::Catalog { .. } => None,
        }
    }
}

static CAPTURES: LazyLock<Mutex<Vec<Capture>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Bumped on every recording, so a settle loop can tell "nothing new" from
/// "nothing at all" without reading the captures themselves.
static REVISION: AtomicU64 = AtomicU64::new(0);
static RECORDING: AtomicBool = AtomicBool::new(false);

/// Queries running right now. A statement is recorded only when it finishes,
/// so a quiet period measured on recordings alone calls a slow query "idle"
/// and stops the capture mid-app — the settle loop waits this out first.
static IN_FLIGHT: AtomicU64 = AtomicU64::new(0);

/// Counts one query as running until dropped, panic or not.
pub struct InFlightGuard {
    _private: (),
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Mark a query as started; the guard it returns marks it finished.
pub fn track_query() -> InFlightGuard {
    IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
    InFlightGuard { _private: () }
}

/// How many queries are running right now.
pub fn in_flight() -> u64 {
    IN_FLIGHT.load(Ordering::SeqCst)
}

/// Begin recording, discarding anything recorded before.
pub fn start() {
    lock().clear();
    RECORDING.store(true, Ordering::SeqCst);
}

/// Stop recording. The captures stay until [`take`].
#[cfg(test)]
pub fn stop() {
    RECORDING.store(false, Ordering::SeqCst);
}

/// How many recordings have happened. Its value is not meaningful; a change in
/// it is.
pub fn revision() -> u64 {
    REVISION.load(Ordering::SeqCst)
}

/// Everything recorded, leaving nothing behind.
pub fn take() -> Vec<Capture> {
    std::mem::take(&mut *lock())
}

/// Record a successful or failed statement.
///
/// The same statement run twice is one capture with `runs` at two: an app that
/// refreshes is not an app with more to say, and the report would otherwise
/// repeat itself for every poll. The later result is the one kept, because a
/// refresh that changed nothing is not worth a second section and a refresh
/// that changed something should not be contradicted by the older copy.
pub fn record(statement: Statement) {
    if !RECORDING.load(Ordering::SeqCst) {
        return;
    }
    let key = key_of(&statement);
    {
        let mut captures = lock();
        match captures
            .iter_mut()
            .find(|capture| key_of(&capture.statement) == key)
        {
            Some(existing) => {
                existing.statement = statement;
                existing.runs += 1;
            }
            None => captures.push(Capture { statement, runs: 1 }),
        }
    }
    REVISION.fetch_add(1, Ordering::SeqCst);
}

/// Record a `query()` call: the SQL, the limit, and the result or the failure.
pub fn query(sql: &str, limit: usize, outcome: Result<&CliResult, String>) {
    record(Statement::Query {
        sql: sql.to_string(),
        limit,
        outcome: outcome.cloned(),
    });
}

/// Record a `catalog()` call.
pub fn catalog(tables: Vec<Table>) {
    record(Statement::Catalog { tables });
}

/// Identity for de-duplication: two statements are the same statement when they
/// are literally the same statement, asked for the same number of rows.
fn key_of(statement: &Statement) -> String {
    match statement {
        Statement::Query { sql, limit, .. } => format!("query\u{1}{limit}\u{1}{sql}"),
        Statement::Catalog { .. } => "catalog".to_string(),
    }
}

fn lock() -> std::sync::MutexGuard<'static, Vec<Capture>> {
    // A poisoned lock means a panic happened while capturing, which is not a
    // reason to lose the evidence the report is made of.
    CAPTURES.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::run_cli_of;

    /// Recording is process-wide, so these tests share one recorder; each takes
    /// the one lock the other guards its own recording with.
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        crate::db::connection_guard()
    }

    fn result(conn: &duckdb::Connection, sql: &str) -> CliResult {
        run_cli_of(conn, sql, 10).unwrap()
    }

    #[test]
    fn in_flight_counts_only_running_queries() {
        assert_eq!(in_flight(), 0);
        let first = track_query();
        let second = track_query();
        assert_eq!(in_flight(), 2);
        drop(first);
        assert_eq!(in_flight(), 1);
        drop(second);
        assert_eq!(in_flight(), 0);
    }

    #[test]
    fn nothing_is_recorded_until_recording_starts() {
        let _guard = guard();
        stop();
        let _ = take();
        let conn = duckdb::Connection::open_in_memory().unwrap();
        query("SELECT 1", 10, Ok(&result(&conn, "SELECT 1")));
        assert!(take().is_empty());
    }

    #[test]
    fn a_repeated_statement_is_one_capture_holding_the_later_result() {
        let _guard = guard();
        start();
        let conn = duckdb::Connection::open_in_memory().unwrap();
        let first = result(&conn, "SELECT 1 AS n");
        query("SELECT n FROM t", 10, Ok(&first));
        let later = result(&conn, "SELECT 2 AS n");
        query("SELECT n FROM t", 10, Ok(&later));
        // A different limit is a different question, and gets its own section.
        query("SELECT n FROM t", 100, Ok(&later));

        let captures = take();
        stop();
        assert_eq!(captures.len(), 2);
        assert_eq!(captures[0].runs, 2);
        assert_eq!(captures[1].runs, 1);
        let recorded = captures[0].result().unwrap().as_ref().unwrap();
        assert_eq!(recorded.rows[0][0], serde_json::json!(2));
    }

    #[test]
    fn a_failure_is_recorded_as_well_as_a_result() {
        let _guard = guard();
        start();
        let conn = duckdb::Connection::open_in_memory().unwrap();
        let error = run_cli_of(&conn, "SELECT * FROM absent", 10).unwrap_err();
        query("SELECT * FROM absent", 10, Err(error.to_string()));
        catalog(vec![Table {
            database: "memory".to_string(),
            schema: "main".to_string(),
            name: "orders".to_string(),
            kind: "table",
            estimated_rows: Some(3),
            comment: None,
            columns: vec![("id".to_string(), "INTEGER".to_string())],
        }]);

        let captures = take();
        stop();
        assert_eq!(captures.len(), 2);
        assert!(captures[0].result().unwrap().is_err());
        let Statement::Catalog { tables } = &captures[1].statement else {
            panic!("expected a catalog capture");
        };
        assert_eq!(tables[0].name, "orders");
    }
}
