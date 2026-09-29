//! What the editor's Run executes: a script of one or more statements, in
//! order, on the shared connection — the way the DuckDB shell reads its input.
//!
//! A script is split into pieces here rather than by DuckDB's parser for two
//! reasons. Dot commands (`.tables`, `.schema`) are shell syntax the parser
//! rejects, and each piece keeps its byte range in the editor text, so an
//! error in the third statement underlines the third statement.
//!
//! Execution stops at the first failure, as the shell's `-bail` does: running
//! `SELECT` after a failed `ATTACH` would only report a second, misleading
//! error. Pieces after it are reported as not run.
//!
//! The CLI's `query` command stays single-statement on purpose: its JSON
//! output is one result, and it validates before executing so a script cannot
//! slip a `COPY … TO` past it.

use std::ops::Range;

use anyhow::{anyhow, Result};
use duckdb::Connection;

use crate::i18n::{tr, trf};
use crate::query::QueryOutcome;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PieceKind {
    Sql,
    /// A shell command, `.name args` on one line.
    Dot,
}

/// One statement or dot command of a script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    pub kind: PieceKind,
    /// Byte range in the script, trimmed, without the terminating `;`.
    pub range: Range<usize>,
}

/// Split `script` into statements and dot commands.
///
/// Statements end at a `;` outside string literals, quoted identifiers,
/// dollar-quoted strings and comments; the last one needs none. A dot command
/// is a line starting with `.` where a statement would start, and ends at the
/// end of its line. A piece holding only comments is dropped.
pub fn split(script: &str) -> Vec<Piece> {
    let bytes = script.as_bytes();
    let mut pieces = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // Where the next piece's first token is, past blanks and comments.
        let Some(start) = skip_trivia(script, i) else {
            break;
        };
        if bytes[start] == b'.' {
            let end = script[start..]
                .find('\n')
                .map_or(script.len(), |ix| start + ix);
            let text = script[start..end].trim_end();
            // A trailing `;` is a habit from SQL, not part of the command.
            let text = text.strip_suffix(';').unwrap_or(text).trim_end();
            pieces.push(Piece {
                kind: PieceKind::Dot,
                range: start..start + text.len(),
            });
            i = end;
            continue;
        }
        let end = statement_end(script, start);
        let text = script[start..end].trim_end();
        pieces.push(Piece {
            kind: PieceKind::Sql,
            range: start..start + text.len(),
        });
        // Past the `;`, if there was one.
        i = end + 1;
    }
    pieces
}

/// The first byte at or after `from` that is not whitespace or a comment.
fn skip_trivia(script: &str, mut from: usize) -> Option<usize> {
    let bytes = script.as_bytes();
    loop {
        while from < bytes.len() && (bytes[from].is_ascii_whitespace() || bytes[from] == b';') {
            from += 1;
        }
        if script[from..].starts_with("--") {
            from = script[from..].find('\n').map_or(bytes.len(), |ix| from + ix);
        } else if script[from..].starts_with("/*") {
            from = script[from + 2..]
                .find("*/")
                .map_or(bytes.len(), |ix| from + 2 + ix + 2);
        } else {
            return (from < bytes.len()).then_some(from);
        }
    }
}

