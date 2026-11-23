//! `ducklocal notebook`: a notebook's cells, and running them, headless.
//!
//! `cells` reads the file and nothing more. `run` runs the SQL cells in
//! order on one connection — so a table one cell creates is there for the
//! next, as in the window — with relative paths starting from the
//! notebook's folder, as the window runs them too.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{CellKind, Document};
use crate::cli::{database_path, parse_args, row_limit, Arg, CliError, FlagSpec, Format};
use crate::script::PieceKind;

pub(crate) const HELP: &str = "\
Usage: ducklocal notebook cells FILE
       ducklocal notebook run FILE [--cell N]... [--keep-going]
                                   [--database PATH] [--read-write] [--limit N]
                                   [--format json|md]
                                   [--restricted [--allow-read PATH]...
                                    [--timeout SECONDS] [--memory-limit SIZE]]

A notebook is a .sql file of cells: a line `-- %%` starts a SQL cell and
`-- %% [markdown]` a Markdown one, whose lines are SQL comments. A file with
no markers is one SQL cell. Cells are numbered from 1, Markdown included.

cells  Print the cells as JSON: {file, cells: [{index, kind, line, source}]},
       line being where the cell's text starts. Nothing runs.

run    Run the SQL cells in order on one connection, so what one cell creates
       the next can read; each cell may hold several statements. Relative
       file paths start from the notebook's folder. Stops at the first cell
       that fails, unless --keep-going.

Options for run:
  --cell N           Run only cell N; repeatable. The cells before it do not
                     run, so it must not need what they create
  --keep-going       Run every cell, whatever failed before it
  --database PATH    Existing database file, opened read-only
  --read-write       Allow writes (requires --database)
  --limit N          Maximum rows per cell's result, default 1000
  --format FORMAT    json (default), or md: the notebook as a Markdown report,
                     its text, each cell's SQL and its result as a table
  --restricted, --allow-read, --timeout, --memory-limit
                     As for `ducklocal query --restricted`: every statement
                     must be one SELECT, and --timeout covers the whole run

Output (json): {file, ok, cells: [...]}. A Markdown cell is {index, kind,
line, source}; a SQL cell adds status (ok, failed or skipped), statements (how
many it held), and result, the last statement's {columns, rows, row_count,
truncated, total_rows, total_rows_exact, elapsed_ms}, or error {kind, message,
hint}. With --restricted the output adds {restricted}, the limits it ran under.

The report is printed whether or not a cell failed; exit 1 when one did, 2 for
argument mistakes.
";

const RUN_SPEC: &[FlagSpec] = &[
    FlagSpec {
        name: "--cell",
        takes_value: true,
    },
    FlagSpec {
        name: "--keep-going",
        takes_value: false,
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
    FlagSpec {
        name: "--restricted",
        takes_value: false,
    },
    FlagSpec {
        name: "--allow-read",
        takes_value: true,
    },
    FlagSpec {
        name: "--timeout",
        takes_value: true,
    },
    FlagSpec {
        name: "--memory-limit",
        takes_value: true,
    },
];

/// `ducklocal notebook ARGS`: the output, and the exit code to leave with —
/// 1 when a cell failed, though the report is still the output.
pub(crate) fn run(args: &[OsString]) -> Result<(String, i32), CliError> {
    let (sub, rest) = args
        .split_first()
        .ok_or_else(|| CliError::argument("Name a notebook command: cells or run"))?;
    match sub.to_str() {
        Some("cells") => cells(rest).map(|output| (output, 0)),
        Some("run") => run_cells(rest),
        Some(other) => Err(
            CliError::argument(format!("Unknown notebook command: {other}"))
                .with_hint("ducklocal notebook cells FILE, or ducklocal notebook run FILE"),
        ),
        None => Err(CliError::argument("Notebook commands are UTF-8")),
    }
}

/// The one positional argument both commands take, read.
fn read_notebook(file: Option<PathBuf>) -> Result<(PathBuf, Document, Vec<usize>), CliError> {
    let file = file.ok_or_else(|| CliError::argument("Name a .sql notebook"))?;
    let text = std::fs::read_to_string(&file).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CliError::failure("io", format!("No notebook at {}", file.display()))
        } else {
            CliError::failure("io", format!("{}: {e}", file.display()))
        }
    })?;
    let (doc, lines) = super::parse_located(&text);
    Ok((file, doc, lines))
}

