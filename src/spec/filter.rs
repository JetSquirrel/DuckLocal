//! `filter` blocks: a click on one plot narrows the queries of the others.
//!
//! ```text
//! filter "channel" {
//!   plot   = plot.revenue_by_channel   // a click on a bar (or a table row) picks
//!   column = channel                   // the value of this column; default: x
//! }
//! query "daily" {
//!   sql = "SELECT date, sum(orders) FROM orders WHERE $channel GROUP BY 1"
//! }
//! ```
//!
//! `$channel` in a query's SQL is a predicate: `("channel" = 'Web')` while a
//! value is picked, `TRUE` while none is — so a dashboard with nothing picked
//! runs exactly the SQL it would without the filter. Several filters combine
//! as the author writes them (`WHERE $channel AND $region`), and a query that
//! names none is never re-run when a pick changes.
//!
//! The picking plot's own queries see `TRUE`: the filter narrows everything
//! else, never the plot it is picked on — otherwise one click would collapse
//! the bars to the one just clicked, and there would be nothing left to click
//! next. This is the cross-filter of Mosaic and crossfilter.js.
//!
//! The picked value goes into the SQL as a string literal, quotes doubled, and
//! DuckDB casts it to the column's type where it is compared; the column is a
//! quoted identifier. Nothing the data holds can end the literal early, so
//! the rewrite keeps a read-only query read-only — the read-only check runs
//! on the SQL with every placeholder `TRUE`.
//!
//! Finding `$name` skips string literals, quoted identifiers, comments and
//! dollar-quoted strings, so a `'$5'` in the data or a `$$ … $$` body is never
//! mistaken for one.
//!
//! A filter with `multiple = true` collects values click by click; its
//! predicate is `("channel" IN ('Web', 'App'))`, with `OR … IS NULL` when NULL
//! is one of them.

use std::collections::HashMap;
use std::ops::Range;

/// One `$name` in a query's SQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Placeholder {
    pub name: String,
    /// Byte range of the whole `$name`, dollar included.
    pub range: Range<usize>,
}

/// What is picked on a plot, for the queries that name its filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pick {
    /// The column the predicate compares, as named on the filter.
    pub column: String,
    /// The cells as the result grid shows them, in the order picked, none
    /// repeated; `None` is SQL NULL. One unless the filter takes `multiple`;
    /// a pick with none is no pick and is not kept.
    pub values: Vec<Option<String>>,
}

impl Pick {
    /// The predicate that stands in for the placeholder.
    pub(crate) fn predicate(&self) -> String {
        let column = format!("\"{}\"", self.column.replace('"', "\"\""));
        let null = self.values.iter().any(Option::is_none);
        let literals: Vec<String> = self
            .values
            .iter()
            .flatten()
            .map(|value| format!("'{}'", value.replace('\'', "''")))
            .collect();
        let listed = match literals.as_slice() {
            [] => None,
            [one] => Some(format!("{column} = {one}")),
            many => Some(format!("{column} IN ({})", many.join(", "))),
        };
        match (listed, null) {
            (Some(listed), true) => format!("({listed} OR {column} IS NULL)"),
            (Some(listed), false) => format!("({listed})"),
            (None, _) => format!("({column} IS NULL)"),
        }
    }

    /// `value` taken out of the pick when it is in it, otherwise added.
    pub(crate) fn toggle(&mut self, value: Option<String>) {
        match self.values.iter().position(|v| *v == value) {
            Some(at) => {
                self.values.remove(at);
            }
            None => self.values.push(value),
        }
    }
}

/// Every `$name` outside strings, quoted identifiers and comments, in order.
/// `$1`-style positional parameters are not names and are left alone.
pub(crate) fn placeholders(sql: &str) -> Vec<Placeholder> {
    let bytes = sql.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    while i < bytes.len() {
        match bytes[i] {
            quote @ (b'\'' | b'"') => {
                // A doubled quote inside is an escaped one; either way the
                // scan resumes after the closing quote.
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == quote {
                        if bytes.get(i + 1) == Some(&quote) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i = sql[i..].find('\n').map_or(bytes.len(), |end| i + end + 1);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = sql[i + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |end| i + 2 + end + 2);
            }
            b'$' => {
                let start = i;
                let mut end = i + 1;
                while end < bytes.len() && ident(bytes[end]) {
                    end += 1;
                }
                let tag = &sql[start..end];
                if bytes.get(end) == Some(&b'$') {
                    // `$$ … $$` or `$tag$ … $tag$`: a string; skip past its end.
                    let close = format!("{tag}$");
                    i = sql[end + 1..]
                        .find(&close)
                        .map_or(bytes.len(), |at| end + 1 + at + close.len());
                } else if end > start + 1 && !bytes[start + 1].is_ascii_digit() {
                    found.push(Placeholder {
                        name: sql[start + 1..end].to_string(),
                        range: start..end,
                    });
                    i = end;
                } else {
                    i = end.max(start + 1);
                }
            }
            _ => i += 1,
        }
    }
    found
}

/// `sql` with each `$name` replaced: by the picked value's predicate when
/// `picks` has one for that name, by `TRUE` when it does not.
pub(crate) fn apply(sql: &str, picks: &HashMap<String, Pick>) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut last = 0;
    for placeholder in placeholders(sql) {
        out.push_str(&sql[last..placeholder.range.start]);
        match picks.get(&placeholder.name) {
            Some(pick) => out.push_str(&pick.predicate()),
            None => out.push_str("TRUE"),
        }
        last = placeholder.range.end;
    }
    out.push_str(&sql[last..]);
    out
}