/// The byte index of the `;` ending the statement that starts at `start`, or
/// the script's length when it runs to the end.
fn statement_end(script: &str, start: usize) -> usize {
    let bytes = script.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b';' => return i,
            quote @ (b'\'' | b'"') => {
                // A doubled quote inside is an escaped one: closing and
                // reopening lands in the same place.
                i = script[i + 1..]
                    .find(quote as char)
                    .map_or(bytes.len(), |ix| i + 1 + ix + 1);
                continue;
            }
            b'-' if script[i..].starts_with("--") => {
                i = script[i..].find('\n').map_or(bytes.len(), |ix| i + ix);
                continue;
            }
            b'/' if script[i..].starts_with("/*") => {
                i = script[i + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |ix| i + 2 + ix + 2);
                continue;
            }
            b'$' => {
                if let Some(tag) = dollar_tag(&script[i..]) {
                    let body = i + tag.len();
                    i = script[body..]
                        .find(tag)
                        .map_or(bytes.len(), |ix| body + ix + tag.len());
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

/// `$tag$` or `$$` at the start of `text`, the opening of a dollar-quoted
/// string. `$1` — a parameter — is not one.
fn dollar_tag(text: &str) -> Option<&str> {
    let rest = &text[1..];
    let len = rest
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    if rest.as_bytes().get(len) != Some(&b'$') {
        return None;
    }
    // A tag cannot start with a digit, or `$1$` would open a string.
    if rest.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return None;
    }
    Some(&text[..len + 2])
}

/// The dot commands this shell understands, for `.help` and for the error an
/// unknown one gets.
const DOT_COMMANDS: &[(&str, &str)] = &[
    (".tables ?PATTERN?", "script.dot.help.tables"),
    (".schema ?PATTERN?", "script.dot.help.schema"),
    (".databases", "script.dot.help.databases"),
    (".help", "script.dot.help.help"),
];

/// The SQL a dot command stands for. `PATTERN` is an `ILIKE` pattern, as in
/// the DuckDB shell: `%` and `_` are wildcards, and a pattern without them
/// matches a name exactly (ignoring case).
pub fn dot_command_sql(command: &str) -> Result<String> {
    let mut words = command.split_whitespace();
    let name = words.next().unwrap_or(".");
    let pattern = words.next();
    if words.next().is_some() {
        return Err(anyhow!(trf("script.dot.too_many_args", &[name])));
    }
    let filter = |column: &str| match pattern {
        Some(p) => format!("WHERE {column} ILIKE '{}'", p.replace('\'', "''")),
        None => String::new(),
    };
    let no_pattern = |sql: String| {
        if pattern.is_some() {
            Err(anyhow!(trf("script.dot.no_args", &[name])))
        } else {
            Ok(sql)
        }
    };
    match name {
        ".tables" => Ok(format!(
            "SELECT database, schema, name, column_names AS columns
             FROM (SHOW ALL TABLES)
             {}
             ORDER BY database, schema, name",
            filter("name")
        )),
        ".schema" => Ok(format!(
            "SELECT database, schema, name, sql FROM (
                 SELECT database_name AS database, schema_name AS schema,
                        table_name AS name, sql
                 FROM duckdb_tables()
                 UNION ALL
                 SELECT database_name, schema_name, view_name, sql
                 FROM duckdb_views() WHERE NOT internal
             )
             {}
             ORDER BY database, schema, name",
            filter("name")
        )),
        ".databases" => no_pattern(
            "SELECT database_name AS name, path, type, readonly
             FROM duckdb_databases()
             WHERE NOT internal
             ORDER BY database_name"
                .to_string(),
        ),
        ".help" => no_pattern(help_sql()),
        _ => Err(anyhow!(trf(
            "script.dot.unknown",
            &[name, &supported_commands()]
        ))),
    }
}

fn supported_commands() -> String {
    DOT_COMMANDS
        .iter()
        .map(|(usage, _)| usage.split_whitespace().next().unwrap_or(usage))
        .collect::<Vec<_>>()
        .join(" ")
}

fn help_sql() -> String {
    let rows: Vec<String> = DOT_COMMANDS
        .iter()
        .map(|(usage, key)| format!("('{usage}', '{}')", tr(key).replace('\'', "''")))
        .collect();
    format!(
        "SELECT * FROM (VALUES {}) AS t(command, description)",
        rows.join(", ")
    )
}

/// One piece of a run script, and what came of it: `None` when an earlier
/// piece failed and this one never ran.
pub struct Step {
    pub piece: Piece,
    pub outcome: Option<Result<QueryOutcome>>,
}

/// The database and schema unqualified names resolve against, as `USE` left
/// them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchPath {
    pub database: String,
    pub schema: String,
}

pub struct ScriptOutcome {
    pub steps: Vec<Step>,
    /// Read after the script ran, so a `USE` in it is reflected; `None` when
    /// the connection could not say.
    pub search_path: Option<SearchPath>,
}

impl ScriptOutcome {
    /// The step a single-result caller reports: the first failure, else the
    /// last step that returned rows, else the last step.
    pub fn primary(&self) -> Option<usize> {
        primary_step(&self.steps)
    }
}

/// See [`ScriptOutcome::primary`].
pub fn primary_step(steps: &[Step]) -> Option<usize> {
    steps
        .iter()
        .position(|s| matches!(s.outcome, Some(Err(_))))
        .or_else(|| {
            steps
                .iter()
                .rposition(|s| matches!(s.outcome, Some(Ok(QueryOutcome::Rows(_)))))
        })
        .or_else(|| steps.len().checked_sub(1))
}

/// Run `script` on the shared connection.
pub fn run(script: &str) -> Result<ScriptOutcome> {
    crate::db::with_connection(|conn| Ok(run_of(conn, script)))
}

pub fn run_of(conn: &Connection, script: &str) -> ScriptOutcome {
    let mut failed = false;
    let steps = split(script)
        .into_iter()
        .map(|piece| {
            if failed {
                return Step {
                    piece,
                    outcome: None,
                };
            }
            let text = &script[piece.range.clone()];
            let outcome = match piece.kind {
                PieceKind::Sql => crate::query::run_of(conn, text),
                PieceKind::Dot => {
                    dot_command_sql(text).and_then(|sql| crate::query::run_of(conn, &sql))
                }
            };
            failed = outcome.is_err();
            Step {
                piece,
                outcome: Some(outcome),
            }
        })
        .collect();
    ScriptOutcome {
        steps,
        search_path: search_path_of(conn),
    }
}

pub fn search_path_of(conn: &Connection) -> Option<SearchPath> {
    conn.query_row("SELECT current_database(), current_schema()", [], |row| {
        Ok(SearchPath {
            database: row.get(0)?,
            schema: row.get(1)?,
        })
    })
    .ok()
}

/// A one-line label for a piece: its first line, whitespace collapsed,
/// shortened to `max_chars`.
pub fn summary(text: &str, max_chars: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        collapsed
    } else {
        let mut short: String = collapsed.chars().take(max_chars.saturating_sub(1)).collect();
        short.push('…');
        short
    }
}

