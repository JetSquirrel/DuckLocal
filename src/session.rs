//! An analysis session: a local record of how a piece of work got where it did,
//! so a person or the next agent can see what a conclusion rests on and whether
//! that still holds.
//!
//! It is one JSON Lines file, named by whoever wants one (`DUCKLOCAL_SESSION`),
//! so nothing is recorded unless asked for, there is no central store, and it
//! can sit beside the work. Each command that runs SQL or changes a dashboard
//! appends what it did.
//!
//! What is kept is the lineage, never the data: SQL and spec text, references
//! to sources with their size and modification time, and the shape of each
//! result, with how complete it was. No rows. Text that may hold credentials is
//! replaced before it reaches the file. `origin` is what the environment says
//! and nothing checks it.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::cli::{parse_args, Arg, CliError, FlagSpec};

const ENV: &str = "DUCKLOCAL_SESSION";
/// The format of the file, on its first line.
const VERSION: u32 = 1;
/// The longest an error runs in the file: DuckDB quotes values in some of them,
/// and those are data.
const MAX_ERROR_CHARS: usize = 500;
/// The largest spec text kept; a larger one keeps only its hash.
const MAX_SPEC_BYTES: usize = 256 * 1024;
/// How much SQL `show` prints per event; `get` has the rest.
const SHOWN_SQL_CHARS: usize = 2_000;
const DEFAULT_SHOWN_EVENTS: usize = 50;
/// How many files of a glob get looked at.
const MAX_GLOB_FILES: usize = 10_000;

pub(crate) const REDACTED: &str = "[not recorded: may hold credentials]";

