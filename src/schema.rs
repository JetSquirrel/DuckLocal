//! Catalog introspection for the schema sidebar.
//! Blocking functions; call via `smol::unblock` from UI code.

use std::collections::HashMap;

use anyhow::Result;
use duckdb::Connection;

/// `(database, schema, table)`, as the catalog views report them.
type TableKey = (String, String, String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Database,
    Schema,
    Table,
    View,
    Column,
}

#[derive(Clone, Debug)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
}

#[derive(Clone, Debug)]
pub struct TableInfo {
    pub database: String,
    pub schema: String,
    pub name: String,
    pub kind: NodeKind,
    pub estimated_rows: Option<i64>,
    pub comment: Option<String>,
    pub columns: Vec<ColumnInfo>,
}

/// A database in the catalog tree: its schemas' tables and views, flattened
/// with schema name attached so the UI can group as it likes.
#[derive(Clone, Debug)]
pub struct DatabaseInfo {
    pub name: String,
    pub tables: Vec<TableInfo>,
}

pub fn load_catalog() -> Result<Vec<DatabaseInfo>> {
    crate::db::with_connection(load_catalog_of)
}

pub fn load_catalog_of(conn: &Connection) -> Result<Vec<DatabaseInfo>> {
    let mut stmt = conn.prepare(
        "SELECT database_name, schema_name, table_name, estimated_size, comment
         FROM duckdb_tables()
         WHERE database_name != 'system'
           AND schema_name NOT IN ('information_schema', 'pg_catalog', 'system')
         ORDER BY database_name, schema_name, table_name",
    )?;
    let tables = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut view_stmt = conn.prepare(
        "SELECT database_name, schema_name, view_name
         FROM duckdb_views()
         WHERE database_name != 'system'
           AND schema_name NOT IN ('information_schema', 'pg_catalog', 'system')
         ORDER BY database_name, schema_name, view_name",
    )?;
    let views = view_stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    // Every table's columns in one pass. Querying `duckdb_columns()` per table
    // instead costs a full catalog scan per table, which dominates catalog
    // load time once a database has more than a handful of tables.
    let mut columns_by_table = columns_by_table(conn)?;

    let mut databases: Vec<DatabaseInfo> = Vec::new();
    let mut push_table = |database: String,
                          schema: String,
                          name: String,
                          kind: NodeKind,
                          estimated_rows: Option<i64>,
                          comment: Option<String>| {
        let columns = columns_by_table
            .remove(&(database.clone(), schema.clone(), name.clone()))
            .unwrap_or_default();
        let db = match databases.iter_mut().find(|d| d.name == database) {
            Some(db) => db,
            None => {
                databases.push(DatabaseInfo {
                    name: database.clone(),
                    tables: Vec::new(),
                });
                databases.last_mut().unwrap()
            }
        };
        db.tables.push(TableInfo {
            database,
            schema,
            name,
            kind,
            estimated_rows,
            comment,
            columns,
        });
    };

    for (database, schema, name, estimated, comment) in tables {
        push_table(database, schema, name, NodeKind::Table, estimated, comment);
    }
    for (database, schema, name) in views {
        push_table(database, schema, name, NodeKind::View, None, None);
    }
    Ok(databases)
}

/// Columns of every table and view, keyed by `(database, schema, table)` and
/// kept in `column_index` order within each entry.
fn columns_by_table(conn: &Connection) -> Result<HashMap<TableKey, Vec<ColumnInfo>>> {
    let mut stmt = conn.prepare(
        "SELECT database_name, schema_name, table_name, column_name, data_type
         FROM duckdb_columns()
         WHERE database_name != 'system'
           AND schema_name NOT IN ('information_schema', 'pg_catalog', 'system')
         ORDER BY database_name, schema_name, table_name, column_index",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            (
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ),
            ColumnInfo {
                name: row.get(3)?,
                data_type: row.get(4)?,
            },
        ))
    })?;

    let mut by_table: HashMap<TableKey, Vec<ColumnInfo>> = HashMap::new();
    for row in rows {
        let (key, column) = row?;
        by_table.entry(key).or_default().push(column);
    }
    Ok(by_table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_lists_tables_views_and_columns() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE orders(id INTEGER, amount DOUBLE);
             CREATE VIEW big_orders AS SELECT * FROM orders WHERE amount > 100;",
        )
        .unwrap();
        let catalog = load_catalog_of(&conn).unwrap();
        let all: Vec<&TableInfo> = catalog.iter().flat_map(|d| d.tables.iter()).collect();
        let orders = all.iter().find(|t| t.name == "orders").unwrap();
        assert_eq!(orders.kind, NodeKind::Table);
        assert_eq!(orders.columns.len(), 2);
        assert_eq!(orders.columns[1].data_type, "DOUBLE");
        let view = all.iter().find(|t| t.name == "big_orders").unwrap();
        assert_eq!(view.kind, NodeKind::View);
    }
}