#[cfg(test)]
mod tests {
    use super::{dot_command_sql, run_of, split, summary, PieceKind};
    use crate::query::QueryOutcome;

    fn texts(script: &str) -> Vec<(PieceKind, &str)> {
        split(script)
            .into_iter()
            .map(|p| (p.kind, &script[p.range]))
            .collect()
    }

    #[test]
    fn statements_split_at_semicolons() {
        assert_eq!(
            texts("ATTACH 'x.db' AS x;\nUSE x;\nSELECT 1"),
            [
                (PieceKind::Sql, "ATTACH 'x.db' AS x"),
                (PieceKind::Sql, "USE x"),
                (PieceKind::Sql, "SELECT 1"),
            ]
        );
    }

    #[test]
    fn semicolons_inside_quotes_and_comments_do_not_split() {
        let script = "SELECT 'a;b', \"c;d\" -- e;f\nFROM t; /* g;h */ SELECT $$i;j$$, $x$k;l$x$";
        assert_eq!(
            texts(script),
            [
                (PieceKind::Sql, "SELECT 'a;b', \"c;d\" -- e;f\nFROM t"),
                (PieceKind::Sql, "SELECT $$i;j$$, $x$k;l$x$"),
            ]
        );
        // An escaped quote stays inside its string.
        assert_eq!(texts("SELECT 'it''s; fine'").len(), 1);
    }

    #[test]
    fn comment_only_pieces_are_dropped() {
        assert_eq!(texts("-- nothing here;\n;;  \n/* nor; here */"), []);
        assert_eq!(texts("SELECT 1;\n-- trailing note"), [(PieceKind::Sql, "SELECT 1")]);
    }

    #[test]
    fn dot_commands_are_lines_of_their_own() {
        assert_eq!(
            texts(".tables\n.schema stat%;\nSELECT 1;"),
            [
                (PieceKind::Dot, ".tables"),
                (PieceKind::Dot, ".schema stat%"),
                (PieceKind::Sql, "SELECT 1"),
            ]
        );
    }

    #[test]
    fn a_parameter_is_not_a_dollar_quote() {
        assert_eq!(texts("SELECT $1; SELECT 2").len(), 2);
    }

    #[test]
    fn a_script_runs_in_order_and_use_sticks() {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        let outcome = run_of(
            &conn,
            "ATTACH ':memory:' AS other; USE other; CREATE TABLE t AS SELECT 42 AS n; SELECT * FROM t",
        );
        assert_eq!(outcome.steps.len(), 4);
        assert!(outcome.steps.iter().all(|s| matches!(s.outcome, Some(Ok(_)))));
        let path = outcome.search_path.clone().unwrap();
        assert_eq!((path.database.as_str(), path.schema.as_str()), ("other", "main"));
        // The rows of the last statement are the ones reported.
        let primary = outcome.primary().unwrap();
        assert_eq!(primary, 3);
        let Some(Ok(QueryOutcome::Rows(result))) = &outcome.steps[primary].outcome else {
            panic!("expected rows");
        };
        assert_eq!(result.rows, [["42"]]);
    }

    #[test]
    fn a_failure_stops_the_script() {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        let outcome = run_of(&conn, "SELECT 1; SELECT * FROM missing; SELECT 3");
        assert!(matches!(outcome.steps[0].outcome, Some(Ok(_))));
        assert!(matches!(outcome.steps[1].outcome, Some(Err(_))));
        assert!(outcome.steps[2].outcome.is_none());
        assert_eq!(outcome.primary(), Some(1));
    }

    #[test]
    fn dot_commands_run() {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE stations(code VARCHAR); CREATE VIEW v AS SELECT 1")
            .unwrap();
        let outcome = run_of(&conn, ".tables\n.tables stat%\n.schema stations\n.databases\n.help");
        let rows: Vec<usize> = outcome
            .steps
            .iter()
            .map(|s| match &s.outcome {
                Some(Ok(QueryOutcome::Rows(r))) => r.rows.len(),
                other => panic!("{:?}", other.as_ref().map(|o| o.as_ref().err().map(|e| e.to_string()))),
            })
            .collect();
        assert_eq!(rows[0], 2, ".tables lists the table and the view");
        assert_eq!(rows[1], 1, "the pattern keeps stations only");
        assert_eq!(rows[2], 1);
        assert!(rows[3] >= 1);
        assert_eq!(rows[4], super::DOT_COMMANDS.len());
    }

    #[test]
    fn unknown_dot_commands_say_what_is_supported() {
        let error = dot_command_sql(".mode box").unwrap_err().to_string();
        assert!(error.contains(".mode"), "{error}");
        assert!(error.contains(".tables"), "{error}");
        // A quote in a pattern cannot break out of the literal.
        assert!(dot_command_sql(".tables x'y").unwrap().contains("'x''y'"));
    }

    #[test]
    fn summaries_are_one_short_line() {
        assert_eq!(summary("SELECT *\n  FROM t", 40), "SELECT * FROM t");
        assert_eq!(summary("SELECT something_long", 8), "SELECT …");
    }
}
