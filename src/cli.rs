use std::ffi::{CStr, CString, OsString};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::ptr;

use duckdb::{ffi, AccessMode, Config, Connection};
use serde_json::json;

pub(crate) const HELP: &str = "\
DuckLocal — local data workspace and headless SQL

Usage:
  ducklocal [PATH ...]          Open the GUI on files, folders or globs
  ducklocal open [OPTIONS]      Show SQL or a PATH in the running window
  ducklocal query [OPTIONS]     Run one SQL statement, print JSON
  ducklocal profile TARGET      Per-column statistics, JSON
  ducklocal export --html APP   Export an analysis app as standalone HTML
  ducklocal check FILE          Validate a .dash dashboard spec
  ducklocal lsp                 Language server for .dash files (stdio)
  ducklocal --version

Run `ducklocal <command> --help` for its options.
A GUI path named like a command must be written with ./ (e.g. ./query).
";

pub(crate) const QUERY_HELP: &str = "\
Usage: ducklocal query (--sql SQL | --sql-file FILE) [OPTIONS]

Run one SQL statement and print the result as JSON.

Options:
  --sql SQL          SQL text
  --sql-file FILE    UTF-8 SQL file; - reads stdin
  --database PATH    Existing database file, opened read-only
  --read-write       Allow writes (requires --database)
  --limit N          Maximum rows, default 1000
  --format FORMAT    json (default) or md, a Markdown table for reading

Output: {columns, rows, row_count, truncated, elapsed_ms}, at most 2,000,000
cells. Column types are Arrow names. Errors are JSON on stderr; exit 2 for
arguments, 1 for SQL or I/O. Read-only is not a sandbox: COPY can write files.
";

pub(crate) const PROFILE_HELP: &str = "\
Usage: ducklocal profile TARGET [--database PATH]

Exact per-column statistics as JSON: type, nulls, distinct, min and max, plus
median and decimals for numbers and day coverage for dates.

TARGET is a csv/tsv/parquet/json file, a workbook (its first sheet), or, with
--database, a table or view name.
";

pub(crate) const EXPORT_HELP: &str = "\
Usage: ducklocal export --html [OPTIONS] APP

Run an analysis app (a folder with main.js, or that file) once in a hidden
window and write the data it queried as a self-contained HTML file.

Options:
  --out FILE         Destination, default ./<app folder>.html
  --force            Replace an existing destination
  --database PATH    Existing database file, opened read-only
  --read-write       Allow writes (requires --database)
  --timeout SECONDS  Capture time limit, default 15
";

pub(crate) const CHECK_HELP: &str = "\
Usage: ducklocal check FILE [--database PATH]

Validate a .dash dashboard spec: parse it, resolve its references, and check
each query with DuckDB's parser. Nothing runs unless --database is given; then
every query runs read-only and plot columns are checked against the results.

Exit 2 for spec mistakes, 1 for database or I/O failures.
";

pub(crate) const LSP_HELP: &str = "\
Usage: ducklocal lsp [--database PATH]

Language server for .dash files over stdio, for editors to spawn: the
diagnostics `check` reports, completion, hover and go-to-definition. With
--database, each query's SQL is also checked by DuckDB's parser.
";

#[derive(Debug)]
pub(crate) struct CliError {
    pub(crate) kind: &'static str,
    pub(crate) message: String,
    pub(crate) code: i32,
}

impl CliError {
    pub(crate) fn kind(&self) -> &'static str {
        self.kind
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn code(&self) -> i32 {
        self.code
    }

    pub(crate) fn argument(message: impl Into<String>) -> Self {
        Self {
            kind: "argument",
            message: message.into(),
            code: 2,
        }
    }

    pub(crate) fn failure(kind: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            kind,
            message: error.to_string(),
            code: 1,
        }
    }
}

/// One flag a command accepts: its name and whether it takes a value.
pub(crate) struct FlagSpec {
    pub name: &'static str,
    pub takes_value: bool,
}

/// A walked argument: a recognized flag with its value, or a positional the
/// command interprets itself.
pub(crate) enum Arg {
    Flag(&'static str, Option<OsString>),
    Positional(OsString),
}

/// Walk `args` against `spec` with the checks every command shares —
/// unknown, duplicate, and missing-value errors worded for `command`.
///
/// A value is missing when the next argument is absent, empty, exactly one of
/// the known flags or `--help`, or — unless the flag is in `dash_value_ok`
/// (`--sql`, whose text may legitimately start with dashes) — starts with
/// `--`. Anything not starting with `--` is a positional for the command to
/// handle itself.
pub(crate) fn parse_args(
    command: &str,
    args: &[OsString],
    spec: &[FlagSpec],
    dash_value_ok: &[&str],
) -> Result<Vec<Arg>, CliError> {
    let mut parsed = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let text = arg
            .to_str()
            .ok_or_else(|| CliError::argument("Option names must be UTF-8"))?;
        if !text.starts_with("--") {
            parsed.push(Arg::Positional(arg.clone()));
            continue;
        }
        let Some(flag) = spec.iter().find(|flag| flag.name == text) else {
            return Err(CliError::argument(format!("Unknown {command} option: {text}")));
        };
        if !seen.insert(flag.name) {
            return Err(CliError::argument(format!("Duplicate option: {}", flag.name)));
        }
        if !flag.takes_value {
            parsed.push(Arg::Flag(flag.name, None));
            continue;
        }
        let value = args
            .next()
            .filter(|value| !value.is_empty())
            .filter(|value| {
                let text = value.to_str();
                text != Some("--help") && !spec.iter().any(|flag| Some(flag.name) == text)
            })
            .filter(|value| {
                dash_value_ok.contains(&flag.name) || !value.to_string_lossy().starts_with("--")
            })
            .ok_or_else(|| CliError::argument(format!("Missing value for {}", flag.name)))?;
        parsed.push(Arg::Flag(flag.name, Some(value.clone())));
    }
    Ok(parsed)
}