fn active() -> Option<PathBuf> {
    std::env::var_os(ENV)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

fn capped(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

/// Whether text may hold a credential. Errs towards keeping it out: a column
/// called `secret_santa` costs a query's text, and a key in the file costs more.
pub(crate) fn may_hold_credentials(text: &str) -> bool {
    let lower = text.to_lowercase();
    if [
        "secret",
        "password",
        "passwd",
        "key_id",
        "access_key",
        "api_key",
        "apikey",
        "token",
        "credential",
    ]
    .iter()
    .any(|word| lower.contains(word))
    {
        return true;
    }
    // user:password@host in a connection string or URL.
    lower.split("://").skip(1).any(|rest| {
        let authority = rest.split(['/', ' ', '\'', '"']).next().unwrap_or("");
        authority.contains('@') && authority.contains(':')
    })
}

/// `text` for the file, and its hash so a later revision can be told apart;
/// neither when it may hold credentials, since a hash of a short secret can be
/// guessed.
fn lineage_text(text: &str) -> (String, Option<String>) {
    if may_hold_credentials(text) {
        (REDACTED.to_string(), None)
    } else {
        (text.to_string(), Some(hash(text.as_bytes())))
    }
}

/// A file's identity for telling later whether it changed: never its contents.
fn fingerprint(path: &Path) -> Value {
    match std::fs::metadata(path) {
        Ok(metadata) => json!({
            "exists": true,
            "size": metadata.len(),
            "modified": metadata
                .modified()
                .ok()
                .map(|time| chrono::DateTime::<chrono::Local>::from(time).to_rfc3339()),
        }),
        Err(_) => json!({"exists": false}),
    }
}

fn is_remote(reference: &str) -> bool {
    reference.contains("://")
}

fn is_glob(reference: &str) -> bool {
    reference.contains(['*', '?', '['])
}

/// A glob's identity: how many files, how much, and the latest change.
fn glob_fingerprint(pattern: &str) -> Value {
    let files: Vec<String> = duckdb::Connection::open_in_memory()
        .and_then(|conn| {
            let mut statement = conn.prepare("SELECT file FROM glob(?) ORDER BY file LIMIT ?")?;
            let rows = statement
                .query_map(duckdb::params![pattern, MAX_GLOB_FILES as i64], |row| {
                    row.get(0)
                })?
                .collect();
            rows
        })
        .unwrap_or_default();
    let mut size = 0u64;
    let mut latest = None;
    for file in &files {
        if let Ok(metadata) = std::fs::metadata(file) {
            size += metadata.len();
            latest = latest.max(metadata.modified().ok());
        }
    }
    json!({
        "exists": !files.is_empty(),
        "files": files.len(),
        "size": size,
        "modified": latest.map(|time| chrono::DateTime::<chrono::Local>::from(time).to_rfc3339()),
    })
}

/// An address only, without anything that may be a key in the query string or
/// a password before the host.
pub(crate) fn safe_address(address: &str) -> String {
    address
        .split('?')
        .next()
        .filter(|address| !may_hold_credentials(address))
        .unwrap_or(REDACTED)
        .to_string()
}

/// A source as recorded: what it was called, where that is, and what it was.
fn source_entry(reference: &str, cwd: &Path) -> Value {
    if is_remote(reference) {
        return json!({"ref": safe_address(reference), "remote": true});
    }
    let path = cwd.join(reference).to_string_lossy().replace('\\', "/");
    let print = if is_glob(reference) {
        glob_fingerprint(&path)
    } else {
        fingerprint(Path::new(&path))
    };
    json!({"ref": reference, "path": path, "seen": print})
}

/// How a recorded source compares with now: `same`, `changed`, `missing`, or
/// `unknown` for what cannot be looked at from here.
fn source_status(source: &Value) -> &'static str {
    let Some(path) = source["path"].as_str() else {
        return "unknown";
    };
    let now = if is_glob(source["ref"].as_str().unwrap_or("")) {
        glob_fingerprint(path)
    } else {
        fingerprint(Path::new(path))
    };
    if now["exists"] != json!(true) {
        "missing"
    } else if now == source["seen"] {
        "same"
    } else {
        "changed"
    }
}

/// Every file and table a SELECT reads, by DuckDB's parser. `None` for a
/// statement the parser will not serialize: only a SELECT serializes.
fn references_of(sql: &str) -> Option<Vec<String>> {
    reads_of(sql).map(|reads| reads.files_and_tables)
}

/// What a SELECT names, by DuckDB's parser.
pub(crate) struct Reads {
    /// Sorted and without duplicates; a name a WITH defines is no table.
    pub files_and_tables: Vec<String>,
    /// The catalogs and schemas tables were qualified with.
    pub qualifiers: Vec<String>,
    /// Those of the names given with no qualifier.
    pub bare: Vec<String>,
    /// A table function that is not a known reader, which may read anything.
    pub other_function: bool,
}

pub(crate) fn reads_of(sql: &str) -> Option<Reads> {
    let conn = duckdb::Connection::open_in_memory().ok()?;
    let text: String = conn
        .query_row("SELECT json_serialize_sql(?::VARCHAR)", [sql], |row| {
            row.get(0)
        })
        .ok()?;
    let tree: Value = serde_json::from_str(&text).ok()?;
    if tree["error"] == json!(true) {
        return None;
    }
    let mut walk = References::default();
    walk.visit(&tree);
    if walk.opaque {
        return None;
    }
    // A name a WITH defines is no table.
    let mut found = walk.found;
    found.retain(|name| !walk.defined.contains(name));
    found.sort();
    found.dedup();
    let mut bare = walk.bare;
    bare.retain(|name| !walk.defined.contains(name));
    walk.qualifiers.sort();
    walk.qualifiers.dedup();
    Some(Reads {
        files_and_tables: found,
        qualifiers: walk.qualifiers,
        bare,
        other_function: walk.other_function,
    })
}

#[derive(Default)]
struct References {
    found: Vec<String>,
    /// The names WITH clauses define.
    defined: Vec<String>,
    /// A reader takes something other than one path, such as a list or an
    /// expression: better to say the sources are unknown than to list some.
    opaque: bool,
    qualifiers: Vec<String>,
    bare: Vec<String>,
    other_function: bool,
}

impl References {
    fn visit(&mut self, node: &Value) {
        match node {
            Value::Object(map) => {
                let kind = map.get("type").and_then(Value::as_str);
                if kind == Some("BASE_TABLE") {
                    if let Some(name) = map.get("table_name").and_then(Value::as_str) {
                        self.found.push(name.to_string());
                        let qualifiers: Vec<&str> = ["catalog_name", "schema_name"]
                            .iter()
                            .filter_map(|field| map.get(*field).and_then(Value::as_str))
                            .filter(|qualifier| !qualifier.is_empty())
                            .collect();
                        if qualifiers.is_empty() {
                            self.bare.push(name.to_string());
                        }
                        self.qualifiers
                            .extend(qualifiers.into_iter().map(str::to_string));
                    }
                }
                if kind == Some("TABLE_FUNCTION") {
                    let function = &map["function"];
                    let name = function["function_name"].as_str().unwrap_or("");
                    if name.starts_with("read_") || name.ends_with("_scan") {
                        let first = &function["arguments"][0]["expression"]["literal"];
                        match first["text"].as_str() {
                            Some(path) => self.found.push(path.to_string()),
                            None => self.opaque = true,
                        }
                    } else if !matches!(name, "range" | "generate_series" | "unnest") {
                        self.other_function = true;
                    }
                }
                if let Some(defined) = map.get("cte_map").and_then(|cte| cte["map"].as_array()) {
                    self.defined.extend(
                        defined
                            .iter()
                            .filter_map(|cte| cte["key"].as_str().map(str::to_string)),
                    );
                }
                map.values().for_each(|child| self.visit(child));
            }
            Value::Array(items) => items.iter().for_each(|item| self.visit(item)),
            _ => {}
        }
    }
}

/// A name that reads a file rather than a table: as DuckDB would take it.
pub(crate) fn names_a_file(reference: &str, cwd: &Path) -> bool {
    is_remote(reference)
        || is_glob(reference)
        || reference.contains(['/', '\\'])
        || Path::new(reference).extension().is_some()
        || cwd.join(reference).is_file()
}

/// The versions behind a result, where they can be found.
fn engine() -> Value {
    let found = crate::cli::open(None, false).ok().map(|conn| {
        let version: Option<String> = conn
            .query_row("SELECT version()", [], |row| row.get(0))
            .ok();
        let extensions: Vec<Value> = conn
            .prepare(
                "SELECT extension_name, extension_version FROM duckdb_extensions() \
                 WHERE loaded ORDER BY extension_name",
            )
            .and_then(|mut statement| {
                statement
                    .query_map([], |row| {
                        Ok(json!({
                            "name": row.get::<_, String>(0)?,
                            "version": row.get::<_, Option<String>>(1)?,
                        }))
                    })?
                    .collect()
            })
            .unwrap_or_default();
        (version, extensions)
    });
    let (duckdb, extensions) = found.unwrap_or((None, Vec::new()));
    json!({
        "ducklocal": env!("CARGO_PKG_VERSION"),
        "duckdb": duckdb,
        "extensions": extensions,
    })
}

/// Whether `path` is a session file: its first line says so. Nothing else is
/// written to or removed.
fn is_session_file(path: &Path) -> bool {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut first = String::new();
    if std::io::BufReader::new(file).read_line(&mut first).is_err() {
        return false;
    }
    serde_json::from_str::<Value>(&first).is_ok_and(|line| line.get("ducklocal_session").is_some())
}

fn append_to(path: &Path, mut event: Value) -> Result<u64, String> {
    let describe = |e: std::io::Error| format!("{}: {e}", path.display());
    let exists = path.exists();
    if exists && !is_session_file(path) {
        return Err(format!(
            "{} is not a session file, so nothing was written to it",
            path.display()
        ));
    }
    let seq = if exists {
        std::fs::read_to_string(path)
            .map_err(describe)?
            .lines()
            .count() as u64
    } else {
        1
    };
    event["seq"] = json!(seq);
    event["at"] = json!(now());
    event["origin"] = json!(crate::spec::presence::agent_name());
    let mut line = String::new();
    if !exists {
        line.push_str(&json!({"ducklocal_session": VERSION, "created_at": now()}).to_string());
        line.push('\n');
    }
    line.push_str(&event.to_string());
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(describe)?;
    // One write, so two commands at once do not interleave within a line.
    file.write_all(line.as_bytes()).map_err(describe)?;
    Ok(seq)
}

/// Record `event` in the active session, if there is one. A record that cannot
/// be written must not fail the work it describes, or pass unnoticed.
fn record(event: Value) {
    let Some(path) = active() else { return };
    if let Err(message) = append_to(&path, event) {
        eprintln!(
            "{}",
            json!({"warning": format!("Session not recorded: {message}")})
        );
    }
}

/// `ducklocal query` ran, or was refused.
pub(crate) fn record_query(
    sql: &str,
    database: Option<&Path>,
    restricted: bool,
    outcome: Result<&crate::query::CliResult, &CliError>,
) {
    if active().is_none() {
        return;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let (text, sql_hash) = lineage_text(sql);
    let references = if sql_hash.is_some() {
        references_of(sql)
    } else {
        None
    };
    let sources: Vec<Value> = references
        .iter()
        .flatten()
        .filter(|reference| names_a_file(reference, &cwd))
        .map(|reference| source_entry(reference, &cwd))
        .collect();
    let tables: Vec<&String> = references
        .iter()
        .flatten()
        .filter(|reference| !names_a_file(reference, &cwd))
        .collect();
    let mut event = json!({
        "kind": "query",
        "sql": text,
        "sql_hash": sql_hash,
        "sources": sources,
        "tables": tables,
        // Only a SELECT can be read for what it touches.
        "sources_known": references.is_some(),
        "database": database.map(|path| json!({
            "path": cwd.join(path).to_string_lossy().replace('\\', "/"),
            "seen": fingerprint(path),
        })),
        "restricted": restricted,
        "engine": engine(),
    });
    match outcome {
        Ok(result) => {
            event["status"] = json!("ok");
            event["columns"] = json!(result
                .columns
                .iter()
                .map(|column| json!({"name": column.name, "type": column.arrow_type}))
                .collect::<Vec<_>>());
            event["row_count"] = json!(result.row_count);
            event["truncated"] = json!(result.truncated);
            event["total_rows"] = json!(result.total_rows);
            event["total_rows_exact"] = json!(result.total_rows_exact);
            event["complete"] = json!(!result.truncated && result.total_rows_exact);
        }
        Err(error) => {
            event["status"] = json!("failed");
            event["error_kind"] = json!(error.kind);
            event["error"] = json!(capped(&error.message, MAX_ERROR_CHARS));
            event["complete"] = json!(false);
        }
    }
    record(event);
}

/// A spec's text for the file, unless it is too large or may hold credentials.
fn spec_revision(source: &str) -> Value {
    let (text, text_hash) = lineage_text(source);
    json!({
        "hash": text_hash,
        "text": (source.len() <= MAX_SPEC_BYTES).then_some(text),
    })
}

/// The sources a spec names, resolved as the dashboard resolves them.
fn spec_sources(file: &Path, source: &str) -> Vec<Value> {
    use crate::spec::{model, source as spec_source};
    let Ok(parsed) = model::parse(source) else {
        return Vec::new();
    };
    let (spec, _) = model::validate_partial(&parsed);
    let base = spec_source::base_of(file);
    spec.sources
        .iter()
        .map(|named| {
            let resolved = spec_source::resolve(&named.path, &base);
            source_entry(&resolved.to_string_lossy(), Path::new(""))
        })
        .collect()
}

/// `ducklocal check` ran. `report` is its output, in either form.
pub(crate) fn record_check(
    file: &Path,
    source: &str,
    restricted: bool,
    report: Result<&str, &CliError>,
) {
    if active().is_none() {
        return;
    }
    let names = |report: &Value, field: &str| -> Vec<String> {
        let items = report[field].as_array().into_iter().flatten();
        items
            .filter_map(|item| {
                item.as_str()
                    .or_else(|| item["query"].as_str())
                    .map(str::to_string)
            })
            .collect()
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut event = json!({
        "kind": "check",
        "restricted": restricted,
        "spec": cwd.join(file).to_string_lossy().replace('\\', "/"),
        "revision": spec_revision(source),
        "sources": spec_sources(file, source),
        "engine": engine(),
    });
    match report {
        Err(error) => {
            event["status"] = json!("failed");
            event["error_kind"] = json!(error.kind);
            event["error"] = json!(capped(&error.message, MAX_ERROR_CHARS));
            event["complete"] = json!(false);
        }
        Ok(text) => {
            let report: Value = serde_json::from_str(text).unwrap_or_default();
            let unresolved = names(&report, "unresolved");
            let truncated: Vec<String> = match report["truncated"].as_array() {
                Some(_) => names(&report, "truncated"),
                None => report["queries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|query| query["truncated"] == json!(true))
                    .filter_map(|query| query["name"].as_str().map(str::to_string))
                    .collect(),
            };
            event["status"] = json!("ok");
            event["complete"] = json!(unresolved.is_empty() && truncated.is_empty());
            event["unresolved"] = json!(unresolved);
            event["empty"] = json!(names(&report, "empty"));
            event["truncated"] = json!(truncated);
        }
    }
    record(event);
}

/// `ducklocal propose` staged a change.
pub(crate) fn record_proposal(file: &Path, proposal: &crate::spec::proposal::Proposal, ok: bool) {
    if active().is_none() {
        return;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    record(json!({
        "kind": "proposal",
        "spec": cwd.join(file).to_string_lossy().replace('\\', "/"),
        "id": proposal.id,
        "base_hash": proposal.base_hash,
        "revision": spec_revision(&proposal.proposed),
        "note": proposal.note,
        "status": "pending",
        "check_ok": ok,
        // Only that it was proposed: `show` reads how it ended from the spec.
        "complete": false,
    }));
}

/// `ducklocal open` handed the window a request.
pub(crate) fn record_open(request: &crate::remote::Request, output: Result<&Value, &CliError>) {
    if active().is_none() {
        return;
    }
    let (text, sql_hash) = match &request.sql {
        Some(sql) => {
            let (text, sql_hash) = lineage_text(sql);
            (Some(text), sql_hash)
        }
        None => (None, None),
    };
    let mut event = json!({
        "kind": "open",
        "request_id": request.request_id,
        "sql": text,
        "sql_hash": sql_hash,
        "paths": request.paths,
        "run": request.run,
        "based_on_revision": request.based_on_revision,
        // What the window did is known only if the command waited to hear.
        "complete": false,
    });
    match output {
        Ok(output) => {
            event["status"] = output.get("status").cloned().unwrap_or(json!("delivered"));
            event["result_status"] = output.get("result_status").cloned().unwrap_or(Value::Null);
            event["revision"] = output.get("revision").cloned().unwrap_or(Value::Null);
        }
        Err(error) => {
            event["status"] = json!("failed");
            event["error_kind"] = json!(error.kind);
            event["error"] = json!(capped(&error.message, MAX_ERROR_CHARS));
        }
    }
    record(event);
}

/// Take rows out of a window state, wherever they are.
fn without_rows(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("preview");
            map.values_mut().for_each(without_rows);
        }
        Value::Array(items) => items.iter_mut().for_each(without_rows),
        _ => {}
    }
}

fn read_events(path: &Path) -> Result<(Value, Vec<Value>), CliError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| CliError::failure("io", format!("{}: {e}", path.display())))?;
    let mut lines = text.lines();
    let header: Value = lines
        .next()
        .and_then(|line| serde_json::from_str(line).ok())
        .filter(|line: &Value| line.get("ducklocal_session").is_some())
        .ok_or_else(|| not_a_session(path))?;
    // A line that does not read is a write that was cut off: said, not hidden.
    let events = lines
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|_| json!({"kind": "unreadable", "complete": false}))
        })
        .collect();
    Ok((header, events))
}

