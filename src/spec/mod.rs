//! Declarative dashboards: a `.dash` file of query and plot blocks.
//!
//! An analysis app (`src/analysis/`) is a script that draws itself; a `.dash`
//! file is the same idea declared rather than scripted — queries as heredocs,
//! plots as attributes, references between them (`query.latency`) instead of
//! return values. The two formats coexist: the DSL covers query + standard
//! plot, the script stays for bespoke layout and interaction.
//!
//! ```text
//! src/spec/syntax.rs   the text as a tree: blocks, attributes, heredocs
//! src/spec/model.rs    what the tree means, checked without a database
//! src/spec/mod.rs      `ducklocal check`, the command half
//! src/spec/prepare.rs  a plot's data, derived from its query's result once
//! src/spec/view.rs     the view half: a `.dash` file as a workspace tab
//! src/spec/tabs.rs     which specs are open, remembered between launches
//! src/spec/lsp.rs      `ducklocal lsp`, the language server half
//! ```
//!
//! `ducklocal check FILE` validates a spec without touching a database: parse,
//! references, required attributes, and each query's SQL through the real
//! DuckDB parser (parse-only, nothing executes). With `--database PATH` it
//! also describes every query on that database and checks each plot's
//! `x`/`y`/`series` against the columns the query actually returns. The view
//! half runs the same parse and validation before drawing anything, so a file
//! that fails `check` opens as its diagnostics, not as a broken chart.

pub mod complete;
pub mod lsp;
pub mod model;
pub mod prepare;
pub mod syntax;
pub mod tabs;
pub mod view;

use std::ffi::OsString;
use std::path::PathBuf;

use serde_json::json;

use crate::cli::{parse_args, Arg, CliError, FlagSpec};

const SPEC: &[FlagSpec] = &[FlagSpec {
    name: "--database",
    takes_value: true,
}];

/// `ducklocal check FILE [--database PATH]`.
///
/// One positional argument, like `profile`: the thing being checked is the
/// whole request; `--database` only says where column names come from.
pub fn check(args: &[OsString]) -> Result<String, CliError> {
    let mut file = None;
    let mut database = None;
    for arg in parse_args("check", args, SPEC, &[])? {
        match arg {
            Arg::Flag("--database", Some(value)) => {
                database = Some(crate::cli::database_path(&value)?);
            }
            Arg::Positional(value) => {
                if file.is_some() {
                    return Err(CliError::argument(
                        "Name one spec: the check validates one file",
                    ));
                }
                file = Some(PathBuf::from(value));
            }
            _ => {
                return Err(CliError {
                    kind: "internal",
                    message: "the argument walker produced a flag check does not declare"
                        .to_string(),
                    code: 1,
                });
            }
        }
    }
    let file = file.ok_or_else(|| CliError::argument("Name a .dash file to check"))?;
    let path_display = file.display().to_string();
    let source = std::fs::read_to_string(&file).map_err(|e| CliError::failure("io", e))?;

    let parsed = model::parse(&source)
        .map_err(|error| spec_error(&path_display, error.line, &error.message))?;
    let spec = model::validate(&parsed).map_err(|diagnostics| {
        spec_error_all(&path_display, diagnostics)
    })?;

    // SQL is checked with the real parser on a throwaway connection: nothing
    // executes, so no table needs to exist and no side effect can happen.
    for query in &spec.queries {
        validate_query_sql(&query.sql).map_err(|message| {
            spec_error(
                &path_display,
                query.line,
                &format!("query {:?}: {message}", query.name),
            )
        })?;
    }

    // Column checking needs the queries described, not run: DESCRIBE plans the
    // statement and reports its result columns without reading the data.
    let mut query_columns: Vec<Option<Vec<(String, String)>>> = vec![None; spec.queries.len()];
    if let Some(database) = &database {
        let connection = crate::cli::open(Some(database.clone()), false)?;
        for (index, query) in spec.queries.iter().enumerate() {
            query_columns[index] = Some(
                describe(&connection, &query.sql)
                    .map_err(|message| CliError::failure("sql", format!(
                        "{}:{}: query {:?}: {message}",
                        path_display, query.line, query.name
                    )))?,
            );
        }
        let lookup = |name: &str| -> Result<Vec<(String, String)>, String> {
            let index = spec
                .queries
                .iter()
                .position(|q| q.name == name)
                .ok_or_else(|| format!("no query named {name:?}"))?;
            Ok(query_columns[index].clone().unwrap_or_default())
        };
        let diagnostics = model::check_columns(&spec, &lookup);
        if !diagnostics.is_empty() {
            return Err(spec_error_all(&path_display, diagnostics));
        }
    }

    Ok(json!({
        "file": path_display,
        "queries": spec.queries.iter().enumerate().map(|(index, q)| {
            let mut entry = json!({"name": q.name, "line": q.line});
            if let Some(columns) = &query_columns[index] {
                entry["columns"] = json!(columns.iter().map(|(name, ty)| {
                    json!({"name": name, "type": ty})
                }).collect::<Vec<_>>());
            }
            entry
        }).collect::<Vec<_>>(),
        "plots": spec.plots.iter().map(|p| json!({
            "name": p.name,
            "type": p.kind,
            "query": p.query,
            "x": p.x,
            "y": p.y,
            "series": p.series,
            "title": p.title,
            "line": p.line,
        })).collect::<Vec<_>>(),
        "database": database.as_ref().map(|p| p.display().to_string()),
    })
    .to_string())
}

