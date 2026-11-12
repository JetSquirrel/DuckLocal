//! `source` blocks: data files under a name, for the queries to read.
//!
//! A query that reads `FROM 'exports/amount-2026-09-01.csv'` is tied to the
//! working directory it runs from and to that one file's name. A source says
//! the where once, beside the spec:
//!
//! ```text
//! source "usage" { path = "exports/amount-*.csv" }
//! query "by_type" { sql = "SELECT type, sum(amount) FROM usage GROUP BY 1" }
//! ```
//!
//! The path is relative to the `.dash` file's own folder, so the spec and its
//! data move together; a glob reads every match as one relation.
//!
//! A source never touches the connection. The dashboard runs on the window's
//! session, and a view created there would show up in the user's catalog,
//! outlive the tab, and collide with a second dashboard's `orders`. Instead
//! each query that names a source gets it as a CTE in front of its own SQL —
//! so a source shadows a table of the same name for that query only, and
//! nothing outside the query can see it. The rewrite adds only a
//! `SELECT * FROM reader('path')`, which reads; the read-only check runs on
//! the query as written.
//!
//! A `view` block is the same idea one step on: a SELECT under a name, for
//! the shaping several queries would otherwise each repeat.
//!
//! ```text
//! source "trail" { path = "cloudtrail.json.gz" }
//! view "events" { sql = "SELECT r.eventName AS name FROM (SELECT unnest(Records) AS r FROM trail)" }
//! query "names" { sql = "SELECT name, count(*) FROM events GROUP BY 1" }
//! ```
//!
//! It too is a CTE in front of each query that names it, after the sources
//! and in the order the file defines them: a view reads the sources and the
//! views above it. Its SQL is someone's SQL, so it passes the same read-only
//! check a query does before anything that names it runs (`unsound_view`).

use std::path::{Path, PathBuf};

use super::model::{Source, View};

/// Why a path cannot be a source, judged by its extension: the readers a
/// source can use are DuckDB's own table functions. `None` when it can.
pub(crate) fn unreadable(path: &str) -> Option<String> {
    if crate::db::data_file_reader(path).is_some() {
        return None;
    }
    Some(if crate::db::is_excel_file(path) {
        "A workbook cannot be a source; open it in DuckLocal and query the table it becomes"
            .to_string()
    } else {
        format!(
            "path {path:?}: a source reads .csv, .tsv, .txt, .parquet, .json, .jsonl or .ndjson files, and gzipped text"
        )
    })
}

/// A source's path as a real one: `~` expanded, relative paths joined to the
/// spec's folder, and the result absolute — the GUI's working directory is
/// wherever it was launched from, which is no anchor at all.
pub(crate) fn resolve(path: &str, base: &Path) -> PathBuf {
    let expanded = PathBuf::from(crate::db::expand_tilde(path));
    let joined = if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    };
    std::path::absolute(&joined).unwrap_or(joined)
}

