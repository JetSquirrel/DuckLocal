//! SQL the sidebar generates for its row actions: `SELECT` previews for
//! tables, columns, and S3 files, `ALTER COLUMN` for type edits — and the
//! table description "Copy table info" puts on the clipboard. Pure
//! functions, tested as such.

use super::model::{ColumnRef, TableRef};
use crate::schema::{NodeKind, TableInfo};
use crate::script::SearchPath;
use crate::ui::completion::{identifier_insert, relative_table_name};

/// Quoted table path for generated SQL, as short as `path` allows: after
/// `USE nl_railway`, a preview reads `FROM stations`.
fn qualified_table_name(table: &TableRef, path: Option<&SearchPath>) -> String {
    relative_table_name(path, &table.database, &table.schema, &table.name)
}

pub(super) fn select_star_sql(table: &TableRef, path: Option<&SearchPath>) -> String {
    format!(
        "SELECT *\nFROM {}\nLIMIT 100;",
        qualified_table_name(table, path)
    )
}

/// The whole relation, for a column overview: a `LIMIT` would summarize the
/// first rows instead of the table.
pub(super) fn overview_sql(table: &TableRef, path: Option<&SearchPath>) -> String {
    format!("FROM {};", qualified_table_name(table, path))
}

pub(super) fn select_column_sql(column: &ColumnRef, path: Option<&SearchPath>) -> String {
    format!(
        "SELECT {}\nFROM {}\nLIMIT 100;",
        identifier_insert(&column.name),
        qualified_table_name(&column.table, path)
    )
}

/// `new_type` stays raw: types with parameters (`DECIMAL(10,2)`) are valid
/// input, and the database owner is the one typing it. Always fully
/// qualified: it runs right away, not in the user's session context.
pub(super) fn alter_column_type_sql(column: &ColumnRef, new_type: &str) -> String {
    format!(
        "ALTER TABLE {} ALTER COLUMN {} SET DATA TYPE {new_type}",
        qualified_table_name(&column.table, None),
        identifier_insert(&column.name)
    )
}

pub(super) fn select_s3_file_sql(uri: &str) -> String {
    format!("SELECT *\nFROM '{}'\nLIMIT 100;", uri.replace('\'', "''"))
}