fn kind_name(kind: CellKind) -> &'static str {
    match kind {
        CellKind::Sql => "sql",
        CellKind::Markdown => "markdown",
    }
}

fn cells(args: &[OsString]) -> Result<String, CliError> {
    let mut file = None;
    for arg in parse_args("notebook cells", args, &[], &[])? {
        match arg {
            Arg::Positional(value) if file.is_none() => file = Some(PathBuf::from(value)),
            Arg::Positional(value) => {
                return Err(CliError::argument(format!(
                    "Unexpected argument: {}",
                    value.to_string_lossy()
                )))
            }
            Arg::Flag(..) => {
                return Err(CliError::failure(
                    "internal",
                    "the argument walker produced a flag cells does not declare",
                ))
            }
        }
    }
    let (file, doc, lines) = read_notebook(file)?;
    let cells: Vec<Value> = doc
        .cells
        .iter()
        .zip(&lines)
        .enumerate()
        .map(|(ix, (cell, line))| {
            json!({
                "index": ix + 1,
                "kind": kind_name(cell.kind),
                "line": line,
                "source": cell.source,
            })
        })
        .collect();
    Ok(json!({"file": file.display().to_string(), "cells": cells}).to_string())
}

struct RunOptions {
    file: Option<PathBuf>,
    only: Vec<usize>,
    keep_going: bool,
    database: Option<PathBuf>,
    read_write: bool,
    limit: usize,
    format: Format,
    limits: crate::restricted::Flags,
}

fn parse_run(args: &[OsString]) -> Result<RunOptions, CliError> {
    let mut options = RunOptions {
        file: None,
        only: Vec::new(),
        keep_going: false,
        database: None,
        read_write: false,
        limit: 1000,
        format: Format::Json,
        limits: Default::default(),
    };
    for arg in parse_args("notebook run", args, RUN_SPEC, &[])? {
        let Some(arg) = options.limits.take(arg)? else {
            continue;
        };
        match arg {
            Arg::Flag("--cell", Some(value)) => {
                let index = value
                    .to_str()
                    .and_then(|text| text.parse::<usize>().ok())
                    .filter(|index| *index > 0)
                    .ok_or_else(|| {
                        CliError::argument(format!(
                            "--cell takes a cell number from 1; got {:?}",
                            value.to_string_lossy()
                        ))
                        .with_hint("List the cells with ducklocal notebook cells FILE")
                    })?;
                if !options.only.contains(&index) {
                    options.only.push(index);
                }
            }
            Arg::Flag("--keep-going", None) => options.keep_going = true,
            Arg::Flag("--database", Some(value)) => {
                options.database = Some(database_path(&value)?);
            }
            Arg::Flag("--read-write", None) => options.read_write = true,
            Arg::Flag("--limit", Some(value)) => options.limit = row_limit(&value)?,
            Arg::Flag("--format", Some(value)) => {
                options.format = match Format::parse(&value)? {
                    Format::Html => {
                        return Err(CliError::argument("notebook run --format is json or md"))
                    }
                    format => format,
                }
            }
            Arg::Positional(value) if options.file.is_none() => {
                options.file = Some(PathBuf::from(value));
            }
            Arg::Positional(value) => {
                return Err(CliError::argument(format!(
                    "Unexpected argument: {}",
                    value.to_string_lossy()
                )))
            }
            _ => {
                return Err(CliError::failure(
                    "internal",
                    "the argument walker produced a flag run does not declare",
                ))
            }
        }
    }
    if options.read_write && options.database.is_none() {
        return Err(CliError::argument("--read-write requires --database"));
    }
    if options.limits.restricted && options.read_write {
        return Err(
            CliError::argument("--restricted only reads; it cannot take --read-write")
                .with_hint("Writing needs the person's approval to run without --restricted"),
        );
    }
    options.limits.validate()?;
    Ok(options)
}