/// The path half of `--database`: `:memory:` is the default, not a value.
pub(crate) fn database_path(value: &OsString) -> Result<PathBuf, CliError> {
    if value == ":memory:" {
        return Err(CliError::argument(
            "Omit --database for an in-memory database",
        ));
    }
    Ok(PathBuf::from(value))
}

#[derive(Debug)]
struct Options {
    sql: Option<String>,
    sql_file: Option<PathBuf>,
    database: Option<PathBuf>,
    read_write: bool,
    limit: usize,
    format: Format,
}

/// How a result is written to stdout.
///
/// `Json` is the contract a caller parses; `Markdown` is the same result
/// rendered to be read — by a person, or by an agent writing it into a
/// document. The values are the same values; only the framing differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Json,
    Markdown,
}

const QUERY_SPEC: &[FlagSpec] = &[
    FlagSpec {
        name: "--sql",
        takes_value: true,
    },
    FlagSpec {
        name: "--sql-file",
        takes_value: true,
    },
    FlagSpec {
        name: "--database",
        takes_value: true,
    },
    FlagSpec {
        name: "--read-write",
        takes_value: false,
    },
    FlagSpec {
        name: "--limit",
        takes_value: true,
    },
    FlagSpec {
        name: "--format",
        takes_value: true,
    },
];

const PROFILE_SPEC: &[FlagSpec] = &[FlagSpec {
    name: "--database",
    takes_value: true,
}];

fn parse(args: &[OsString]) -> Result<Options, CliError> {
    let mut options = Options {
        sql: None,
        sql_file: None,
        database: None,
        read_write: false,
        limit: 1000,
        format: Format::Json,
    };
    for arg in parse_args("query", args, QUERY_SPEC, &["--sql"])? {
        match arg {
            Arg::Flag("--sql", Some(value)) => {
                options.sql = Some(
                    value
                        .to_str()
                        .ok_or_else(|| CliError::argument("SQL must be UTF-8"))?
                        .to_string(),
                );
            }
            Arg::Flag("--sql-file", Some(value)) => options.sql_file = Some(value.into()),
            Arg::Flag("--database", Some(value)) => {
                options.database = Some(database_path(&value)?);
            }
            Arg::Flag("--read-write", None) => options.read_write = true,
            Arg::Flag("--format", Some(value)) => {
                options.format = match value.to_str() {
                    Some("json") => Format::Json,
                    Some("md") => Format::Markdown,
                    _ => return Err(CliError::argument("--format must be json or md")),
                };
            }
            Arg::Flag("--limit", Some(value)) => {
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
            Arg::Positional(value) => {
                return Err(CliError::argument(format!(
                    "Unknown query option: {}",
                    value.to_string_lossy()
                )));
            }
            _ => {
                return Err(CliError::failure(
                    "internal",
                    "the argument walker produced a flag query does not declare",
                ));
            }
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

/// One raw DuckDB pointer per guard: the moment a pointer exists it has an
/// owner, so no early return — present or future — can leak it. Wrapped
/// immediately after the call that produces the pointer, whether or not the
/// call succeeded, because a failed `duckdb_open`/`duckdb_connect` may still
/// leave something to destroy behind. Locals drop in reverse declaration
/// order, which is the required destroy order.
struct ParserDatabase(ffi::duckdb_database);

impl Drop for ParserDatabase {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                ffi::duckdb_close(&mut self.0);
            }
        }
    }
}

struct ParserConnection(ffi::duckdb_connection);

impl Drop for ParserConnection {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                ffi::duckdb_disconnect(&mut self.0);
            }
        }
    }
}

struct ExtractedStatements(ffi::duckdb_extracted_statements);

impl Drop for ExtractedStatements {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                ffi::duckdb_destroy_extracted(&mut self.0);
            }
        }
    }
}