/// `sql` as it runs with nothing picked: every placeholder `TRUE`. What the
/// read-only check and `check` judge.
pub(crate) fn neutral(sql: &str) -> String {
    apply(sql, &HashMap::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(sql: &str) -> Vec<String> {
        placeholders(sql).into_iter().map(|p| p.name).collect()
    }

    #[test]
    fn placeholders_are_found_outside_strings_and_comments() {
        assert_eq!(
            names("SELECT * FROM t WHERE $channel AND ($region OR x = 1)"),
            ["channel", "region"]
        );
        assert_eq!(
            names("SELECT '$not', \"$nor\", $$ $body $$, $t$ $x $t$ -- $no\n/* $none */ FROM t WHERE $yes"),
            ["yes"]
        );
        assert_eq!(names("SELECT 'it''s $x' WHERE $ok"), ["ok"]);
        // Positional parameters and a lone dollar are not filters.
        assert!(names("SELECT $1, $ FROM t").is_empty());
    }

    #[test]
    fn nothing_picked_is_true_and_a_pick_is_a_quoted_comparison() {
        let sql = "FROM t WHERE $channel AND $day";
        assert_eq!(neutral(sql), "FROM t WHERE TRUE AND TRUE");
        let mut picks = HashMap::new();
        picks.insert(
            "channel".to_string(),
            Pick {
                column: "Sales \"Channel\"".into(),
                values: vec![Some("it's".into())],
            },
        );
        picks.insert(
            "day".to_string(),
            Pick {
                column: "day".into(),
                values: vec![None],
            },
        );
        assert_eq!(
            apply(sql, &picks),
            "FROM t WHERE (\"Sales \"\"Channel\"\"\" = 'it''s') \
             AND (\"day\" IS NULL)"
        );
    }

    #[test]
    fn a_pick_of_several_values_reads_any_of_them() {
        let mut pick = Pick {
            column: "kind".into(),
            values: vec![Some("api".into())],
        };
        pick.toggle(Some("o'web".into()));
        assert_eq!(pick.predicate(), "(\"kind\" IN ('api', 'o''web'))");
        pick.toggle(None);
        pick.toggle(Some("api".into()));
        assert_eq!(
            pick.values,
            [Some("o'web".to_string()), None],
            "a value toggled twice is out again"
        );
        assert_eq!(
            pick.predicate(),
            "(\"kind\" = 'o''web' OR \"kind\" IS NULL)"
        );
        pick.toggle(Some("o'web".into()));
        assert_eq!(pick.predicate(), "(\"kind\" IS NULL)");
    }

    #[test]
    fn several_values_select_their_rows_and_nulls_when_asked() {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        let count = |values: Vec<Option<&str>>| -> i64 {
            let predicate = Pick {
                column: "x".into(),
                values: values.into_iter().map(|v| v.map(String::from)).collect(),
            }
            .predicate();
            conn.query_row(
                &format!(
                    "SELECT count(*) FROM (VALUES (1), (2), (3), (NULL::INTEGER)) AS t(x) \
                     WHERE {predicate}"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(count(vec![Some("1"), Some("3")]), 2);
        assert_eq!(count(vec![Some("1"), None]), 2);
        assert_eq!(count(vec![Some("1"), Some("2"), None]), 3);
    }

    /// The grid shows both as NULL, and they are different rows.
    #[test]
    fn a_picked_null_and_the_text_null_select_their_own_rows() {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        let count = |value: Option<&str>| -> i64 {
            let predicate = Pick {
                column: "x".into(),
                values: vec![value.map(String::from)],
            }
            .predicate();
            conn.query_row(
                &format!(
                    "SELECT count(*) FROM (VALUES (NULL::VARCHAR), ('NULL'), ('ok')) AS t(x) \
                     WHERE {predicate}"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(count(None), 1);
        assert_eq!(count(Some("NULL")), 1);
        assert_eq!(count(Some("ok")), 1);
    }

    /// From a real result to the pick: the marker is what says which it was.
    #[test]
    fn a_result_says_which_cells_are_sql_null() {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        let crate::query::QueryOutcome::Rows(result) = crate::query::run_of(
            &conn,
            "SELECT * FROM (VALUES (1, NULL::VARCHAR), (2, 'NULL'), (3, 'ok')) AS t(n, x) ORDER BY n",
        )
        .unwrap() else {
            panic!("a SELECT returns rows");
        };
        assert_eq!(result.rows[0][1], "NULL");
        assert_eq!(result.rows[1][1], "NULL");
        assert!(result.is_sql_null(0, 1));
        assert!(!result.is_sql_null(1, 1));
        assert!(!result.is_sql_null(2, 1));
    }

    #[test]
    fn an_unterminated_string_ends_the_scan_without_panicking() {
        assert!(names("SELECT 'open $x").is_empty());
        assert!(names("SELECT /* open $x").is_empty());
        assert!(names("SELECT $$ open $x").is_empty());
        assert_eq!(names("SELECT $x"), ["x"]);
    }
}