/// What became of one SQL cell.
enum Ran {
    Ok {
        statements: usize,
        result: Option<crate::query::CliResult>,
    },
    Failed {
        statements: usize,
        error: CliError,
    },
    Skipped,
}

fn run_cells(args: &[OsString]) -> Result<(String, i32), CliError> {
    let options = parse_run(args)?;
    let (file, doc, lines) = read_notebook(options.file.clone())?;
    if let Some(&past) = options.only.iter().find(|&&index| index > doc.cells.len()) {
        return Err(CliError::argument(format!(
            "{} has {} cells; there is no cell {past}",
            file.display(),
            doc.cells.len()
        ))
        .with_hint("List the cells with ducklocal notebook cells FILE"));
    }
    let selected = |index: usize| options.only.is_empty() || options.only.contains(&index);

    let policy = options.limits.policy()?;
    let conn = crate::cli::open(options.database.clone(), options.read_write)?;
    // Paths in the SQL start from the notebook's folder, as in the window.
    let base = file.parent().unwrap_or(Path::new(""));
    if let Some(search_path) = crate::spec::source::search_path(base) {
        conn.execute_batch(&crate::query::set_file_search_path(&search_path))
            .map_err(|e| CliError::failure("database", e))?;
    }
    // After the search path: the policy locks every setting.
    let watch = match &policy {
        Some(policy) => {
            policy.enforce(&conn)?;
            Some(policy.watch(&conn))
        }
        None => None,
    };

    let mut ran: Vec<Option<Ran>> = Vec::with_capacity(doc.cells.len());
    let mut stopped = false;
    for (ix, cell) in doc.cells.iter().enumerate() {
        let index = ix + 1;
        if cell.kind != CellKind::Sql || !selected(index) {
            ran.push(None);
            continue;
        }
        if stopped {
            ran.push(Some(Ran::Skipped));
            continue;
        }
        let outcome = run_cell(
            &conn,
            &cell.source,
            options.limit,
            options.database.as_deref(),
            policy.as_ref().zip(watch.as_ref()),
        );
        if let Ran::Failed { error, .. } = &outcome {
            // Past a time or memory limit the rest would only fail too.
            let limit_hit = matches!(error.kind, "timeout" | "resource_limit");
            stopped = limit_hit || !options.keep_going;
        }
        ran.push(Some(outcome));
    }
    let failed = ran
        .iter()
        .any(|ran| matches!(ran, Some(Ran::Failed { .. })));

    let output = match options.format {
        Format::Markdown => markdown_report(&doc, &ran, &selected),
        _ => {
            let mut report = json_report(
                &file,
                &doc,
                &lines,
                &ran,
                &selected,
                options.only.is_empty(),
            );
            if let Some(policy) = &policy {
                report["restricted"] = policy.report();
            }
            report.to_string()
        }
    };
    Ok((output, if failed { 1 } else { 0 }))
}

/// Run one cell's statements in order, stopping at the first that fails.
/// The cell's result is its last statement's.
fn run_cell(
    conn: &duckdb::Connection,
    source: &str,
    limit: usize,
    database: Option<&Path>,
    policy: Option<(&crate::restricted::Policy, &crate::restricted::Watch)>,
) -> Ran {
    let pieces = crate::script::split(source);
    let statements = pieces.len();
    let mut result = None;
    for piece in pieces {
        let text = &source[piece.range.clone()];
        let sql = match piece.kind {
            PieceKind::Sql => text.to_string(),
            PieceKind::Dot => match crate::script::dot_command_sql(text) {
                Ok(sql) => sql,
                Err(error) => {
                    return Ran::Failed {
                        statements,
                        error: CliError::failure("sql", error),
                    }
                }
            },
        };
        let outcome = (|| {
            if let Some((policy, _)) = policy {
                policy.admit(conn, &sql)?;
            }
            crate::query::run_cli_of(conn, &sql, limit).map_err(|e| {
                let message = e.to_string();
                policy
                    .and_then(|(policy, watch)| policy.classify(&message, watch))
                    .unwrap_or_else(|| {
                        crate::cli::with_keyword_hint(CliError::failure("sql", message), &sql)
                    })
            })
        })();
        crate::session::record_query(&sql, database, policy.is_some(), outcome.as_ref());
        match outcome {
            Ok(cell_result) => result = Some(cell_result),
            Err(error) => return Ran::Failed { statements, error },
        }
    }
    Ran::Ok { statements, result }
}

