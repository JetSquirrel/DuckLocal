use std::ffi::{CStr, CString, OsString};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::ptr;

use duckdb::{ffi, AccessMode, Config, Connection};
use serde_json::json;

const HELP: &str = "DuckLocal — local data workspace and headless SQL\n\nUsage:\n  ducklocal [PATH ...]                   Open the GUI (files, folders, globs)\n  ducklocal query --sql SQL [OPTIONS]    Execute one statement, return JSON\n  ducklocal query --sql-file FILE [OPTIONS]\n  ducklocal profile TARGET [--database PATH]  Per-column statistics, JSON\n  ducklocal --help\n  ducklocal --version\n\nQuery options:\n  --sql SQL          SQL text; exactly one of --sql and --sql-file is required\n  --sql-file FILE    UTF-8 SQL file; - reads stdin\n  --database PATH    File database; must exist, opened read-only by default\n  --read-write       Allow database writes/creation (requires --database)\n  --limit N          Maximum returned rows, default 1000; positive integer\n  --help            Show this help\n\nOutput: one JSON object with columns, rows, row_count, truncated, elapsed_ms.\nColumn types are Arrow debug names, not SQL type names. A 2,000,000-cell\nbudget also applies. Limits constrain output, not computation.\nErrors: JSON on stderr, empty stdout; exit 2 for arguments, 1 for SQL/I/O.\nRead-only is NOT a filesystem/network sandbox: COPY can write files.\nExtensions are not automatically installed. GUI state/history is not used.\nA GUI path named query or profile must be written ./query, ./profile.\n\nProfile: TARGET is a data file (csv/tsv/parquet/json) or, with --database,\na table or view name. Per column it reports type, nulls, distinct, min, max;\nfor numbers the decimals actually used, the median and max_over_median; for\ndates covered_days, span_days and missing_days. Statistics are exact and read\nthe whole relation.\n";

#[derive(Debug)]
struct CliError {
    kind: &'static str,
    message: String,
    code: i32,
}

impl CliError {
    fn argument(message: impl Into<String>) -> Self {
        Self {
            kind: "argument",
            message: message.into(),
            code: 2,
        }
    }

    fn failure(kind: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            kind,
            message: error.to_string(),
            code: 1,
        }
    }
}

#[derive(Debug)]
struct Options {
    sql: Option<String>,
    sql_file: Option<PathBuf>,
    database: Option<PathBuf>,
    read_write: bool,
    limit: usize,
}

fn parse(args: &[OsString]) -> Result<Options, CliError> {
    let mut options = Options {
        sql: None,
        sql_file: None,
        database: None,
        read_write: false,
        limit: 1000,
    };
    let mut seen = std::collections::HashSet::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let flag = arg
            .to_str()
            .ok_or_else(|| CliError::argument("Option names must be UTF-8"))?;
        if !matches!(
            flag,
            "--sql" | "--sql-file" | "--database" | "--read-write" | "--limit"
        ) {
            return Err(CliError::argument(format!("Unknown query option: {flag}")));
        }
        if !seen.insert(flag) {
            return Err(CliError::argument(format!("Duplicate option: {flag}")));
        }
        if flag == "--read-write" {
            options.read_write = true;
            continue;
        }
        let value = args
            .next()
            .ok_or_else(|| CliError::argument(format!("Missing value for {flag}")))?;
        if value.is_empty()
            || matches!(
                value.to_str(),
                Some("--sql" | "--sql-file" | "--database" | "--read-write" | "--limit" | "--help")
            )
            || (flag != "--sql" && value.to_string_lossy().starts_with("--"))
        {
            return Err(CliError::argument(format!("Missing value for {flag}")));
        }
        match flag {
            "--sql" => {
                options.sql = Some(
                    value
                        .to_str()
                        .ok_or_else(|| CliError::argument("SQL must be UTF-8"))?
                        .to_string(),
                )
            }
            "--sql-file" => options.sql_file = Some(value.into()),
            "--database" => {
                if value == ":memory:" {
                    return Err(CliError::argument(
                        "Omit --database for an in-memory database",
                    ));
                }
                options.database = Some(value.into());
            }
            "--limit" => {
                let text = value.to_str().unwrap_or("");
                options.limit = text
                    .parse()
                    .ok()
                    .filter(|n| *n > 0)
                    .filter(|_| text.bytes().all(|b| b.is_ascii_digit()))
                    .ok_or_else(|| {
                        CliError::argument("--limit must be a positive integer fitting usize")
                    })?;
            }
            _ => unreachable!(),
        }
    }
    if options.sql.is_some() == options.sql_file.is_some() {
        return Err(CliError::argument(
            "Provide exactly one of --sql and --sql-file",
        ));
    }
    if options.read_write && options.database.is_none() {
        return Err(CliError::argument("--read-write requires --database"));
    }
    Ok(options)
}

struct Parser {
    database: ffi::duckdb_database,
    connection: ffi::duckdb_connection,
    extracted: ffi::duckdb_extracted_statements,
}

impl Drop for Parser {
    fn drop(&mut self) {
        unsafe {
            if !self.extracted.is_null() {
                ffi::duckdb_destroy_extracted(&mut self.extracted);
            }
            if !self.connection.is_null() {
                ffi::duckdb_disconnect(&mut self.connection);
            }
            if !self.database.is_null() {
                ffi::duckdb_close(&mut self.database);
            }
        }
    }
}

