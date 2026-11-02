//! `ducklocal query --restricted`: running SQL an agent wrote, or read out of
//! data, with DuckDB's own limits on what it can reach.
//!
//! Two layers, because neither is enough alone (`docs/agent-security.md`):
//!
//! - DuckDB's settings refuse every file, URL and extension outside the
//!   allowlist, then lock themselves so the SQL cannot turn them back.
//! - DuckDB's `allowed_paths` and `allowed_directories` let a statement write
//!   as well as read, so `COPY` could overwrite the very file it was allowed
//!   to read. DuckDB's parser therefore has to see one SELECT before anything
//!   runs: an allowlist of what a statement is, not a search of its text.
//!
//! This runs inside DuckLocal's process. It is no operating-system sandbox,
//! and it binds the SQL, not whoever chooses the command line.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use duckdb::Connection;
use serde_json::{json, Value};

use crate::cli::CliError;

pub(crate) const DEFAULT_TIMEOUT_SECONDS: u64 = 60;
pub(crate) const DEFAULT_MEMORY_LIMIT: &str = "2GB";

/// What a restricted statement may reach.
#[derive(Debug)]
pub(crate) struct Policy {
    directories: Vec<String>,
    files: Vec<String>,
    /// The paths as reported back: canonical, one per `--allow-read`.
    allow_read: Vec<String>,
    timeout_seconds: u64,
    memory_limit: String,
}

/// A path as DuckDB and a person should read it: Windows' canonical form
/// carries a `\\?\` prefix neither would write.
fn plain(path: &Path) -> String {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => rest.to_string(),
        _ => text.into_owned(),
    }
}

/// `path` as named, made absolute, or `None` if only the file system can say
/// what it means. A root reached through a link (`/tmp` on macOS) is written
/// both ways in SQL, and DuckDB compares text.
fn as_named(path: &Path, cwd: &Path) -> Option<String> {
    let joined = cwd.join(path);
    let mut named = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => return None,
            other => named.push(other),
        }
    }
    Some(plain(&named))
}

fn valid_memory_limit(limit: &str) -> bool {
    let digits = limit.bytes().take_while(u8::is_ascii_digit).count();
    let unit = limit[digits..].to_ascii_uppercase();
    digits > 0
        && limit[..digits].parse::<u64>().is_ok_and(|n| n > 0)
        && matches!(
            unit.as_str(),
            "KB" | "MB" | "GB" | "TB" | "KIB" | "MIB" | "GIB" | "TIB"
        )
}

fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn list(items: &[String]) -> String {
    let items: Vec<String> = items.iter().map(|item| quoted(item)).collect();
    format!("[{}]", items.join(", "))
}