fn error_json(error: &CliError) -> Value {
    let hint = error
        .hint
        .clone()
        .or_else(|| crate::cli::sniffed_type_hint(&error.message))
        .or_else(|| crate::cli::hint_for(error.kind, &error.message).map(str::to_string));
    let mut body = json!({"kind": error.kind, "message": error.message});
    if let Some(hint) = hint {
        body["hint"] = json!(hint);
    }
    body
}

fn json_report(
    file: &Path,
    doc: &Document,
    lines: &[usize],
    ran: &[Option<Ran>],
    selected: &dyn Fn(usize) -> bool,
    every_cell: bool,
) -> Value {
    let mut cells = Vec::new();
    for (ix, ((cell, line), ran)) in doc.cells.iter().zip(lines).zip(ran).enumerate() {
        let index = ix + 1;
        // With --cell, the report is the cells asked for; without, the
        // Markdown is there too, as the context the SQL was written in.
        let shown = match cell.kind {
            CellKind::Markdown => every_cell,
            CellKind::Sql => selected(index),
        };
        if !shown {
            continue;
        }
        let mut entry = json!({
            "index": index,
            "kind": kind_name(cell.kind),
            "line": line,
            "source": cell.source,
        });
        match ran {
            Some(Ran::Ok { statements, result }) => {
                entry["status"] = json!("ok");
                entry["statements"] = json!(statements);
                if let Some(result) = result {
                    entry["result"] = serde_json::to_value(result).unwrap_or(Value::Null);
                }
            }
            Some(Ran::Failed { statements, error }) => {
                entry["status"] = json!("failed");
                entry["statements"] = json!(statements);
                entry["error"] = error_json(error);
            }
            Some(Ran::Skipped) => entry["status"] = json!("skipped"),
            None => {}
        }
        cells.push(entry);
    }
    let ok = !ran
        .iter()
        .any(|ran| matches!(ran, Some(Ran::Failed { .. })));
    json!({"file": file.display().to_string(), "ok": ok, "cells": cells})
}

