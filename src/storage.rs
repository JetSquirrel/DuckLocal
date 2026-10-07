//! What a `.duckdb` file's header says about who can read it.
//!
//! A database file starts with a fixed header DuckDB writes before anything
//! else, and it can be read without DuckDB opening the file — which is the
//! point: the file this matters for is the one DuckDB refuses to open.
//!
//! ```text
//! 0..8     checksum of the block
//! 8..12    "DUCK"
//! 12..20   storage version number (u64 LE): the format, 64 since v0.10
//! 20..52   flags
//! 52..84   the library version that created the file, NUL-padded ("v1.4.3")
//! 84..116  its source id (a commit hash)
//! ```
//!
//! Files written by older DuckDB releases (before v1.2) leave the two
//! strings empty.

use std::io::Read;

use crate::i18n::{trf_in, Language};

const MAGIC: &[u8; 4] = b"DUCK";
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";
const HEADER_LEN: usize = 116;
/// The first storage number of the v1.x line of formats; files below it were
/// written by v0.9 or earlier, which no current release reads.
const FIRST_STABLE_STORAGE: u64 = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileKind {
    Duckdb {
        storage: u64,
        /// The DuckDB release that created the file, when it says.
        created_by: Option<String>,
    },
    Sqlite,
    /// Not a database file DuckLocal can name.
    Other,
}

/// Read the kind of database at `path` from its first bytes. `None` when
/// the file cannot be read at all, or is empty: a new database is about to
/// be created there.
pub fn file_kind(path: &str) -> Option<FileKind> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut header = Vec::with_capacity(HEADER_LEN);
    file.by_ref()
        .take(HEADER_LEN as u64)
        .read_to_end(&mut header)
        .ok()?;
    if header.is_empty() {
        return None;
    }
    Some(kind_of(&header))
}

fn kind_of(header: &[u8]) -> FileKind {
    if header.starts_with(SQLITE_MAGIC) {
        return FileKind::Sqlite;
    }
    if header.len() < 20 || &header[8..12] != MAGIC {
        return FileKind::Other;
    }
    let storage = u64::from_le_bytes(header[12..20].try_into().expect("8 bytes"));
    let created_by = header
        .get(52..84)
        .map(|raw| {
            let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            String::from_utf8_lossy(&raw[..end]).trim().to_string()
        })
        .filter(|v| v.starts_with('v'));
    FileKind::Duckdb {
        storage,
        created_by,
    }
}

/// `v1.10.2` → `(1, 10, 2)`; anything else, including dev builds' suffixes
/// beyond the patch number, compares by what parses.
fn release(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version
        .trim_start_matches('v')
        .split(|c: char| !c.is_ascii_digit())
        .map(|p| p.parse::<u64>().ok());
    Some((
        parts.next()??,
        parts.next()??,
        parts.next().flatten().unwrap_or(0),
    ))
}

/// Why DuckDB refused to open `path`, in terms of what to do about it — or
/// `None` when the header does not explain the failure and DuckDB's own
/// message is the best there is.
///
/// `ours` is the bundled DuckDB's version (`v1.5.5`); `error` is DuckDB's
/// message. A file's version only explains a failure DuckDB blamed on the
/// version: a newer release's file that this one can still read fails for
/// other reasons too — a lock held by that release, above all.
pub fn explain_open_failure(path: &str, ours: &str, error: &str, lang: Language) -> Option<String> {
    let text = |key, args: &[&str]| Some(trf_in(lang, key, args));
    match file_kind(path)? {
        FileKind::Sqlite => text("storage.sqlite", &[&sql_string(path)]),
        FileKind::Other => text("storage.not_duckdb", &[]),
        FileKind::Duckdb { storage, .. }
            if storage < FIRST_STABLE_STORAGE && refused_for_its_version(error) =>
        {
            text("storage.too_old", &[ours])
        }
        FileKind::Duckdb {
            created_by: Some(created_by),
            ..
        } if release(&created_by) > release(ours) && refused_for_its_version(error) => text(
            "storage.too_new",
            &[&created_by, ours, &sql_string(path), ours],
        ),
        FileKind::Duckdb { .. } => None,
    }
}

/// The bundled DuckDB's release, `v1.5.5`, without opening a database.
pub fn library_version() -> &'static str {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| {
        // SAFETY: returns a pointer to a static, NUL-terminated string.
        unsafe { std::ffi::CStr::from_ptr(duckdb::ffi::duckdb_library_version()) }
            .to_string_lossy()
            .into_owned()
    })
}

/// Whether DuckDB refused a file over its storage version: the two messages
/// it opens a file with, for the header's version number and for the
/// serialization a newer release wrote.
fn refused_for_its_version(error: &str) -> bool {
    error.contains("database file with version number")
        || error.contains("storage version greater than the latest version")
}

/// [`explain_open_failure`]'s text in front of DuckDB's own message, which
/// stays for whoever needs the exact error.
pub fn explain_error(path: &str, error: impl std::fmt::Display, lang: Language) -> String {
    let message = error.to_string();
    match explain_open_failure(path, library_version(), &message, lang) {
        Some(explanation) => format!("{explanation}\n\n{message}"),
        None => message,
    }
}