fn not_a_session(path: &Path) -> CliError {
    CliError::failure(
        "not_session",
        format!("{} is not a session file", path.display()),
    )
}

/// What about `event` no longer holds, or never did. Empty means nothing is
/// known against it, not that it is proved right.
fn doubts(event: &Value) -> Vec<String> {
    let mut doubts = Vec::new();
    match event["status"].as_str() {
        Some("failed") => doubts.push("it failed".to_string()),
        Some("stale") => doubts.push("the window refused it as stale".to_string()),
        _ => {}
    }
    if event["truncated"] == json!(true) {
        doubts.push(format!(
            "only {} of {}{} rows were returned",
            event["row_count"],
            if event["total_rows_exact"] == json!(true) {
                ""
            } else {
                "at least "
            },
            event["total_rows"]
        ));
    }
    for (field, words) in [
        ("unresolved", "not checked, reading unavailable tables"),
        ("truncated", "cut off by the row budget"),
    ] {
        if let Some(names) = event[field].as_array().filter(|names| !names.is_empty()) {
            let names: Vec<&str> = names.iter().filter_map(Value::as_str).collect();
            doubts.push(format!("{words}: {}", names.join(", ")));
        }
    }
    if event["sources_known"] == json!(false) && event["status"] != json!("failed") {
        doubts.push(if event["sql"] == json!(REDACTED) {
            "its SQL was not kept because it may hold credentials, so its sources are unknown"
                .to_string()
        } else {
            "its sources could not be read from the SQL".to_string()
        });
    }
    for source in event["sources"].as_array().into_iter().flatten() {
        let name = source["ref"].as_str().unwrap_or("?");
        match source_status(source) {
            "changed" => doubts.push(format!("{name} has changed since")),
            "missing" => doubts.push(format!("{name} is no longer there")),
            _ => {}
        }
    }
    if let Some(database) = event.get("database").filter(|d| !d.is_null()) {
        let path = database["path"].as_str().unwrap_or("");
        if fingerprint(Path::new(path)) != database["seen"] {
            doubts.push(format!("the database {path} has changed since"));
        }
    }
    if event["kind"] == json!("check") {
        let current = event["spec"]
            .as_str()
            .and_then(|path| std::fs::read(path).ok());
        match (current, event["revision"]["hash"].as_str()) {
            (None, _) => doubts.push("the spec is no longer there".to_string()),
            (Some(bytes), Some(checked)) if hash(&bytes) != checked => {
                doubts.push("the spec has changed since".to_string())
            }
            _ => {}
        }
    }
    if event["kind"] == json!("proposal") {
        let spec = event["spec"].as_str().map(PathBuf::from);
        let status = spec
            .as_deref()
            .and_then(|spec| crate::spec::proposal::load(spec).ok().flatten())
            .filter(|current| Some(current.id.as_str()) == event["id"].as_str())
            .map(|current| current.status);
        use crate::spec::proposal::Status;
        match status {
            Some(Status::Applied) => {}
            Some(Status::Pending) => doubts.push("not yet decided on".to_string()),
            Some(Status::Rejected) => doubts.push("the person rejected it".to_string()),
            None => doubts.push("replaced by a later proposal".to_string()),
        }
    }
    if event["kind"] == json!("open") && event["status"] == json!("delivered") {
        doubts.push("delivered, with no word on how it went".to_string());
    }
    if event["kind"] == json!("unreadable") {
        doubts.push("this line of the file could not be read".to_string());
    }
    doubts
}