/// The folder a spec's relative source paths start from.
pub(crate) fn base_of(spec_path: &Path) -> PathBuf {
    spec_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// The `file_search_path` a spec's queries run with: its folder, absolute,
/// so a path written straight into the SQL (`FROM 'data/m.csv'`) finds the
/// file beside the spec from any working directory, as a `source` path does.
/// DuckDB still tries the working directory first for a single file (a glob
/// prefers the search path), so a same-named file there wins — rare from a
/// terminal, impossible from a GUI launched at `/`. `None` for a folder whose
/// name has a comma, which the setting would read as two folders.
pub(crate) fn search_path(base: &Path) -> Option<String> {
    let base = if base.as_os_str().is_empty() {
        Path::new(".")
    } else {
        base
    };
    let path = std::path::absolute(base).ok()?;
    let path = path.to_string_lossy().into_owned();
    (!path.contains(',')).then_some(path)
}

/// `sql` with every source and view it names defined in front of it, as
/// CTEs. A query that names none comes back as written.
///
/// Which of them a query names is a word match, case-insensitive like SQL
/// names: a false match — the word inside a string literal — only adds a CTE
/// nothing reads, and DuckDB does not scan a CTE nothing reads. What a named
/// view names counts too, so a view brings its own sources along.
pub(crate) fn expand(sql: &str, sources: &[Source], views: &[View], base: &Path) -> String {
    let mut named = words(sql);
    let names = |named: &[&str], name: &str| named.iter().any(|w| w.eq_ignore_ascii_case(name));
    // Last first: a view reads only what is defined above it, so by the time
    // one is passed over, everything that could name it has been seen.
    let mut used_views: Vec<&View> = Vec::new();
    for view in views.iter().rev() {
        if names(&named, &view.name) {
            named.extend(words(&view.sql));
            used_views.push(view);
        }
    }
    used_views.reverse();
    let used: Vec<&Source> = sources
        .iter()
        .filter(|source| names(&named, &source.name))
        .collect();
    if used.is_empty() && used_views.is_empty() {
        return sql.to_string();
    }
    let ctes = used
        .iter()
        .map(|source| {
            let path = resolve(&source.path, base).to_string_lossy().into_owned();
            // Validation has already said the extension has a reader.
            let reader = crate::db::data_file_reader(&path).unwrap_or("read_csv_auto");
            format!(
                "\"{}\" AS (SELECT * FROM {reader}('{}'))",
                source.name.replace('"', "\"\""),
                path.replace('\'', "''")
            )
        })
        .chain(used_views.iter().map(|view| {
            let inner = view.sql.trim().trim_end_matches(';');
            let (keyword, _) = leading_word(skip_comments(inner));
            // The newline before the parenthesis keeps a trailing `--`
            // comment from swallowing it.
            match keyword.to_ascii_lowercase().as_str() {
                "with" | "select" | "from" => format!("\"{}\" AS (\n{inner}\n)", view.name),
                // As below: what a CTE cannot hold, a subquery can.
                _ => format!("\"{}\" AS (SELECT * FROM (\n{inner}\n))", view.name),
            }
        }))
        .collect::<Vec<_>>()
        .join(",\n");

    let body = skip_comments(sql);
    let (keyword, rest) = leading_word(body);
    match keyword.to_ascii_lowercase().as_str() {
        // Join the query's own CTE list; RECURSIVE applies to the whole list,
        // so it stays in front.
        "with" => {
            let (next, after) = leading_word(rest.trim_start());
            if next.eq_ignore_ascii_case("recursive") {
                format!("WITH RECURSIVE {ctes},\n{}", after.trim_start())
            } else {
                format!("WITH {ctes},\n{}", rest.trim_start())
            }
        }
        "select" | "from" => format!("WITH {ctes}\n{body}"),
        // SHOW, DESCRIBE, SUMMARIZE, VALUES, PIVOT, TABLE: statements a CTE
        // cannot lead, but a subquery can hold. The newline before the
        // parenthesis keeps a trailing `--` comment from swallowing it.
        _ => {
            let inner = body.trim_end().trim_end_matches(';');
            format!("WITH {ctes}\nSELECT * FROM (\n{inner}\n)")
        }
    }
}

/// The first view whose SQL is not a single read-only statement, and why.
/// A view's SQL runs inside every query that names it, so it is held to what
/// a query is, and judged on its own: SQL that only parses once it is inside
/// the parentheses of a CTE, or that closes them early, does not pass.
pub(crate) fn unsound_view(views: &[View]) -> Option<(&View, String)> {
    views
        .iter()
        .find_map(|view| Some((view, super::validate_query_sql(&view.sql).err()?)))
}

/// The identifier-shaped words of `sql`, for matching source names.
fn words(sql: &str) -> Vec<&str> {
    sql.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .collect()
}

/// `sql` past its leading whitespace and comments, both `--` and `/* */`.
fn skip_comments(sql: &str) -> &str {
    let mut rest = sql.trim_start();
    loop {
        if let Some(after) = rest.strip_prefix("--") {
            rest = after
                .find('\n')
                .map(|ix| &after[ix + 1..])
                .unwrap_or("")
                .trim_start();
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after
                .find("*/")
                .map(|ix| &after[ix + 2..])
                .unwrap_or("")
                .trim_start();
        } else {
            return rest;
        }
    }
}

/// The leading ASCII word of `text` and what follows it.
fn leading_word(text: &str) -> (&str, &str) {
    let end = text
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(text.len());
    text.split_at(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fully absolute fixtures on both platforms, independent of the runner's drive.
    fn root() -> &'static Path {
        #[cfg(windows)]
        {
            Path::new(r"C:\")
        }
        #[cfg(not(windows))]
        {
            Path::new("/")
        }
    }

    fn source(name: &str, path: &str) -> Source {
        Source {
            name: name.into(),
            path: path.into(),
            line: 1,
            name_span: (7, 10),
        }
    }

    #[test]
    fn a_query_that_names_no_source_runs_as_written() {
        let sources = [source("orders", "orders.csv")];
        let sql = "SELECT * FROM customers";
        assert_eq!(expand(sql, &sources, &[], &root().join("data")), sql);
        // Part of a longer word is not the name.
        let sql = "SELECT * FROM orders_2026";
        assert_eq!(expand(sql, &sources, &[], &root().join("data")), sql);
    }

    #[test]
    fn a_named_source_becomes_a_cte_read_from_the_specs_folder() {
        let base = root().join("data").join("dash");
        let path = base.join("exports").join("o*.csv");
        let sources = [
            source("orders", "exports/o*.csv"),
            source("unused", "u.parquet"),
        ];
        let sql = expand(
            "SELECT channel FROM ORDERS GROUP BY 1",
            &sources,
            &[],
            &base,
        );
        assert_eq!(
            sql,
            format!("WITH \"orders\" AS (SELECT * FROM read_csv_auto('{}'))\nSELECT channel FROM ORDERS GROUP BY 1", path.display())
        );
        assert!(!sql.contains("unused"), "{sql}");
    }

    fn view(name: &str, sql: &str) -> View {
        View {
            name: name.into(),
            sql: sql.into(),
            line: 1,
            col: 1,
            span: (0, 4),
            name_span: (5, 8),
        }
    }

    #[test]
    fn a_named_view_follows_the_sources_it_reads() {
        let base = root().join("d");
        let path = base.join("t.json");
        let sources = [source("trail", "t.json"), source("unused", "u.csv")];
        let views = [
            view("events", "SELECT unnest(Records) AS r FROM trail;\n"),
            view("denied", "FROM events WHERE r.errorCode IS NOT NULL"),
            view("spare", "SUMMARIZE unused"),
        ];
        // Naming the second view brings the first, and the first its source.
        assert_eq!(
            expand("SELECT count(*) FROM Denied", &sources, &views, &base),
            format!(
                "WITH \"trail\" AS (SELECT * FROM read_json_auto('{}')),\n\
                 \"events\" AS (\nSELECT unnest(Records) AS r FROM trail\n),\n\
                 \"denied\" AS (\nFROM events WHERE r.errorCode IS NOT NULL\n)\n\
                 SELECT count(*) FROM Denied",
                path.display()
            )
        );
        // A query that names no view gets none.
        let plain = expand("FROM trail", &sources, &views, &base);
        assert!(!plain.contains("events"), "{plain}");
        // A statement a CTE cannot hold sits in a subquery.
        let wrapped = expand("FROM spare", &sources, &views, &base);
        assert!(
            wrapped.contains("\"spare\" AS (SELECT * FROM (\nSUMMARIZE unused\n))"),
            "{wrapped}"
        );
    }

    #[test]
    fn a_view_is_held_to_what_a_query_is() {
        assert!(unsound_view(&[view("v", "SELECT 1")]).is_none());
        for sql in [
            "DROP TABLE t",
            "SELECT 1; DROP TABLE t",
            "SELECT 1) SELECT * FROM (SELECT 2",
        ] {
            let views = [view("ok", "FROM t"), view("bad", sql)];
            let (found, _) = unsound_view(&views).expect(sql);
            assert_eq!(found.name, "bad");
        }
    }

    #[test]
    fn a_query_with_its_own_ctes_keeps_them() {
        let path = root().join("abs").join("o.parquet");
        let sources = [source("orders", &path.to_string_lossy())];
        let base = root().join("ignored");
        assert_eq!(
            expand("-- note\nWITH t AS (FROM orders) SELECT * FROM t", &sources, &[], &base),
            format!("WITH \"orders\" AS (SELECT * FROM read_parquet('{}')),\nt AS (FROM orders) SELECT * FROM t", path.display())
        );
        assert_eq!(
            expand("with recursive t AS (FROM orders) FROM t", &sources, &[], &base),
            format!("WITH RECURSIVE \"orders\" AS (SELECT * FROM read_parquet('{}')),\nt AS (FROM orders) FROM t", path.display())
        );
    }

    #[test]
    fn statements_a_cte_cannot_lead_are_wrapped() {
        let path = root().join("abs").join("o.json");
        let sources = [source("orders", &path.to_string_lossy())];
        assert_eq!(
            expand("SUMMARIZE orders; -- all of it", &sources, &[], root()),
            format!("WITH \"orders\" AS (SELECT * FROM read_json_auto('{}'))\nSELECT * FROM (\nSUMMARIZE orders; -- all of it\n)", path.display())
        );
    }

    #[test]
    fn quotes_in_a_path_stay_inside_the_literal() {
        let sources = [source("o", "it's.csv")];
        let base = root().join("d");
        let sql = expand("FROM o", &sources, &[], &base);
        let escaped = base.join("it''s.csv");
        assert_eq!(
            sql,
            format!(
                "WITH \"o\" AS (SELECT * FROM read_csv_auto('{}'))\nFROM o",
                escaped.display()
            )
        );
    }

    #[test]
    fn relative_paths_follow_the_spec_and_absolute_ones_stay() {
        let base = root().join("x").join("y");
        let absolute = root().join("abs.csv");
        assert_eq!(resolve("a/b.csv", &base), base.join("a").join("b.csv"));
        assert_eq!(resolve(&absolute.to_string_lossy(), &base), absolute);
        assert_eq!(base_of(&base.join("d.dash")), base);
        // A spec named without a folder is in the working directory.
        assert_eq!(
            search_path(&base_of(Path::new("d.dash"))),
            Some(std::env::current_dir().unwrap().display().to_string())
        );
        assert_eq!(search_path(&base.join("a,b")), None);
    }

    #[test]
    fn only_readable_extensions_are_sources() {
        assert_eq!(unreadable("logs/*.parquet"), None);
        assert_eq!(unreadable("a.TSV"), None);
        assert!(unreadable("book.xlsx").unwrap().contains("workbook"));
        assert!(unreadable("data").unwrap().contains(".csv"));
    }
}