/// `path` as a single-quoted SQL string literal.
fn sql_string(path: &str) -> String {
    format!("'{}'", path.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(storage: u64, created_by: &str) -> Vec<u8> {
        let mut h = vec![0u8; HEADER_LEN];
        h[8..12].copy_from_slice(MAGIC);
        h[12..20].copy_from_slice(&storage.to_le_bytes());
        h[52..52 + created_by.len()].copy_from_slice(created_by.as_bytes());
        h
    }

    #[test]
    fn reads_the_header_of_a_file_this_build_wrote() {
        let path = std::env::temp_dir().join("ducklocal_storage_header.duckdb");
        let _ = std::fs::remove_file(&path);
        let conn = duckdb::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE t AS SELECT 1 AS a; CHECKPOINT")
            .unwrap();
        drop(conn);
        let ours: String = duckdb::Connection::open_in_memory()
            .unwrap()
            .query_row("SELECT library_version FROM pragma_version()", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(library_version(), ours);
        let kind = file_kind(path.to_str().unwrap()).unwrap();
        let FileKind::Duckdb {
            storage,
            created_by,
        } = kind
        else {
            panic!("expected a DuckDB file, got {kind:?}");
        };
        assert!(storage >= FIRST_STABLE_STORAGE);
        assert_eq!(created_by.as_deref(), Some(ours.as_str()));
        // A file this build reads needs no explanation.
        assert_eq!(
            explain_open_failure(path.to_str().unwrap(), &ours, TOO_NEW, Language::En),
            None
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn names_what_the_header_says() {
        assert_eq!(
            kind_of(&header(68, "v1.6.0")),
            FileKind::Duckdb {
                storage: 68,
                created_by: Some("v1.6.0".into())
            }
        );
        // Pre-v1.2 files carry no version string.
        assert_eq!(
            kind_of(&header(64, "")),
            FileKind::Duckdb {
                storage: 64,
                created_by: None
            }
        );
        assert_eq!(kind_of(b"SQLite format 3\0rest"), FileKind::Sqlite);
        assert_eq!(kind_of(b"hello\n"), FileKind::Other);
    }

    #[test]
    fn explains_a_file_from_a_newer_or_ancient_duckdb() {
        let dir = std::env::temp_dir();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            path.to_str().unwrap().to_string()
        };
        let newer = write("ducklocal_storage_newer.duckdb", &header(69, "v1.10.0"));
        let message = explain_open_failure(&newer, "v1.5.5", TOO_NEW, Language::En).unwrap();
        assert!(
            message.contains("v1.10.0") && message.contains("v1.5.5"),
            "{message}"
        );
        assert!(message.contains("STORAGE_VERSION"), "{message}");

        let same = write("ducklocal_storage_same.duckdb", &header(64, "v1.5.5"));
        assert_eq!(explain_open_failure(&same, "v1.5.5", TOO_NEW, Language::En), None);

        let ancient = write("ducklocal_storage_ancient.duckdb", &header(51, ""));
        assert!(explain_open_failure(&ancient, "v1.5.5", OLD_NUMBER, Language::En).is_some());

        let sqlite = write("ducklocal_storage.sqlite", b"SQLite format 3\0....");
        assert!(explain_open_failure(&sqlite, "v1.5.5", "not a valid DuckDB database file", Language::En)
            .unwrap()
            .contains("sqlite"));

        for path in [newer, same, ancient, sqlite] {
            let _ = std::fs::remove_file(path);
        }
    }

    /// DuckDB's messages for a file it will not read for its version.
    const TOO_NEW: &str = "Invalid Input Error: Error opening \"x.duckdb\": file was written with \
        a storage version greater than the latest version supported by this DuckDB instance.";
    const OLD_NUMBER: &str = "IO Error: Trying to read a database file with version number 51, \
        but we can only read versions between 64 and 67.";

    #[test]
    fn a_newer_file_that_failed_for_another_reason_is_not_called_too_new() {
        let path = std::env::temp_dir().join("ducklocal_storage_newer_locked.duckdb");
        std::fs::write(&path, header(64, "v1.10.0")).unwrap();
        let path = path.to_str().unwrap().to_string();
        let locked = "IO Error: Could not set lock on file \"x.duckdb\": Conflicting lock is held";
        assert_eq!(explain_open_failure(&path, "v1.5.5", locked, Language::En), None);
        assert!(explain_open_failure(&path, "v1.5.5", OLD_NUMBER, Language::En).is_some());
        let _ = std::fs::remove_file(path);
    }

    /// The copy the too-new message prescribes runs as written.
    #[test]
    fn the_prescribed_downgrade_runs() {
        let dir = std::env::temp_dir();
        let src = dir.join("ducklocal_storage_src.duckdb");
        let dst = dir.join("ducklocal_storage_dst.duckdb");
        for path in [&src, &dst] {
            let _ = std::fs::remove_file(path);
        }
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "ATTACH {} AS made (STORAGE_VERSION 'v1.5.0'); \
             CREATE TABLE made.t AS SELECT 42 AS n; DETACH made;",
            sql_string(src.to_str().unwrap())
        ))
        .unwrap();
        conn.execute_batch(&format!(
            "ATTACH {} AS src (READ_ONLY); \
             ATTACH {} AS dst (STORAGE_VERSION 'v1.0.0'); \
             COPY FROM DATABASE src TO dst;",
            sql_string(src.to_str().unwrap()),
            sql_string(dst.to_str().unwrap()),
        ))
        .unwrap();
        let n: i64 = conn
            .query_row("SELECT n FROM dst.t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 42);
        drop(conn);
        for path in [&src, &dst] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn releases_compare_numerically() {
        assert!(release("v1.10.0") > release("v1.9.9"));
        assert!(release("v1.5.5") > release("v1.5.0"));
        assert_eq!(release("v1.5.5-dev123"), Some((1, 5, 5)));
        assert_eq!(release("garbage"), None);
    }
}