/// An event as `show` prints it: its long text cut, its doubts added.
fn summarized(event: &Value) -> Value {
    let mut shown = event.clone();
    if let Some(sql) = event["sql"].as_str() {
        shown["sql"] = json!(capped(sql, SHOWN_SQL_CHARS));
    }
    if let Some(revision) = shown.get_mut("revision").and_then(Value::as_object_mut) {
        revision.remove("text");
    }
    shown["doubts"] = json!(doubts(event));
    shown
}

/// What a note rests on, and every doubt about it.
fn conclusions(events: &[Value]) -> Vec<Value> {
    let latest = events.iter().rev().find(|e| e["kind"] == json!("note"));
    events
        .iter()
        .filter(|event| event["kind"] == json!("note"))
        .map(|note| {
            let refs = note["refs"].as_array().cloned().unwrap_or_default();
            let mut doubted = Vec::new();
            for seq in &refs {
                match events.iter().find(|event| &event["seq"] == seq) {
                    Some(event) => doubted.extend(
                        doubts(event)
                            .into_iter()
                            .map(|doubt| format!("#{seq}: {doubt}")),
                    ),
                    None => doubted.push(format!("#{seq}: no such step")),
                }
            }
            let since = events
                .iter()
                .filter(|event| event["seq"].as_u64() > note["seq"].as_u64())
                .filter(|event| event["kind"] != json!("note") && event["kind"] != json!("window"))
                .count();
            json!({
                "seq": note["seq"],
                "at": note["at"],
                "origin": note["origin"],
                "text": note["text"],
                "refs": refs,
                "latest": Some(note) == latest,
                "steps_since": since,
                "doubts": doubted,
            })
        })
        .collect()
}

