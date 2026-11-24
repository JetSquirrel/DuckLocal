//! Text spliced into DuckDB SQL. Every name or value the app builds into a
//! statement goes through one of these, so the escaping is written, and
//! reviewed, in one place.

/// `name` as a double-quoted identifier: a table, view, column, schema or
/// database, whatever its case or characters.
pub(crate) fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `text` as a single-quoted string literal: a path, a pattern, a value.
pub(crate) fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_double_their_quotes() {
        assert_eq!(ident("orders"), "\"orders\"");
        assert_eq!(ident("order items"), "\"order items\"");
        assert_eq!(ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(ident(""), "\"\"");
    }

    #[test]
    fn literals_double_their_quotes() {
        assert_eq!(literal("data.csv"), "'data.csv'");
        assert_eq!(literal("O'Brien's.csv"), "'O''Brien''s.csv'");
        assert_eq!(literal("say \"hi\""), "'say \"hi\"'");
        assert_eq!(literal(""), "''");
    }

    #[test]
    fn duckdb_reads_back_what_was_quoted() {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        let name = "it's a \"view\"";
        let value = "O'Brien \"Jr\"";
        conn.execute_batch(&format!(
            "CREATE VIEW {} AS SELECT {} AS v",
            ident(name),
            literal(value)
        ))
        .unwrap();
        let back: String = conn
            .query_row(&format!("SELECT v FROM {}", ident(name)), [], |r| r.get(0))
            .unwrap();
        assert_eq!(back, value);
    }
}
