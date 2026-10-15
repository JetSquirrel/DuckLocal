//! The window's log file and crash reports.
//!
//! A release build has no console on Windows and is not started from one on
//! macOS, so what `tracing` writes to stderr reaches no one, and a panic —
//! which aborts the process (`panic = "abort"`) — leaves nothing behind. The
//! window therefore also logs to `<data>/logs/ducklocal.log`, and a panic
//! writes `crash-<unix seconds>.log` beside it *synchronously*, from the hook,
//! before the abort. A `crash.pending` marker naming that report tells the
//! next launch to say what happened (`take_pending`).
//!
//! Only the window does this: the CLI keeps its stderr, which its caller sees.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer as _;

const LOG_FILE: &str = "ducklocal.log";
/// The previous launch's log, kept so a crash's lead-up survives the
/// relaunch that reports it.
const PREVIOUS_LOG_FILE: &str = "ducklocal.previous.log";
const REPORT_PREFIX: &str = "crash-";
const PENDING_MARKER: &str = "crash.pending";
/// How many crash reports are kept, newest first.
const KEPT_REPORTS: usize = 10;

/// Where the log and the crash reports live: beside the history store.
pub fn logs_dir() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "DuckLocal")?;
    Some(dirs.data_dir().join("logs"))
}

/// Log to stderr as before and to the log file, then install the panic hook.
/// A log folder that cannot be made costs the file, never the launch.
pub fn init() {
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
    };
    let dir = logs_dir().filter(|dir| fs::create_dir_all(dir).is_ok());
    let file = dir.as_deref().and_then(open_log);
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(filter()))
        .with(file.map(|file| {
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(Mutex::new(file))
                .with_filter(filter())
        }))
        .init();
    if let Some(dir) = dir {
        prune_reports(&dir, KEPT_REPORTS);
        install_hook(dir);
    }
}

/// This launch's log, the last one's moved aside.
fn open_log(dir: &Path) -> Option<File> {
    let log = dir.join(LOG_FILE);
    if log.exists() {
        fs::rename(&log, dir.join(PREVIOUS_LOG_FILE)).ok();
    }
    File::create(log).ok()
}

fn install_hook(dir: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let thread = std::thread::current();
        let report = report_text(
            &panic_message(info.payload()),
            info.location().map(|at| at.to_string()).as_deref(),
            thread.name().unwrap_or("unnamed"),
            &std::backtrace::Backtrace::force_capture().to_string(),
        );
        // Each step on its own: a report written without its marker is
        // still on disk to find.
        let path = dir.join(format!("{REPORT_PREFIX}{secs}.log"));
        if fs::write(&path, report).is_ok() {
            fs::write(dir.join(PENDING_MARKER), path.to_string_lossy().as_bytes()).ok();
        }
        previous(info);
    }));
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| message.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(no message)".to_string())
}

/// The report's text. Its first line is the panic message, which is what the
/// next launch shows.
fn report_text(message: &str, location: Option<&str>, thread: &str, backtrace: &str) -> String {
    let mut text = String::new();
    // `write!` to a String cannot fail.
    let _ = writeln!(text, "{message}");
    let _ = writeln!(text);
    let _ = writeln!(text, "location: {}", location.unwrap_or("unknown"));
    let _ = writeln!(text, "thread: {thread}");
    let _ = writeln!(text, "version: {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(
        text,
        "os: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let _ = writeln!(text, "duckdb: {}", crate::storage::library_version());
    let _ = writeln!(text);
    let _ = writeln!(text, "{backtrace}");
    text
}

/// A crash the last run left a report for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashReport {
    pub path: PathBuf,
    /// The panic message: the report's first line.
    pub summary: String,
}

/// The report of a crash not yet told about, once: the marker goes with it.
pub fn take_pending() -> Option<CrashReport> {
    take_pending_in(&logs_dir()?)
}

fn take_pending_in(dir: &Path) -> Option<CrashReport> {
    let marker = dir.join(PENDING_MARKER);
    let named = fs::read_to_string(&marker).ok()?;
    fs::remove_file(&marker).ok();
    let path = PathBuf::from(named.trim());
    let summary = fs::read_to_string(&path)
        .ok()?
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    Some(CrashReport { path, summary })
}

/// Remove all but the newest `keep` reports. Their names carry the time, so
/// the newest sort last.
fn prune_reports(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut reports: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let secs = name
                .strip_prefix(REPORT_PREFIX)?
                .strip_suffix(".log")?
                .parse()
                .ok()?;
            Some((secs, entry.path()))
        })
        .collect();
    reports.sort();
    let excess = reports.len().saturating_sub(keep);
    for (_, path) in reports.into_iter().take(excess) {
        fs::remove_file(path).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ducklocal_crash_{label}_{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_pending_crash_is_told_once_with_its_first_line() {
        let dir = temp_dir("pending");
        let report = dir.join("crash-5.log");
        fs::write(
            &report,
            report_text("index out of bounds", Some("src/a.rs:1:2"), "main", ""),
        )
        .unwrap();
        fs::write(
            dir.join(PENDING_MARKER),
            report.to_string_lossy().as_bytes(),
        )
        .unwrap();
        assert_eq!(
            take_pending_in(&dir),
            Some(CrashReport {
                path: report,
                summary: "index out of bounds".into()
            })
        );
        assert_eq!(take_pending_in(&dir), None);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_the_newest_reports_are_kept() {
        let dir = temp_dir("prune");
        for secs in [30, 100, 9, 2000] {
            fs::write(dir.join(format!("crash-{secs}.log")), "x").unwrap();
        }
        fs::write(dir.join(LOG_FILE), "x").unwrap();
        prune_reports(&dir, 2);
        let mut left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        left.sort();
        assert_eq!(left, ["crash-100.log", "crash-2000.log", "ducklocal.log"]);
        fs::remove_dir_all(&dir).ok();
    }
}