fn show_json(path: &Path, header: &Value, events: &[Value], last: usize) -> Value {
    let skipped = events.len().saturating_sub(last);
    json!({
        "session": path.to_string_lossy(),
        "version": header["ducklocal_session"],
        "created_at": header["created_at"],
        "steps": events.len(),
        "latest_seq": events.last().map(|event| event["seq"].clone()),
        "omitted_steps": skipped,
        "conclusions": conclusions(events),
        "events": events[skipped..].iter().map(summarized).collect::<Vec<_>>(),
    })
}

fn show_markdown(path: &Path, events: &[Value], last: usize) -> String {
    let skipped = events.len().saturating_sub(last);
    let mut out = format!("# Session {}\n\n", path.display());
    if skipped > 0 {
        out.push_str(&format!("({skipped} earlier steps not shown)\n\n"));
    }
    for event in &events[skipped..] {
        let what = match event["kind"].as_str().unwrap_or("?") {
            "query" => format!(
                "query, {}: `{}`",
                event["status"].as_str().unwrap_or("?"),
                capped(event["sql"].as_str().unwrap_or("").trim(), 120).replace('\n', " ")
            ),
            "check" => format!(
                "checked {}: {}",
                event["spec"].as_str().unwrap_or("?"),
                event["status"].as_str().unwrap_or("?")
            ),
            "proposal" => format!(
                "proposed a change to {}",
                event["spec"].as_str().unwrap_or("?")
            ),
            "open" => format!(
                "handed the window a request: {}",
                event["status"].as_str().unwrap_or("?")
            ),
            "window" => "noted what the window showed".to_string(),
            "note" => format!("**Conclusion:** {}", event["text"].as_str().unwrap_or("")),
            other => other.to_string(),
        };
        out.push_str(&format!(
            "- #{} {} ({}): {what}\n",
            event["seq"],
            event["at"].as_str().unwrap_or(""),
            event["origin"].as_str().unwrap_or("?"),
        ));
        let mut doubted = doubts(event);
        if event["kind"] == json!("note") {
            doubted = conclusions(events)
                .into_iter()
                .find(|note| note["seq"] == event["seq"])
                .and_then(|note| note["doubts"].as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(|doubt| doubt.as_str().map(str::to_string))
                .collect();
        }
        for doubt in doubted {
            out.push_str(&format!("  - ⚠ {doubt}\n"));
        }
    }
    out.trim_end().to_string()
}

pub(crate) const HELP: &str = "\
Usage: ducklocal session show [FILE] [--last N] [--format json|md]
       ducklocal session get [FILE] --seq N
       ducklocal session note [FILE] --text TEXT [--refs 3,4]
       ducklocal session snapshot [FILE]
       ducklocal session clear FILE

A local record of how an analysis got where it did. Set DUCKLOCAL_SESSION=FILE
and query, check, propose and open each append what they did. Without it,
nothing is recorded. It holds SQL and spec text, references to sources with
their size and modification time, and each result's shape and completeness: no
rows, no credentials, nothing sent anywhere. SQL or a spec that may hold
credentials is left out, and so may a false positive such as a column named
secret. Errors are kept, cut short, and can quote values. The file is yours: it
stays until you clear it.

  show       The steps, each with `doubts`: what about it failed, was cut off or
             has changed since; plus `conclusions` and what each rests on. No
             doubts means nothing is known against it, not that it is right.
  get        One step whole, such as a spec revision's text
  note       Record a conclusion; --refs names the steps it rests on, by
             default the step before it
  snapshot   Record what the window shows: tabs, SQL, filter picks, no rows
  clear      Delete the file, once it has been shown to be a session file

FILE defaults to DUCKLOCAL_SESSION, except for clear. --last defaults to 50.
`origin` is what the environment says (DUCKLOCAL_AGENT), and nothing checks it.
What the person does in the window is not recorded except through snapshot, and
through how a proposal ended. Exit 2 for arguments, 1 with kind not_session or
io.
";

const SPEC: &[FlagSpec] = &[
    FlagSpec {
        name: "--last",
        takes_value: true,
    },
    FlagSpec {
        name: "--format",
        takes_value: true,
    },
    FlagSpec {
        name: "--seq",
        takes_value: true,
    },
    FlagSpec {
        name: "--text",
        takes_value: true,
    },
    FlagSpec {
        name: "--refs",
        takes_value: true,
    },
];

fn number(value: &OsString, flag: &str) -> Result<u64, CliError> {
    let text = value.to_string_lossy();
    text.parse()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| CliError::argument(format!("{flag} must be a whole number; got {text:?}")))
}

