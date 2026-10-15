//! Declarative dashboards: a `.dash` file of source, query and plot blocks.
//!
//! A `.dash` file declares a dashboard rather than scripting one — queries as
//! heredocs, plots as attributes, references between them (`query.latency`)
//! instead of return values.
//!
//! ```text
//! src/spec/syntax.rs   the text as a tree: blocks, attributes, heredocs
//! src/spec/model.rs    what the tree means, checked without a database
//! src/spec/source.rs   `source` blocks: named data files, as each query's CTEs
//! src/spec/filter.rs   `filter` blocks: a click's pick, as `$name` predicates
//! src/spec/mod.rs      `ducklocal check`, the command half
//! src/spec/prepare.rs  a plot's data, derived from its query's result once
//! src/spec/plot.rs     the plots the catalog charts cannot draw honestly
//! src/spec/highlight.rs  colours for the source editor, SQL heredocs included
//! src/spec/view.rs     the view half: a `.dash` file as a workspace tab
//! src/spec/tabs.rs     which specs are open, remembered between launches
//! src/spec/watch.rs    noticing edits made to an open spec outside the window
//! src/spec/lsp.rs      `ducklocal lsp`, the language server half
//! ```
//!
//! `ducklocal check FILE` validates a spec: parse, references, required
//! attributes, and each query's SQL through the real DuckDB parser. Then it
//! runs every query, read-only — on `--database PATH`, or in memory, which is
//! all a dashboard over its own `source` files needs — so an error that only
//! shows once rows are read fails the check, not the dashboard, and checks
//! each plot's columns against the ones its query returns. The view
//! half runs the same parse and validation before drawing anything, so a file
//! that fails `check` opens as its diagnostics, not as a broken chart.

pub mod comment_card;
pub mod comments;
pub mod complete;
pub mod filter;
pub mod highlight;
pub mod lsp;
pub mod model;
pub mod plot;
pub mod prepare;
pub mod presence;
pub mod source;
pub mod syntax;
pub mod tabs;
pub mod view;
pub mod watch;
mod wheel;

use std::ffi::OsString;
use std::path::PathBuf;

use serde_json::json;

use crate::cli::{parse_args, Arg, CliError, FlagSpec};

const SPEC: &[FlagSpec] = &[
    FlagSpec {
        name: "--database",
        takes_value: true,
    },
    FlagSpec {
        name: "--summary",
        takes_value: false,
    },
];

