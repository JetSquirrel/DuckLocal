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

use std::path::{Path, PathBuf};

use super::model::Source;

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
            "path {path:?}: a source reads .csv, .tsv, .txt, .parquet, .json, .jsonl or .ndjson files"
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

/// `sql` with every source it names defined in front of it, as CTEs. A query
/// that names none comes back as written.
///
/// Which sources a query names is a word match, case-insensitive like SQL
/// names: a false match — the word inside a string literal — only adds a CTE
/// nothing reads, and DuckDB does not scan a CTE nothing reads.
pub(crate) fn expand(sql: &str, sources: &[Source], base: &Path) -> String {
    let words = words(sql);
    let used: Vec<&Source> = sources
        .iter()
        .filter(|source| words.iter().any(|w| w.eq_ignore_ascii_case(&source.name)))
        .collect();
    if used.is_empty() {
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
            rest = after.find('\n').map(|ix| &after[ix + 1..]).unwrap_or("").trim_start();
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.find("*/").map(|ix| &after[ix + 2..]).unwrap_or("").trim_start();
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
        assert_eq!(expand(sql, &sources, Path::new("/data")), sql);
        // Part of a longer word is not the name.
        let sql = "SELECT * FROM orders_2026";
        assert_eq!(expand(sql, &sources, Path::new("/data")), sql);
    }

    #[test]
    fn a_named_source_becomes_a_cte_read_from_the_specs_folder() {
        let sources = [source("orders", "exports/o*.csv"), source("unused", "u.parquet")];
        let sql = expand(
            "SELECT channel FROM ORDERS GROUP BY 1",
            &sources,
            Path::new("/data/dash"),
        );
        assert_eq!(
            sql,
            "WITH \"orders\" AS (SELECT * FROM read_csv_auto('/data/dash/exports/o*.csv'))\nSELECT channel FROM ORDERS GROUP BY 1"
        );
        assert!(!sql.contains("unused"), "{sql}");
    }

    #[test]
    fn a_query_with_its_own_ctes_keeps_them() {
        let sources = [source("orders", "/abs/o.parquet")];
        let base = Path::new("/ignored");
        assert_eq!(
            expand("-- note\nWITH t AS (FROM orders) SELECT * FROM t", &sources, base),
            "WITH \"orders\" AS (SELECT * FROM read_parquet('/abs/o.parquet')),\nt AS (FROM orders) SELECT * FROM t"
        );
        assert_eq!(
            expand("with recursive t AS (FROM orders) FROM t", &sources, base),
            "WITH RECURSIVE \"orders\" AS (SELECT * FROM read_parquet('/abs/o.parquet')),\nt AS (FROM orders) FROM t"
        );
    }

    #[test]
    fn statements_a_cte_cannot_lead_are_wrapped() {
        let sources = [source("orders", "/abs/o.json")];
        assert_eq!(
            expand("SUMMARIZE orders; -- all of it", &sources, Path::new("/")),
            "WITH \"orders\" AS (SELECT * FROM read_json_auto('/abs/o.json'))\nSELECT * FROM (\nSUMMARIZE orders; -- all of it\n)"
        );
    }

    #[test]
    fn quotes_in_a_path_stay_inside_the_literal() {
        let sources = [source("o", "it's.csv")];
        let sql = expand("FROM o", &sources, Path::new("/d"));
        assert!(sql.contains("('/d/it''s.csv')"), "{sql}");
    }

    #[test]
    fn relative_paths_follow_the_spec_and_absolute_ones_stay() {
        assert_eq!(resolve("a/b.csv", Path::new("/x/y")), PathBuf::from("/x/y/a/b.csv"));
        assert_eq!(resolve("/abs.csv", Path::new("/x/y")), PathBuf::from("/abs.csv"));
        assert_eq!(base_of(Path::new("/x/y/d.dash")), PathBuf::from("/x/y"));
    }

    #[test]
    fn only_readable_extensions_are_sources() {
        assert_eq!(unreadable("logs/*.parquet"), None);
        assert_eq!(unreadable("a.TSV"), None);
        assert!(unreadable("book.xlsx").unwrap().contains("workbook"));
        assert!(unreadable("data").unwrap().contains(".csv"));
    }
}