/// `ducklocal session`.
pub(crate) fn run(args: &[OsString]) -> Result<String, CliError> {
    let Some(action) = args.first().and_then(|arg| arg.to_str()) else {
        return Err(CliError::argument(
            "Say what to do: show, get, note, snapshot or clear",
        ));
    };
    let mut file: Option<PathBuf> = None;
    let mut last = DEFAULT_SHOWN_EVENTS;
    let mut markdown = false;
    let mut seq = None;
    let mut text = None;
    let mut refs = None;
    for arg in parse_args("session", &args[1..], SPEC, &["--text"])? {
        match arg {
            Arg::Flag("--last", Some(value)) => last = number(&value, "--last")? as usize,
            Arg::Flag("--format", Some(value)) => {
                markdown = match value.to_string_lossy().as_ref() {
                    "md" | "markdown" => true,
                    "json" => false,
                    other => {
                        return Err(CliError::argument(format!(
                            "--format must be json or md; got {other:?}"
                        )))
                    }
                };
            }
            Arg::Flag("--seq", Some(value)) => seq = Some(number(&value, "--seq")?),
            Arg::Flag("--text", Some(value)) => {
                text = Some(
                    value
                        .into_string()
                        .map_err(|_| CliError::argument("--text must be UTF-8"))?,
                );
            }
            Arg::Flag("--refs", Some(value)) => {
                let parsed: Result<Vec<u64>, _> = value
                    .to_string_lossy()
                    .split(',')
                    .map(|part| part.trim().parse())
                    .collect();
                refs = Some(parsed.map_err(|_| {
                    CliError::argument("--refs is a comma-separated list of step numbers")
                })?);
            }
            Arg::Positional(value) if file.is_none() => file = Some(value.into()),
            Arg::Positional(_) => return Err(CliError::argument("Name one session file")),
            _ => {
                return Err(CliError::failure(
                    "internal",
                    "session accepted a flag it does not handle",
                ))
            }
        }
    }
    if action == "clear" {
        // Named outright: an environment variable is no way to delete a file.
        let file = file.ok_or_else(|| CliError::argument("Name the session file to clear"))?;
        if !file.exists() {
            return Ok(json!({"cleared": false, "reason": "no such file"}).to_string());
        }
        if !is_session_file(&file) {
            return Err(not_a_session(&file));
        }
        std::fs::remove_file(&file)
            .map_err(|e| CliError::failure("io", format!("{}: {e}", file.display())))?;
        return Ok(json!({"cleared": true}).to_string());
    }
    let file = file
        .or_else(active)
        .ok_or_else(|| CliError::argument(format!("Name a session file, or set {ENV}")))?;
    match action {
        "show" => {
            let (header, events) = read_events(&file)?;
            Ok(if markdown {
                show_markdown(&file, &events, last)
            } else {
                show_json(&file, &header, &events, last).to_string()
            })
        }
        "get" => {
            let seq = seq.ok_or_else(|| CliError::argument("Name a step with --seq N"))?;
            let (_, events) = read_events(&file)?;
            events
                .iter()
                .find(|event| event["seq"] == json!(seq))
                .map(|event| {
                    let mut event = event.clone();
                    event["doubts"] = json!(doubts(&event));
                    event.to_string()
                })
                .ok_or_else(|| CliError::failure("not_found", format!("No step {seq}")))
        }
        "note" => {
            let text = text.ok_or_else(|| CliError::argument("Say what with --text TEXT"))?;
            if text.trim().is_empty() {
                return Err(CliError::argument("The note is empty"));
            }
            let events = if file.exists() {
                read_events(&file)?.1
            } else {
                Vec::new()
            };
            let refs = refs.unwrap_or_else(|| {
                events
                    .iter()
                    .rev()
                    .find(|event| event["kind"] != json!("note"))
                    .and_then(|event| event["seq"].as_u64())
                    .into_iter()
                    .collect()
            });
            for seq in &refs {
                if !events.iter().any(|event| event["seq"] == json!(seq)) {
                    return Err(CliError::argument(format!("No step {seq} to refer to")));
                }
            }
            let seq = append_to(
                &file,
                json!({"kind": "note", "text": text.trim(), "refs": refs}),
            )
            .map_err(|message| CliError::failure("io", message))?;
            Ok(json!({"seq": seq, "refs": refs}).to_string())
        }
        "snapshot" => {
            let mut state = crate::remote::window_state()?;
            without_rows(&mut state);
            let seq = append_to(&file, json!({"kind": "window", "state": state}))
                .map_err(|message| CliError::failure("io", message))?;
            Ok(json!({"seq": seq}).to_string())
        }
        other => Err(CliError::argument(format!(
            "Unknown session action: {other}; use show, get, note, snapshot or clear"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ducklocal_session_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn credentials_stay_out_and_a_false_alarm_says_so() {
        for sql in [
            "CREATE SECRET s (TYPE s3, KEY_ID 'AKIA', SECRET 'x')",
            "ATTACH 'postgres://me:hunter2@db/x' AS p",
            "SELECT * FROM read_csv('s3://b/x?X-Amz-Security-Token=abc')",
            "SET s3_access_key_id = 'AKIA'",
            "SELECT password FROM users",
        ] {
            let (text, sql_hash) = lineage_text(sql);
            assert_eq!(text, REDACTED, "{sql}");
            assert_eq!(sql_hash, None, "{sql}");
        }
        let (text, sql_hash) = lineage_text("SELECT sum(n) FROM 'a.csv'");
        assert_eq!(text, "SELECT sum(n) FROM 'a.csv'");
        assert!(sql_hash.is_some());
        // A plain URL with a port is no password.
        assert!(!may_hold_credentials(
            "SELECT * FROM 'https://host:8080/a.csv'"
        ));
    }

    #[test]
    fn the_parser_says_what_a_select_reads() {
        let refs = references_of(
            "WITH a AS (SELECT * FROM 'one.csv') SELECT * FROM a, read_parquet('two.parquet') t, \
             orders JOIN '*.json' j ON true",
        )
        .unwrap();
        for expected in ["one.csv", "two.parquet", "orders", "*.json"] {
            assert!(refs.iter().any(|r| r == expected), "{expected} in {refs:?}");
        }
        assert!(
            !refs.iter().any(|r| r == "a"),
            "a CTE is no source: {refs:?}"
        );
        // A list of files is not left half-listed.
        assert_eq!(
            references_of("SELECT * FROM read_csv(['a.csv', 'b.csv'])"),
            None
        );
        assert_eq!(references_of("COPY t TO 'x.csv'"), None);
        assert_eq!(references_of("not sql"), None);
    }

    #[test]
    fn a_source_is_told_apart_as_it_changes_and_goes() {
        let dir = scratch("source");
        std::fs::write(dir.join("a.csv"), "n\n1\n").unwrap();
        let entry = source_entry("a.csv", &dir);
        assert_eq!(entry["seen"]["size"], 4);
        assert_eq!(source_status(&entry), "same");
        std::fs::write(dir.join("a.csv"), "n\n1\n2\n").unwrap();
        assert_eq!(source_status(&entry), "changed");
        std::fs::remove_file(dir.join("a.csv")).unwrap();
        assert_eq!(source_status(&entry), "missing");

        // A glob follows the files it matches.
        std::fs::write(dir.join("p1.csv"), "n\n1\n").unwrap();
        let glob = source_entry("p*.csv", &dir);
        assert_eq!(glob["seen"]["files"], 1);
        assert_eq!(source_status(&glob), "same");
        std::fs::write(dir.join("p2.csv"), "n\n2\n").unwrap();
        assert_eq!(source_status(&glob), "changed");
    }

    #[test]
    fn a_remote_source_keeps_only_its_address() {
        let entry = source_entry("https://example.com/a.csv?sig=abc", Path::new(""));
        assert_eq!(entry["ref"], "https://example.com/a.csv");
        assert_eq!(entry["remote"], true);
        assert_eq!(source_status(&entry), "unknown");
        let entry = source_entry("https://me:pw@example.com/a.csv", Path::new(""));
        assert_eq!(entry["ref"], REDACTED);
    }

    #[test]
    fn nothing_is_written_over_a_file_that_is_not_a_session() {
        let dir = scratch("foreign");
        let path = dir.join("notes.txt");
        std::fs::write(&path, "my notes\n").unwrap();
        assert!(append_to(&path, json!({"kind": "note"})).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "my notes\n");
        assert!(read_events(&path).is_err());
    }

    #[test]
    fn steps_number_up_and_a_cut_off_line_is_reported() {
        let dir = scratch("append");
        let path = dir.join("s.jsonl");
        assert_eq!(append_to(&path, json!({"kind": "note"})).unwrap(), 1);
        assert_eq!(append_to(&path, json!({"kind": "note"})).unwrap(), 2);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"kind\": \"que\n");
        std::fs::write(&path, text).unwrap();
        let (_, events) = read_events(&path).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[2]["kind"], "unreadable");
        assert!(!doubts(&events[2]).is_empty());
    }

    #[test]
    fn doubts_say_what_a_result_was_not() {
        let event = json!({
            "kind": "query", "status": "ok", "truncated": true,
            "row_count": 10, "total_rows": 500, "total_rows_exact": false,
            "sources_known": true, "sources": [],
        });
        let said = doubts(&event).join("; ");
        assert!(said.contains("only 10 of at least 500"), "{said}");

        let failed = json!({"kind": "query", "status": "failed", "sources_known": false});
        assert_eq!(doubts(&failed), vec!["it failed".to_string()]);

        let withheld = json!({
            "kind": "query", "status": "ok", "sources_known": false, "sql": REDACTED,
        });
        assert!(doubts(&withheld)[0].contains("may hold credentials"));
        let unknown = json!({"kind": "query", "status": "ok", "sources_known": false});
        assert!(doubts(&unknown)[0].contains("could not be read"));

        let delivered = json!({"kind": "open", "status": "delivered"});
        assert!(doubts(&delivered)[0].contains("no word"));
        let clean = json!({
            "kind": "query", "status": "ok", "truncated": false,
            "sources_known": true, "sources": [],
        });
        assert!(doubts(&clean).is_empty());
    }

    #[test]
    fn rows_are_taken_out_of_a_window_state() {
        let mut state =
            json!({"tabs": [{"result": {"preview": [["secret row"]], "row_count": 1}}]});
        without_rows(&mut state);
        assert!(!state.to_string().contains("secret row"));
        assert_eq!(state["tabs"][0]["result"]["row_count"], 1);
    }
}