/// Parse-only validation: this must never execute SQL. The real DuckDB parser
/// runs on a throwaway in-memory connection, so a multi-statement script
/// (say `COPY ... TO ...; SELECT 2`) is rejected before anything has a chance
/// to run it.
pub(crate) fn validate_sql(sql: &str) -> Result<(), CliError> {
    let sql =
        CString::new(sql).map_err(|_| CliError::argument("SQL must not contain NUL bytes"))?;
    unsafe {
        let mut raw = ptr::null_mut();
        let opened = ffi::duckdb_open(ptr::null(), &mut raw);
        let database = ParserDatabase(raw);
        if opened != ffi::DuckDBSuccess {
            return Err(CliError::failure("database", "Cannot initialize SQL parser"));
        }
        let mut raw = ptr::null_mut();
        let connected = ffi::duckdb_connect(database.0, &mut raw);
        let connection = ParserConnection(raw);
        if connected != ffi::DuckDBSuccess {
            return Err(CliError::failure("database", "Cannot initialize SQL parser"));
        }
        let mut raw = ptr::null_mut();
        let count = ffi::duckdb_extract_statements(connection.0, sql.as_ptr(), &mut raw);
        let extracted = ExtractedStatements(raw);
        if extracted.0.is_null() {
            return Err(CliError::failure("sql", "Cannot parse the SQL"));
        }
        let error = ffi::duckdb_extract_statements_error(extracted.0);
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
            let Some(path) = options.sql_file.as_ref() else {
                return Err(CliError::failure(
                    "internal",
                    "argument parsing accepted neither --sql nor --sql-file",
                ));
            };
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
    match options.format {
        // Trailing newline is not part of the document; `dispatch` adds the
        // one line ending every output gets.
        Format::Markdown => Ok(crate::query::format_markdown(&result)
            .trim_end()
            .to_string()),
        Format::Json => serde_json::to_string(&result).map_err(|e| CliError::failure("output", e)),
    }
}

/// The connection every subcommand runs on: its own, never the GUI's, with
/// extension auto-installation off and a file database read-only unless the
/// caller asked for writes.
pub(crate) fn open(database: Option<PathBuf>, read_write: bool) -> Result<Connection, CliError> {
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
    let (target, rest) = args
        .split_first()
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
    for arg in parse_args("profile", rest, PROFILE_SPEC, &[])? {
        match arg {
            Arg::Flag("--database", Some(value)) => database = Some(database_path(&value)?),
            Arg::Positional(value) => {
                return Err(CliError::argument(format!(
                    "Unknown profile option: {}",
                    value.to_string_lossy()
                )));
            }
            _ => {
                return Err(CliError::failure(
                    "internal",
                    "the argument walker produced a flag profile does not declare",
                ));
            }
        }
    }
    let conn = open(database, false)?;
    // A target that names nothing is a mistake in the arguments, and reporting
    // it as a SQL failure would send a caller looking at their data instead of
    // at their command line — it is still an argument error even though
    // resolution now needs the connection: a workbook has no table function,
    // so its first sheet is imported into the connection to be profiled.
    let relation = crate::profile::relation_of(&conn, target)
        .map_err(|error| CliError::argument(error.to_string()))?;
    let profile = crate::profile::run(&conn, target, &relation)
        .map_err(|error| CliError::failure("sql", error))?;
    serde_json::to_string(&profile).map_err(|e| CliError::failure("output", e))
}

pub fn dispatch(args: &[OsString]) -> Option<i32> {
    let first = args.first()?.to_str()?;
    if !matches!(
        first,
        "query" | "profile" | "open" | "export" | "check" | "dash" | "lsp" | "--help" | "--version"
    ) && !first.starts_with("--")
    {
        return None;
    }
    // The language server's stdio is a frame stream, not a document: it
    // handles its own output and exit code, so it never reaches the writeln
    // path below.
    if first == "lsp" && args.get(1).map(|arg| arg.as_os_str()) != Some(std::ffi::OsStr::new("--help")) {
        return Some(crate::spec::lsp::main(&args[1..]));
    }
    let result = match (first, args.len()) {
        ("--help", 1) => Ok(HELP.to_string()),
        ("--version", 1) => Ok(format!("ducklocal {}", env!("CARGO_PKG_VERSION"))),
        ("query", 2) if args[1] == "--help" => Ok(QUERY_HELP.to_string()),
        ("query", _) => query(&args[1..]),
        ("profile", 2) if args[1] == "--help" => Ok(PROFILE_HELP.to_string()),
        ("profile", _) => profile(&args[1..]),
        ("open", 2) if args[1] == "--help" => Ok(crate::remote::OPEN_HELP.to_string()),
        ("open", _) => crate::remote::open(&args[1..]),
        ("export", _) => crate::app_export::dispatch(&args[1..]),
        ("check", 2) if args[1] == "--help" => Ok(CHECK_HELP.to_string()),
        ("check", _) => crate::spec::check(&args[1..]),
        ("lsp", 2) if args[1] == "--help" => Ok(LSP_HELP.to_string()),
        // `dash export` was the app's HTML export before the command was
        // renamed; `dash` alone now means the .dash spec format. Saying so
        // beats the fallback, which would open a GUI on a path named "dash".
        ("dash", _) => Err(CliError::argument(
            "`dash export` is now `export`: ducklocal export --html APP",
        )),
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