fn validate_sql(sql: &str) -> Result<(), CliError> {
    let sql =
        CString::new(sql).map_err(|_| CliError::argument("SQL must not contain NUL bytes"))?;
    let mut parser = Parser {
        database: ptr::null_mut(),
        connection: ptr::null_mut(),
        extracted: ptr::null_mut(),
    };
    unsafe {
        if ffi::duckdb_open(ptr::null(), &mut parser.database) != ffi::DuckDBSuccess
            || ffi::duckdb_connect(parser.database, &mut parser.connection) != ffi::DuckDBSuccess
        {
            return Err(CliError::failure(
                "database",
                "Cannot initialize SQL parser",
            ));
        }
        let count =
            ffi::duckdb_extract_statements(parser.connection, sql.as_ptr(), &mut parser.extracted);
        let error = ffi::duckdb_extract_statements_error(parser.extracted);
        if !error.is_null() && !CStr::from_ptr(error).to_bytes().is_empty() {
            return Err(CliError::failure(
                "sql",
                CStr::from_ptr(error).to_string_lossy(),
            ));
        }
        if count != 1 {
            return Err(CliError::argument(format!(
                "Expected exactly one SQL statement; found {count}"
            )));
        }
    }
    Ok(())
}

fn query(args: &[OsString]) -> Result<String, CliError> {
    let options = parse(args)?;
    let sql = match options.sql {
        Some(sql) => sql,
        None => {
            let path = options.sql_file.as_ref().unwrap();
            if path.as_os_str() == "-" {
                let mut sql = String::new();
                std::io::stdin()
                    .read_to_string(&mut sql)
                    .map_err(|e| CliError::failure("io", e))?;
                sql
            } else {
                std::fs::read_to_string(path).map_err(|e| CliError::failure("io", e))?
            }
        }
    };
    validate_sql(&sql)?;
    let conn = open(options.database, options.read_write)?;
    let result = crate::query::run_cli_of(&conn, &sql, options.limit)
        .map_err(|e| CliError::failure("sql", e))?;
    serde_json::to_string(&result).map_err(|e| CliError::failure("output", e))
}

/// The connection every subcommand runs on: its own, never the GUI's, with
/// extension auto-installation off and a file database read-only unless the
/// caller asked for writes.
fn open(database: Option<PathBuf>, read_write: bool) -> Result<Connection, CliError> {
    let config = Config::default()
        .with("autoinstall_known_extensions", "false")
        .map_err(|e| CliError::failure("database", e))?;
    if let Some(path) = database {
        if !read_write {
            let metadata =
                std::fs::metadata(&path).map_err(|e| CliError::failure("database", e))?;
            if !metadata.is_file() {
                return Err(CliError::failure(
                    "database",
                    "--database must name an existing file",
                ));
            }
        }
        let mode = if read_write {
            AccessMode::ReadWrite
        } else {
            AccessMode::ReadOnly
        };
        Connection::open_with_flags(
            path,
            config
                .access_mode(mode)
                .map_err(|e| CliError::failure("database", e))?,
        )
    } else {
        Connection::open_in_memory_with_flags(config)
    }
    .map_err(|e| CliError::failure("database", e))
}

/// `ducklocal profile TARGET [--database PATH]`.
///
/// One positional argument, because the thing being profiled is the whole
/// request; `--database` only says where to look for a name.
fn profile(args: &[OsString]) -> Result<String, CliError> {
    let mut args = args.iter();
    let target = args
        .next()
        .ok_or_else(|| CliError::argument("Name a data file, a table or a view to profile"))?;
    let target = target
        .to_str()
        .ok_or_else(|| CliError::argument("TARGET must be UTF-8"))?;
    if target.starts_with("--") {
        return Err(CliError::argument(
            "The first profile argument is the target, not an option",
        ));
    }
    let mut database = None;
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("--database") => {
                if database.is_some() {
                    return Err(CliError::argument("Duplicate option: --database"));
                }
                let value = args
                    .next()
                    .filter(|value| !value.is_empty() && !value.to_string_lossy().starts_with("--"))
                    .ok_or_else(|| CliError::argument("Missing value for --database"))?;
                if value == ":memory:" {
                    return Err(CliError::argument(
                        "Omit --database for an in-memory database",
                    ));
                }
                database = Some(PathBuf::from(value));
            }
            _ => {
                return Err(CliError::argument(format!(
                    "Unknown profile option: {}",
                    flag.to_string_lossy()
                )))
            }
        }
    }
    // Resolved before the connection is opened: a target that names nothing is
    // a mistake in the arguments, and reporting it as a SQL failure would send
    // a caller looking at their data instead of at their command line.
    let relation = crate::profile::relation_of(target)
        .map_err(|error| CliError::argument(error.to_string()))?;
    let conn = open(database, false)?;
    let profile = crate::profile::run(&conn, target, &relation)
        .map_err(|error| CliError::failure("sql", error))?;
    serde_json::to_string(&profile).map_err(|e| CliError::failure("output", e))
}

pub fn dispatch(args: &[OsString]) -> Option<i32> {
    let first = args.first()?.to_str()?;
    if !matches!(first, "query" | "profile" | "--help" | "--version") && !first.starts_with("--") {
        return None;
    }
    let result = match (first, args.len()) {
        ("--help", 1) => Ok(HELP.to_string()),
        ("--version", 1) => Ok(format!("ducklocal {}", env!("CARGO_PKG_VERSION"))),
        ("query", 2) if args[1] == "--help" => Ok(HELP.to_string()),
        ("query", _) => query(&args[1..]),
        ("profile", 2) if args[1] == "--help" => Ok(HELP.to_string()),
        ("profile", _) => profile(&args[1..]),
        _ => Err(CliError::argument(
            "Unknown or extra arguments; use ducklocal --help",
        )),
    };
    let result = result.and_then(|output| {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{output}")
            .and_then(|_| stdout.flush())
            .map_err(|e| CliError::failure("io", e))
    });
    Some(match result {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "{}",
                json!({"error": {"kind": error.kind, "message": error.message}})
            );
            error.code
        }
    })
}