/// A dashboard query's SQL, checked without running it: exactly one statement,
/// and one that only reads. A `.dash` file is something people send each
/// other, and the view runs its queries on open, on every change to the file
/// and on every launch that restores the tab — a `DROP`, `COPY … TO` or
/// `ATTACH` in it would run on the user's live connection before any plot
/// could say "not rows".
///
/// "Only reads" is DuckDB's own judgement, not a keyword list:
/// `json_serialize_sql` serializes SELECT statements (CTEs, `FROM`-first,
/// `VALUES`, `TABLE`, `SHOW`, `DESCRIBE`, `SUMMARIZE` included) and refuses
/// everything else. Its one false refusal is `PIVOT`/`UNPIVOT`, which cannot
/// carry a write, so a statement led by either is let through.
pub(crate) fn validate_query_sql(sql: &str) -> Result<(), String> {
    crate::cli::validate_sql(sql).map_err(|error| error.message().to_string())?;
    let leading = sql
        .trim_start()
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(leading.as_str(), "pivot" | "unpivot") {
        return Ok(());
    }
    let parser = duckdb::Connection::open_in_memory().map_err(|e| e.to_string())?;
    let refused: bool = parser
        .query_row(
            "SELECT coalesce(json_serialize_sql(?1::VARCHAR)::JSON->>'error' = 'true', true)",
            [sql],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if refused {
        return Err("a dashboard query must be a single read-only statement \
                    (SELECT, WITH, FROM, VALUES, SHOW, DESCRIBE, SUMMARIZE, PIVOT)"
            .to_string());
    }
    Ok(())
}

/// The columns a query returns, per DuckDB's own description of it.
fn describe(
    connection: &duckdb::Connection,
    sql: &str,
) -> Result<Vec<(String, String)>, String> {
    let mut statement = connection
        .prepare(&format!("DESCRIBE {sql}"))
        .map_err(|e| e.to_string())?;
    let mut rows = statement.query([]).map_err(|e| e.to_string())?;
    let mut columns = Vec::new();
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        columns.push((
            row.get::<_, String>(0).map_err(|e| e.to_string())?,
            row.get::<_, String>(1).map_err(|e| e.to_string())?,
        ));
    }
    Ok(columns)
}

fn spec_error(file: &str, line: usize, message: &str) -> CliError {
    CliError {
        kind: "spec",
        message: format!("{file}:{line}: {message}"),
        code: 2,
    }
}

fn spec_error_all(file: &str, diagnostics: Vec<model::Diagnostic>) -> CliError {
    CliError {
        kind: "spec",
        message: diagnostics
            .iter()
            .map(|d| format!("{file}:{}: {}", d.line, d.message))
            .collect::<Vec<_>>()
            .join("\n"),
        code: 2,
    }
}

#[cfg(test)]
mod tests {
    use super::validate_query_sql;

    #[test]
    fn a_dashboard_query_may_only_read() {
        for sql in [
            "SELECT 1",
            "WITH x AS (SELECT 1 AS n) SELECT n FROM x",
            "FROM range(3)",
            "VALUES (1), (2)",
            "SHOW TABLES",
            "DESCRIBE SELECT 1",
            "SUMMARIZE SELECT 1",
            "PIVOT (SELECT 1 AS a, 2 AS b) ON a IN (1) USING sum(b)",
            "SELECT * FROM read_csv('orders.csv') -- a trailing note",
        ] {
            assert!(validate_query_sql(sql).is_ok(), "{sql}");
        }
        for sql in [
            "DROP TABLE orders",
            "COPY (SELECT 1) TO '/tmp/out.csv'",
            "ATTACH 'other.duckdb'",
            "INSERT INTO t VALUES (1) RETURNING *",
            "CREATE TABLE t AS SELECT 1",
            "INSTALL httpfs",
            "SELECT 1; DROP TABLE orders",
        ] {
            assert!(validate_query_sql(sql).is_err(), "{sql}");
        }
    }
}
