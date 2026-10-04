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
  ducklocal query [OPTIONS]     Run one SQL statement, print JSON
  ducklocal schema [PATH ...]   Relations and columns; --stats per column
  ducklocal open [OPTIONS]      Show work in the running window; --state reads it
  ducklocal check FILE          Validate a .dash dashboard spec
  ducklocal export --html APP   Export an analysis app as standalone HTML
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

Deprecated: the same as `ducklocal schema TARGET --stats`.

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

For editors, `ducklocal lsp [--database PATH]` serves the same diagnostics,
with completion, hover and go-to-definition, over stdio.
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
    /// What to do about it, when that is known: the next command to run or
    /// the flag to add. An agent reading the error acts on this line rather
    /// than guessing from DuckDB's message; `dispatch` fills it in from
    /// [`hint_for`] when the error site did not.
    pub(crate) hint: Option<String>,
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
            hint: None,
        }
    }

    pub(crate) fn failure(kind: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            kind,
            message: error.to_string(),
            code: 1,
            hint: None,
        }
    }

    pub(crate) fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// The next step for an error whose site gave none, read off its message.
///
/// DuckDB's messages say what went wrong; these say what to run next, in this
/// CLI's own terms — which flag, which command. Only the mistakes an agent
/// actually makes are here, and each hint is a thing to do, never a restating
/// of the message.
pub(crate) fn hint_for(kind: &str, message: &str) -> Option<&'static str> {
    let has = |needle: &str| message.contains(needle);
    if kind == "sql" || kind == "database" {
        if has("read-only mode") || has("read-only database") {
            return Some("File databases open read-only; add --read-write to allow writes");
        }
        if has("Table with name") && has("does not exist") {
            return Some(
                "List what exists with `ducklocal schema` (with --database PATH for a database \
                 file); read a data file by quoting its path: FROM 'data.csv'",
            );
        }
        if has("Referenced column") || has("Candidate bindings") {
            return Some(
                "See a relation's columns with `ducklocal schema PATH` or DESCRIBE; double-quote \
                 names with spaces or capitals",
            );
        }
        if has("No files found") {
            return Some(
                "Relative paths resolve against the current directory; check the name, or use an \
                 absolute path",
            );
        }
        if has("Could not convert") || has("Conversion Error") {
            return Some(
                "TRY_CAST(expr AS type) turns values that do not convert into NULL; \
                 `ducklocal schema PATH --stats` shows what the column holds",
            );
        }
    }
    if kind == "argument" && has("Expected exactly one SQL statement") {
        return Some(
            "Run one statement per call: chain steps with WITH … AS (…), or make separate calls",
        );
    }
    None
}

/// The known flag an unknown one was most likely meant to be: one or two
/// edits away, or the same name with a different number of dashes.
fn closest_flag<'a>(text: &str, spec: &'a [FlagSpec]) -> Option<&'a str> {
    let bare = text.trim_start_matches('-');
    spec.iter()
        .map(|flag| (flag.name, edit_distance(bare, flag.name.trim_start_matches('-'))))
        .filter(|(_, distance)| *distance <= 2)
        .min_by_key(|(_, distance)| *distance)
        .map(|(name, _)| name)
}

/// Levenshtein distance, over chars; flag names are short.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitute = previous + usize::from(ca != *cb);
            previous = row[j + 1];
            row[j + 1] = substitute.min(row[j] + 1).min(previous + 1);
        }
    }
    row[b.len()]
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
            let error = CliError::argument(format!("Unknown {command} option: {text}"));
            return Err(match closest_flag(text, spec) {
                Some(name) => error.with_hint(format!("Did you mean {name}?")),
                None => error.with_hint(format!("See ducklocal {command} --help")),
            });
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

#[hotpath::measure]
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
    let output = match options.format {
        // Trailing newline is not part of the document; `dispatch` adds the
        // one line ending every output gets.
        Format::Markdown => Ok(crate::query::format_markdown(&result)
            .trim_end()
            .to_string()),
        Format::Json => hotpath::measure_block!("cli::serialize_json", {
            serde_json::to_string(&result)
        })
        .map_err(|e| CliError::failure("output", e)),
    };
    // The process exits right after printing. Freeing up to 2M cells one by
    // one, each an object for a DECIMAL, DATE or TIMESTAMP, took as long as
    // building them; the OS reclaims the memory at exit for free.
    std::mem::forget(result);
    output
}

/// The connection every subcommand runs on: its own, never the GUI's, with
/// extension auto-installation off and a file database read-only unless the
/// caller asked for writes.
#[hotpath::measure]
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

/// `ducklocal profile TARGET [--database PATH]`: the deprecated spelling of
/// `schema TARGET --stats`, kept so released scripts keep working.
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
    profile_target(target, database)
}

/// Exact per-column statistics of one relation: a data file, a workbook's
/// first sheet, or — with a database — a table or view name.
pub(crate) fn profile_target(target: &str, database: Option<PathBuf>) -> Result<String, CliError> {
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
        "query"
            | "schema"
            | "profile"
            | "open"
            | "export"
            | "check"
            | "dash"
            | "lsp"
            | "--help"
            | "--version"
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
        ("schema", 2) if args[1] == "--help" => Ok(crate::schema_cli::SCHEMA_HELP.to_string()),
        ("schema", _) => crate::schema_cli::run(&args[1..]),
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
            let hint = error
                .hint
                .clone()
                .or_else(|| hint_for(error.kind, &error.message).map(str::to_string));
            let mut body = json!({"kind": error.kind, "message": error.message});
            if let Some(hint) = hint {
                body["hint"] = json!(hint);
            }
            let _ = writeln!(std::io::stderr().lock(), "{}", json!({"error": body}));
            error.code
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_misspelled_flag_is_matched_to_the_one_meant() {
        assert_eq!(closest_flag("--limt", QUERY_SPEC), Some("--limit"));
        assert_eq!(closest_flag("-sql", QUERY_SPEC), Some("--sql"));
        assert_eq!(closest_flag("--databse", QUERY_SPEC), Some("--database"));
        assert_eq!(closest_flag("--everything", QUERY_SPEC), None);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn hints_follow_the_message_and_the_kind() {
        let read_only = "Invalid Input Error: Cannot execute statement of type \"DROP\" on database \"w\" which is attached in read-only mode!";
        assert!(hint_for("sql", read_only).unwrap().contains("--read-write"));
        assert!(hint_for("sql", "Catalog Error: Table with name x does not exist!")
            .unwrap()
            .contains("ducklocal schema"));
        assert!(hint_for("argument", "Expected exactly one SQL statement; found 2").is_some());
        // A kind the hint is not about gets none.
        assert!(hint_for("io", "Table with name x does not exist").is_none());
        assert!(hint_for("sql", "Parser Error: syntax error at or near \"x\"").is_none());
    }
}
