//! SQL the sidebar generates for its row actions: `SELECT` previews for
//! tables, columns, and S3 files, and `ALTER COLUMN` for type edits. Pure
//! functions, tested as such.

use super::model::{ColumnRef, TableRef};
use crate::ui::completion::identifier_insert;

/// Fully-qualified, quoted table path for generated SQL.
fn qualified_table_name(table: &TableRef) -> String {
    format!(
        "{}.{}.{}",
        identifier_insert(&table.database),
        identifier_insert(&table.schema),
        identifier_insert(&table.name)
    )
}

pub(super) fn select_star_sql(table: &TableRef) -> String {
    format!("SELECT *\nFROM {}\nLIMIT 100;", qualified_table_name(table))
}

pub(super) fn select_column_sql(column: &ColumnRef) -> String {
    format!(
        "SELECT {}\nFROM {}\nLIMIT 100;",
        identifier_insert(&column.name),
        qualified_table_name(&column.table)
    )
}

/// `new_type` stays raw: types with parameters (`DECIMAL(10,2)`) are valid
/// input, and the database owner is the one typing it.
pub(super) fn alter_column_type_sql(column: &ColumnRef, new_type: &str) -> String {
    format!(
        "ALTER TABLE {} ALTER COLUMN {} SET DATA TYPE {new_type}",
        qualified_table_name(&column.table),
        identifier_insert(&column.name)
    )
}

pub(super) fn select_s3_file_sql(uri: &str) -> String {
    format!("SELECT *\nFROM '{}'\nLIMIT 100;", uri.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: a `gpui_kit::*` glob anywhere in the
    // import chain drags gpui's `test` attribute macro into scope and shadows
    // the built-in `#[test]`.
    use super::{
        alter_column_type_sql, select_column_sql, select_s3_file_sql, select_star_sql,
    };
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
            select_star_sql(&table_ref("orders", false)),
            "SELECT *\nFROM memory.main.orders\nLIMIT 100;"
        );
        // Names that are not plain identifiers get double-quoted.
        assert_eq!(
            select_star_sql(&table_ref("order items", false)),
            "SELECT *\nFROM memory.main.\"order items\"\nLIMIT 100;"
        );

        let column = ColumnRef {
            table: table_ref("orders", false),
            name: "total amount".to_string(),
            data_type: "DOUBLE".to_string(),
        };
        assert_eq!(
            select_column_sql(&column),
            "SELECT \"total amount\"\nFROM memory.main.orders\nLIMIT 100;"
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
            select_star_sql(&table_ref("we\"ird", false)),
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
}