/// `ducklocal check FILE [--database PATH] [--summary]`.
///
/// One positional argument, like `profile`: the thing being checked is the
/// whole request; `--database` only says where column names come from.
pub fn check(args: &[OsString]) -> Result<String, CliError> {
    let mut file = None;
    let mut database = None;
    let mut summary = false;
    for arg in parse_args("check", args, SPEC, &[])? {
        match arg {
            Arg::Flag("--database", Some(value)) => {
                database = Some(crate::cli::database_path(&value)?);
            }
            Arg::Flag("--summary", None) => summary = true,
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
                    hint: None,
                });
            }
        }
    }
    let file = file.ok_or_else(|| CliError::argument("Name a .dash file to check"))?;
    let path_display = file.display().to_string();
    let source = std::fs::read_to_string(&file).map_err(|e| CliError::failure("io", e))?;

    let parsed = model::parse(&source)
        .map_err(|error| spec_error(&path_display, error.line, &error.message))?;
    // A flawed spec is still checked as far as it goes — its sound queries
    // run, their plots' columns compared — so one run reports every mistake,
    // not one stage of them per run.
    let (mut spec, mut diagnostics) = model::validate_partial(&parsed);

    // SQL is checked with the real parser on a throwaway connection: nothing
    // executes, so no table needs to exist and no side effect can happen. A
    // query that fails it is not run.
    let mut keyword_hint = None;
    spec.queries
        .retain(|query| match validate_query_sql(&query.sql) {
            Ok(()) => true,
            Err(message) => {
                if keyword_hint.is_none() {
                    keyword_hint = crate::cli::with_keyword_hint(
                        spec_error(&path_display, query.line, &message),
                        &query.sql,
                    )
                    .hint;
                }
                diagnostics.push(model::Diagnostic {
                    line: query.line,
                    col: query.col,
                    span: query.span,
                    message: format!("query {:?}: {message}", query.name),
                });
                false
            }
        });

    // Every query is described — its result columns, for the column check —
    // and then run, the way opening the dashboard runs it: on the database
    // when one is named, on an in-memory one otherwise, which is all a
    // dashboard over its own `source` files needs. DESCRIBE only plans: a
    // cast the build cannot perform or a value deep in the data that will
    // not convert surfaces only once rows are read, and a check that passes a
    // dashboard the GUI then cannot draw is no check. The SQL has been shown
    // read-only above and the connection is read-only (or holds nothing), so
    // running it changes nothing; it goes through the view's own `run_of`, so
    // the two stop at the same row budget and fail on the same statements.
    // Every failing query is reported, not only the first.
    //
    // In memory, a query reading a table no source defines may be meant for
    // a database the check was not given: it is listed as `unresolved`, not
    // failed, and its plots' columns go unchecked.
    let mut query_columns: Vec<Option<Vec<(String, String)>>> = vec![None; spec.queries.len()];
    let mut unresolved = Vec::new();
    let base = source::base_of(&file);
    let connection = crate::cli::open(database.clone(), false)?;
    // Paths written into the SQL start from the spec's folder too, as the
    // view runs them: a dashboard beside its data checks from anywhere.
    if let Some(search_path) = source::search_path(&base) {
        connection
            .execute_batch(&crate::query::set_file_search_path(&search_path))
            .map_err(|e| CliError::failure("database", e))?;
    }
    // The window, if one has this spec open, marks the dashboard while the
    // check runs and each plot while its query does.
    let _checking = presence::Checking::start(&file);
    let mut failures = Vec::new();
    for (index, query) in spec.queries.iter().enumerate() {
        let plots: Vec<String> = spec
            .plots
            .iter()
            .filter(|plot| plot.query == query.name)
            .map(|plot| plot.name.clone())
            .collect();
        if !plots.is_empty() {
            presence::announce(&file, presence::Activity::Checking, plots);
        }
        let located = |message: String| {
            format!(
                "{}:{}: query {:?}: {message}",
                path_display, query.line, query.name
            )
        };
        // What runs is the query with the sources it names in front of
        // it — the same SQL the dashboard runs, with nothing picked.
        let sql = source::expand(&filter::neutral(&query.sql), &spec.sources, &base);
        let outcome = describe(&connection, &sql).and_then(|columns| {
            match crate::query::run_of(&connection, &sql) {
                Ok(crate::query::QueryOutcome::Rows(_)) => Ok(columns),
                Ok(crate::query::QueryOutcome::Affected { .. }) => {
                    Err("returns no rows to plot".to_string())
                }
                Err(error) => Err(format!("{error:#}")),
            }
        });
        match outcome {
            Ok(columns) => query_columns[index] = Some(columns),
            Err(message) if database.is_none() && names_a_missing_table(&message) => {
                unresolved
                    .push(json!({"query": query.name, "line": query.line, "reason": message}));
            }
            Err(message) => failures.push(located(message)),
        }
    }
    // Nothing picked, every `$name` is TRUE and binds anywhere; a pick puts
    // the filter's column there, and a query that cannot see it fails on the
    // first click. Bound (not run) with a pick in place, so the check finds
    // that before anyone clicks. Only for a column the picking plot's query
    // returns — one it does not is the spec's mistake, said below.
    let returns = |query: &str, column: &str| {
        spec.queries
            .iter()
            .position(|q| q.name == query)
            .and_then(|index| query_columns[index].as_ref())
            .is_some_and(|columns| columns.iter().any(|(name, _)| name == column))
    };
    for (index, query) in spec.queries.iter().enumerate() {
        if query_columns[index].is_none() {
            continue;
        }
        let bind =
            |sql: &str| describe(&connection, &source::expand(sql, &spec.sources, &base)).map(drop);
        for message in unpickable(&spec, query, returns, bind) {
            failures.push(format!(
                "{}:{}: query {:?}: {message}",
                path_display, query.line, query.name
            ));
        }
    }
    if !failures.is_empty() && diagnostics.is_empty() {
        return Err(CliError::failure("sql", failures.join("\n")));
    }
    let ran = |name: &str| {
        spec.queries
            .iter()
            .position(|q| q.name == name)
            .is_some_and(|index| query_columns[index].is_some())
    };
    let mut checked = spec.clone();
    checked.plots.retain(|plot| ran(&plot.query));
    checked.filters.retain(|filter| {
        spec.plots
            .iter()
            .any(|plot| plot.name == filter.plot && ran(&plot.query))
    });
    let lookup = |name: &str| -> Result<Vec<(String, String)>, String> {
        let index = spec
            .queries
            .iter()
            .position(|q| q.name == name)
            .ok_or_else(|| format!("no query named {name:?}"))?;
        Ok(query_columns[index].clone().unwrap_or_default())
    };
    diagnostics.extend(model::check_columns(&checked, &lookup));
    if !diagnostics.is_empty() {
        diagnostics.sort_by_key(|d| (d.line, d.col));
        let mut error = spec_error_all(&path_display, diagnostics);
        // Queries that failed on the database alongside the spec's mistakes:
        // the same run, so the same report.
        for failure in &failures {
            error.message.push('\n');
            error.message.push_str(failure);
        }
        // The hint a failing query alone would have had, read off its message
        // as for any sql error.
        error.hint = keyword_hint
            .or_else(|| crate::cli::hint_for("sql", &failures.join("\n")).map(str::to_string));
        return Err(error);
    }

    // After an edit the question is only whether it passed: counts, and the
    // queries it could not run, whose plots went unchecked — passing says
    // nothing about those.
    if summary {
        return Ok(json!({
            "file": path_display,
            "ok": true,
            "sources": spec.sources.len(),
            "queries": spec.queries.len(),
            "filters": spec.filters.len(),
            "plots": spec.plots.len(),
            "unresolved": unresolved.iter().map(|u| &u["query"]).collect::<Vec<_>>(),
        })
        .to_string());
    }

    Ok(json!({
        "file": path_display,
        "sources": spec.sources.iter().map(|s| json!({
            "name": s.name,
            "path": s.path,
            "resolved": source::resolve(&s.path, &base).display().to_string(),
            "line": s.line,
        })).collect::<Vec<_>>(),
        "queries": spec.queries.iter().enumerate().map(|(index, q)| {
            let mut entry = json!({"name": q.name, "line": q.line});
            if let Some(columns) = &query_columns[index] {
                entry["columns"] = json!(columns.iter().map(|(name, ty)| {
                    json!({"name": name, "type": ty})
                }).collect::<Vec<_>>());
            }
            entry
        }).collect::<Vec<_>>(),
        "filters": spec.filters.iter().map(|f| json!({
            "name": f.name,
            "plot": f.plot,
            "column": f.column,
            "queries": spec.queries.iter()
                .filter(|q| spec.filters_of(q).iter().any(|used| used.name == f.name))
                .map(|q| q.name.clone())
                .collect::<Vec<_>>(),
            "line": f.line,
        })).collect::<Vec<_>>(),
        "plots": spec.plots.iter().map(|p| json!({
            "name": p.name,
            "type": p.kind,
            "query": p.query,
            "x": (!p.x.is_empty()).then_some(&p.x),
            "y": p.y,
            "series": p.series,
            "lat": p.lat,
            "lng": p.lng,
            "color": p.color,
            "size": p.size,
            "size_scale": p.size_scale,
            "tooltip": p.tooltip,
            "value": p.value,
            "note": p.note.as_ref().map(|note| match note {
                model::Note::Text(words) => json!({"text": words}),
                model::Note::Column(column) => json!({"column": column}),
            }),
            "tone": p.tone,
            "text": p.text,
            "title": p.title,
            "width": p.width(),
            "line": p.line,
        })).collect::<Vec<_>>(),
        "database": database.as_ref().map(|p| p.display().to_string()),
        "unresolved": unresolved,
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
    // Judged with nothing picked: a `$name` is a predicate, and what a pick
    // puts there is a comparison with a quoted literal (see `filter`).
    let sql = &filter::neutral(sql);
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
    let parser = crate::db::config()
        .and_then(duckdb::Connection::open_in_memory_with_flags)
        .map_err(|e| e.to_string())?;
    let refused: bool = parser
        .query_row(
            "SELECT coalesce(json_serialize_sql(?1::VARCHAR)::JSON->>'error' = 'true', true)",
            [sql],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if refused {
        return Err("a dashboard query must be a single read-only statement \
                    (SELECT, WITH, FROM, VALUES, SHOW, DESCRIBE, SUMMARIZE, \
                    or a PIVOT that names its values with IN)"
            .to_string());
    }
    Ok(())
}

/// Whether DuckDB refused a query for naming a table or view it does not
/// have — in memory, one that may live in a database the check was not given.
pub(crate) fn names_a_missing_table(message: &str) -> bool {
    message.contains("Catalog Error")
        && (message.contains("Table with name") || message.contains("View with name"))
        && message.contains("does not exist")
}

/// Each filter `query` names whose column it cannot see, as a message: the
/// query bound by `bind` with that one filter picked. A filter names a column
/// of the plot it is picked on, and the other queries must have one by the
/// same name — a `CASE … AS payment` in the picking query is not a column the
/// table it narrows has. The picking plot's own query sees TRUE (see
/// `filter`), so it is never asked; nor is any query about a column that
/// query does not return (`returns`: query, column), the spec's mistake.
fn unpickable(
    spec: &model::Spec,
    query: &model::Query,
    returns: impl Fn(&str, &str) -> bool,
    bind: impl Fn(&str) -> Result<(), String>,
) -> Vec<String> {
    let mut named: Vec<String> = filter::placeholders(&query.sql)
        .into_iter()
        .map(|p| p.name)
        .collect();
    named.sort();
    named.dedup();
    let mut problems = Vec::new();
    for name in named {
        let Some(picked) = spec.filters.iter().find(|f| f.name == name) else {
            continue;
        };
        let Some(picking) = spec.plots.iter().find(|plot| plot.name == picked.plot) else {
            continue;
        };
        if picking.query == query.name || !returns(&picking.query, &picked.column) {
            continue;
        }
        // NULL picked: a predicate that binds for a column of any type, so
        // only a missing column fails.
        let pick = filter::Pick {
            column: picked.column.clone(),
            value: None,
        };
        let sql = filter::apply(
            &query.sql,
            &std::collections::HashMap::from([(name.clone(), pick)]),
        );
        if let Err(error) = bind(&sql) {
            let reason = error.lines().next().unwrap_or_default().to_string();
            problems.push(format!(
                "${name} compares {:?}, picked on plot {:?}, and this query has \
                 no such column, so a click fails it: {reason}",
                picked.column, picked.plot
            ));
        }
    }
    problems
}

/// The columns a query returns, per DuckDB's own description of it.
fn describe(connection: &duckdb::Connection, sql: &str) -> Result<Vec<(String, String)>, String> {
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
        hint: None,
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
        hint: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{model, unpickable, validate_query_sql};

    #[test]
    fn a_filter_column_the_narrowed_query_lacks_is_caught_before_a_click() {
        let source = r#"
query "kinds" {
  sql = "SELECT CASE k WHEN 1 THEN 'a' ELSE 'b' END AS kind, count(*) AS n FROM t GROUP BY 1"
}
query "daily" {
  sql = "SELECT d, count(*) AS n FROM t WHERE $kind AND $kind GROUP BY 1"
}
filter "kind" {
  plot = plot.kinds
}
plot "kinds" {
  type = "bar"
  query = query.kinds
  x = kind
  y = n
}
plot "daily" {
  type = "line"
  query = query.daily
  x = d
  y = n
}
"#;
        let (spec, diagnostics) = model::validate_partial(&model::parse(source).unwrap());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let connection = duckdb::Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE t AS SELECT 1 AS k, DATE '2024-01-01' AS d")
            .unwrap();
        let bind = |sql: &str| {
            connection
                .prepare(&format!("DESCRIBE {sql}"))
                .map(drop)
                .map_err(|e| e.to_string())
        };
        let query = |name: &str| spec.queries.iter().find(|q| q.name == name).unwrap();
        // Named twice, said once; the picking plot's own query is not asked.
        let returns = |query: &str, column: &str| query == "kinds" && column == "kind";
        let problems = unpickable(&spec, query("daily"), returns, bind);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("\"kind\""), "{}", problems[0]);
        assert!(unpickable(&spec, query("kinds"), returns, bind).is_empty());
        // A column the picking query does not return is not asked about.
        assert!(unpickable(&spec, query("daily"), |_: &str, _: &str| false, bind).is_empty());
        // Once the table has the column, the pick binds.
        connection
            .execute_batch("ALTER TABLE t ADD COLUMN kind VARCHAR")
            .unwrap();
        assert!(unpickable(&spec, query("daily"), returns, bind).is_empty());
    }

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
            // A filter's placeholder is judged as TRUE.
            "SELECT * FROM t WHERE $channel AND $region",
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