impl Policy {
    /// Each path must exist: one that does not could be created later, and a
    /// typo should not read as "nothing to allow".
    pub(crate) fn new(
        allow_read: &[PathBuf],
        timeout_seconds: Option<u64>,
        memory_limit: Option<String>,
    ) -> Result<Self, CliError> {
        let cwd = std::env::current_dir().map_err(|e| CliError::failure("io", e))?;
        let memory_limit = memory_limit.unwrap_or_else(|| DEFAULT_MEMORY_LIMIT.to_string());
        if !valid_memory_limit(&memory_limit) {
            return Err(CliError::argument(format!(
                "--memory-limit must be a size such as 512MB or 2GB; got {memory_limit:?}"
            )));
        }
        let mut policy = Self {
            directories: Vec::new(),
            files: Vec::new(),
            allow_read: Vec::new(),
            timeout_seconds: timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS),
            memory_limit,
        };
        for path in allow_read {
            let canonical = std::fs::canonicalize(path)
                .map_err(|e| CliError::argument(format!("--allow-read {}: {e}", path.display())))?;
            let kept = if canonical.is_dir() {
                &mut policy.directories
            } else {
                &mut policy.files
            };
            let canonical = plain(&canonical);
            policy.allow_read.push(canonical.clone());
            for form in as_named(path, &cwd).into_iter().chain([canonical]) {
                if !kept.contains(&form) {
                    kept.push(form);
                }
            }
        }
        Ok(policy)
    }

    /// Close `conn` to everything outside the policy. Any setting it refuses
    /// fails the command: there is no running with less than was asked for.
    pub(crate) fn enforce(&self, conn: &Connection) -> Result<(), CliError> {
        let statements = [
            format!("SET allowed_directories = {}", list(&self.directories)),
            format!("SET allowed_paths = {}", list(&self.files)),
            // A spill to disk would be a write nothing here allowed.
            "SET temp_directory = ''".to_string(),
            format!("SET memory_limit = {}", quoted(&self.memory_limit)),
            "SET autoinstall_known_extensions = false".to_string(),
            "SET autoload_known_extensions = false".to_string(),
            "SET allow_community_extensions = false".to_string(),
            "SET enable_external_access = false".to_string(),
            // Last: everything above stops being changeable by the SQL.
            "SET lock_configuration = true".to_string(),
        ];
        for statement in statements {
            conn.execute_batch(&statement).map_err(|e| {
                CliError::failure(
                    "policy_setup",
                    format!("Cannot apply the restriction, so nothing ran: {e}"),
                )
            })?;
        }
        Ok(())
    }

    /// Refuse anything but one SELECT, by DuckDB's own parser, before it runs.
    pub(crate) fn admit(&self, conn: &Connection, sql: &str) -> Result<(), CliError> {
        let reads = crate::query::is_read_only_query(conn, sql).map_err(|e| {
            CliError::failure(
                "policy_setup",
                format!("Cannot read the statement, so nothing ran: {e}"),
            )
        })?;
        if reads {
            return Ok(());
        }
        Err(
            CliError::failure("policy_denied", "--restricted runs only a single SELECT").with_hint(
                "FROM, VALUES, SHOW, DESCRIBE and SUMMARIZE count, and so does a PIVOT that names its \
                 values: SELECT * FROM (PIVOT t ON a IN ('x', 'y') USING sum(b)). Anything that \
                 writes needs the person's approval to run without --restricted",
            ),
        )
    }

    /// Interrupt `conn` when the time runs out; dropping the watch stops it.
    pub(crate) fn watch(&self, conn: &Connection) -> Watch {
        let handle = conn.interrupt_handle();
        let fired = Arc::new(AtomicBool::new(false));
        let (stop, stopped) = mpsc::channel::<()>();
        let timeout = Duration::from_secs(self.timeout_seconds);
        let flag = fired.clone();
        std::thread::spawn(move || {
            if let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(timeout) {
                flag.store(true, Ordering::SeqCst);
                handle.interrupt();
            }
        });
        Watch { _stop: stop, fired }
    }

    /// The error for a statement that failed under the policy, when the policy
    /// is why. DuckDB's message stays the message. A wording this does not
    /// recognize only loses the kind and the hint: the statement failed anyway.
    pub(crate) fn classify(&self, message: &str, watch: &Watch) -> Option<CliError> {
        if watch.fired.load(Ordering::SeqCst) {
            return Some(
                CliError::failure(
                    "timeout",
                    format!("Stopped after the {}s limit", self.timeout_seconds),
                )
                .with_hint("Narrow the query, or raise --timeout if the person agrees"),
            );
        }
        if message.contains("Out of Memory Error") {
            return Some(
                CliError::failure("resource_limit", message).with_hint(format!(
                    "The limit is {} and nothing spills to disk; narrow the query, or raise \
                     --memory-limit if the person agrees",
                    self.memory_limit
                )),
            );
        }
        if message.contains("Permission Error") || message.contains("configuration has been locked")
        {
            return Some(CliError::failure("policy_denied", message).with_hint(
                "Outside what --restricted allows. Ask the person before adding \
                     --allow-read PATH; writes, network and extensions need their approval to \
                     run without --restricted",
            ));
        }
        None
    }

    /// The limits a result ran under, for its output.
    pub(crate) fn report(&self) -> Value {
        json!({
            "allow_read": self.allow_read,
            "timeout_seconds": self.timeout_seconds,
            "memory_limit": self.memory_limit,
        })
    }
}

/// A running statement's timer.
pub(crate) struct Watch {
    /// Dropped with the watch, which wakes the timer to end.
    _stop: mpsc::Sender<()>,
    fired: Arc<AtomicBool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_memory_limit_is_a_size_and_nothing_else() {
        for limit in ["512MB", "2GB", "1gib", "100kb"] {
            assert!(valid_memory_limit(limit), "{limit}");
        }
        for limit in ["", "GB", "2", "0GB", "-1GB", "2GB'; COPY", "1.5GB", "2 GB"] {
            assert!(!valid_memory_limit(limit), "{limit}");
        }
    }

    #[test]
    fn a_quote_in_a_path_stays_in_the_string() {
        assert_eq!(
            list(&["it's".to_string(), "b".to_string()]),
            "['it''s', 'b']"
        );
    }

    #[test]
    fn a_path_is_named_absolute_unless_only_the_disk_can_say() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(as_named(Path::new("."), &cwd), Some(plain(&cwd)));
        assert_eq!(
            as_named(Path::new("./a"), &cwd),
            Some(plain(&cwd.join("a")))
        );
        assert_eq!(as_named(Path::new("../a"), &cwd), None);
    }

    #[test]
    fn a_path_that_does_not_exist_is_refused_up_front() {
        let error = Policy::new(&[PathBuf::from("ducklocal_no_such_dir")], None, None).unwrap_err();
        assert_eq!(error.kind, "argument");
        assert!(error.message.contains("ducklocal_no_such_dir"));
    }

    #[test]
    fn the_defaults_are_reported() {
        let policy = Policy::new(&[], None, None).unwrap();
        let report = policy.report();
        assert_eq!(report["allow_read"], json!([]));
        assert_eq!(report["timeout_seconds"], DEFAULT_TIMEOUT_SECONDS);
        assert_eq!(report["memory_limit"], DEFAULT_MEMORY_LIMIT);
    }
}