/// A table as Markdown, for pasting into a chat, an issue or an agent's
/// prompt: the name a query uses, what it is, its size, and every column
/// with its type.
pub(super) fn table_info_text(table: &TableInfo) -> String {
    let name = relative_table_name(None, &table.database, &table.schema, &table.name);
    let kind = match table.kind {
        NodeKind::Table => "table",
        NodeKind::View => "view",
    };
    let rows = table
        .estimated_rows
        .map(|rows| format!(", ~{rows} rows"))
        .unwrap_or_default();
    let mut text = format!(
        "{name} ({kind}{rows}, {} columns)\n\n| column | type |\n|---|---|\n",
        table.columns.len()
    );
    for column in &table.columns {
        // A pipe in a name would end the cell early.
        text.push_str(&format!(
            "| {} | {} |\n",
            column.name.replace('|', "\\|"),
            column.data_type
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: a `gpui_kit::*` glob anywhere in the
    // import chain drags gpui's `test` attribute macro into scope and shadows
    // the built-in `#[test]`.
    use super::{
        alter_column_type_sql, select_column_sql, select_s3_file_sql, select_star_sql,
        table_info_text,
    };
    use crate::schema::{ColumnInfo, NodeKind, TableInfo};
    use crate::script::SearchPath;
    use crate::ui::sidebar::model::{ColumnRef, TableRef};

    fn table_ref(name: &str, is_view: bool) -> TableRef {
        TableRef {
            database: "memory".to_string(),
            schema: "main".to_string(),
            name: name.to_string(),
            is_view,
        }
    }

    #[test]
    fn s3_file_sql_escapes_quotes() {
        assert_eq!(
            select_s3_file_sql("s3://logs/2024/a.parquet"),
            "SELECT *\nFROM 's3://logs/2024/a.parquet'\nLIMIT 100;"
        );
        assert_eq!(
            select_s3_file_sql("s3://logs/it's.csv"),
            "SELECT *\nFROM 's3://logs/it''s.csv'\nLIMIT 100;"
        );
    }

    #[test]
    fn generated_selects_qualify_and_quote() {
        assert_eq!(
            select_star_sql(&table_ref("orders", false), None),
            "SELECT *\nFROM memory.main.orders\nLIMIT 100;"
        );
        // Names that are not plain identifiers get double-quoted.
        assert_eq!(
            select_star_sql(&table_ref("order items", false), None),
            "SELECT *\nFROM memory.main.\"order items\"\nLIMIT 100;"
        );

        let column = ColumnRef {
            table: table_ref("orders", false),
            name: "total amount".to_string(),
            data_type: "DOUBLE".to_string(),
        };
        assert_eq!(
            select_column_sql(&column, None),
            "SELECT \"total amount\"\nFROM memory.main.orders\nLIMIT 100;"
        );
    }

    #[test]
    fn after_use_previews_drop_what_the_search_path_supplies() {
        let path = |database: &str, schema: &str| SearchPath {
            database: database.into(),
            schema: schema.into(),
        };
        let orders = table_ref("orders", false);
        assert_eq!(
            select_star_sql(&orders, Some(&path("memory", "main"))),
            "SELECT *\nFROM orders\nLIMIT 100;"
        );
        assert_eq!(
            select_star_sql(&orders, Some(&path("memory", "staging"))),
            "SELECT *\nFROM main.orders\nLIMIT 100;"
        );
        assert_eq!(
            select_star_sql(&orders, Some(&path("nl_railway", "main"))),
            "SELECT *\nFROM memory.main.orders\nLIMIT 100;"
        );
    }

    #[test]
    fn alter_type_sql_quotes_identifiers_and_keeps_raw_type() {
        let column = ColumnRef {
            table: table_ref("orders", false),
            name: "amount".to_string(),
            data_type: "INTEGER".to_string(),
        };
        assert_eq!(
            alter_column_type_sql(&column, "DECIMAL(10,2)"),
            "ALTER TABLE memory.main.orders ALTER COLUMN amount SET DATA TYPE DECIMAL(10,2)"
        );
    }

    #[test]
    fn quotes_escape_embedded_double_quotes() {
        assert_eq!(
            select_star_sql(&table_ref("we\"ird", false), None),
            "SELECT *\nFROM memory.main.\"we\"\"ird\"\nLIMIT 100;"
        );
    }

    #[test]
    fn alter_type_sql_is_accepted_by_duckdb() {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE orders(id INTEGER, amount INTEGER); INSERT INTO orders VALUES (1, 42);",
        )
        .unwrap();
        let column = ColumnRef {
            table: table_ref("orders", false),
            name: "amount".to_string(),
            data_type: "INTEGER".to_string(),
        };
        conn.execute_batch(&alter_column_type_sql(&column, "DECIMAL(10,2)"))
            .unwrap();
        let value: String = conn
            .query_row("SELECT amount::VARCHAR FROM orders", [], |r| r.get(0))
            .unwrap();
        assert_eq!(value, "42.00");
    }

    #[test]
    fn table_info_lists_every_column_as_markdown() {
        let column = |name: &str, data_type: &str| ColumnInfo {
            name: name.to_string(),
            data_type: data_type.to_string(),
        };
        let table = TableInfo {
            database: "memory".to_string(),
            schema: "main".to_string(),
            name: "orders".to_string(),
            kind: NodeKind::Table,
            estimated_rows: Some(42),
            columns: vec![column("date", "DATE"), column("a|b", "VARCHAR")],
        };
        assert_eq!(
            table_info_text(&table),
            "memory.main.orders (table, ~42 rows, 2 columns)\n\n\
             | column | type |\n|---|---|\n\
             | date | DATE |\n\
             | a\\|b | VARCHAR |\n"
        );
    }
}