/// The notebook as a document to read: its Markdown as written, each SQL
/// cell as a code block with its result as a table under it.
fn markdown_report(
    doc: &Document,
    ran: &[Option<Ran>],
    selected: &dyn Fn(usize) -> bool,
) -> String {
    let mut out = Vec::new();
    for (ix, (cell, ran)) in doc.cells.iter().zip(ran).enumerate() {
        match cell.kind {
            CellKind::Markdown => {
                if !cell.source.trim().is_empty() {
                    out.push(cell.source.trim_end().to_string());
                }
            }
            CellKind::Sql => {
                if !selected(ix + 1) || cell.source.trim().is_empty() {
                    continue;
                }
                out.push(format!("```sql\n{}\n```", cell.source.trim_end()));
                match ran {
                    Some(Ran::Ok {
                        result: Some(result),
                        ..
                    }) if !result.columns.is_empty() => {
                        out.push(crate::query::format_markdown(result).trim_end().to_string());
                    }
                    Some(Ran::Ok { .. }) => {}
                    Some(Ran::Failed { error, .. }) => {
                        out.push(format!(
                            "> **Error:** {}",
                            error.message.replace('\n', "\n> ")
                        ));
                    }
                    Some(Ran::Skipped) => out.push("_Not run: an earlier cell failed._".into()),
                    None => {}
                }
            }
        }
    }
    out.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notebook(text: &str) -> (tempdir::Dir, PathBuf) {
        let dir = tempdir::Dir::new();
        let path = dir.0.join("nb.sql");
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    /// A folder of its own per test, removed when the test ends.
    mod tempdir {
        use std::sync::atomic::{AtomicUsize, Ordering};
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                static NEXT: AtomicUsize = AtomicUsize::new(0);
                let path = std::env::temp_dir().join(format!(
                    "ducklocal_nb_cli_{}_{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::SeqCst)
                ));
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).ok();
            }
        }
    }

    fn args(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    fn run_json(path: &Path, extra: &[&str]) -> (Value, i32) {
        let mut all = vec!["run", path.to_str().unwrap()];
        all.extend_from_slice(extra);
        let (output, code) = run(&args(&all)).unwrap();
        (serde_json::from_str(&output).unwrap(), code)
    }

    const TEXT: &str = "\
-- %% [markdown]
-- # Totals

-- %%
CREATE TEMP TABLE t AS SELECT * FROM 'rows.csv';

-- %%
SELECT sum(n) AS total FROM t;
";

    #[test]
    fn cells_lists_each_cell_with_its_line() {
        let (_dir, path) = notebook(TEXT);
        let output = cells(&args(&[path.to_str().unwrap()])).unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        let cells = value["cells"].as_array().unwrap();
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0]["kind"], "markdown");
        assert_eq!(cells[0]["line"], 2);
        assert_eq!(cells[1]["line"], 5);
        assert_eq!(cells[2]["source"], "SELECT sum(n) AS total FROM t;");
    }

    #[test]
    fn run_shares_one_connection_and_finds_files_beside_the_notebook() {
        let (dir, path) = notebook(TEXT);
        std::fs::write(dir.0.join("rows.csv"), "n\n1\n2\n3\n").unwrap();
        let (report, code) = run_json(&path, &[]);
        assert_eq!(code, 0, "{report}");
        assert_eq!(report["ok"], true);
        let cells = report["cells"].as_array().unwrap();
        assert_eq!(cells[1]["status"], "ok");
        assert_eq!(cells[2]["result"]["rows"][0][0], 6);
    }

    #[test]
    fn a_failing_cell_stops_the_run_unless_told_to_keep_going() {
        let (_dir, path) = notebook("-- %%\nSELECT * FROM missing;\n-- %%\nSELECT 1 AS one;\n");
        let (report, code) = run_json(&path, &[]);
        assert_eq!(code, 1);
        assert_eq!(report["ok"], false);
        assert_eq!(report["cells"][0]["status"], "failed");
        assert_eq!(report["cells"][0]["error"]["kind"], "sql");
        assert_eq!(report["cells"][1]["status"], "skipped");

        let (report, code) = run_json(&path, &["--keep-going"]);
        assert_eq!(code, 1);
        assert_eq!(report["cells"][1]["status"], "ok");
    }

    #[test]
    fn only_the_cells_asked_for_run() {
        let (_dir, path) = notebook("-- %%\nSELECT * FROM missing;\n-- %%\nSELECT 2 AS two;\n");
        let (report, code) = run_json(&path, &["--cell", "2"]);
        assert_eq!(code, 0, "{report}");
        let cells = report["cells"].as_array().unwrap();
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0]["index"], 2);
        let error = run(&args(&["run", path.to_str().unwrap(), "--cell", "9"])).unwrap_err();
        assert_eq!(error.code, 2);
    }

    #[test]
    fn restricted_refuses_a_cell_that_writes() {
        let (_dir, path) =
            notebook("-- %%\nCREATE TABLE t AS SELECT 1;\n-- %%\nSELECT 1 AS one;\n");
        let (report, code) = run_json(&path, &["--restricted"]);
        assert_eq!(code, 1);
        assert_eq!(report["cells"][0]["error"]["kind"], "policy_denied");
        assert!(report["restricted"].is_object());
    }

    #[test]
    fn markdown_format_reads_as_a_report() {
        let (dir, path) = notebook(TEXT);
        std::fs::write(dir.0.join("rows.csv"), "n\n1\n2\n").unwrap();
        let (output, code) =
            run(&args(&["run", path.to_str().unwrap(), "--format", "md"])).unwrap();
        assert_eq!(code, 0);
        assert!(
            output.starts_with("# Totals\n\n```sql\nCREATE TEMP TABLE"),
            "{output}"
        );
        assert!(output.contains("| total |"), "{output}");
    }
}
