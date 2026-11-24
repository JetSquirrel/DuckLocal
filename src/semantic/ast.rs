//! DuckDB's own parser and SQL writer, as the semantic layer's.
//!
//! `json_serialize_sql` reads SQL into DuckDB's parse tree as JSON and
//! `json_deserialize_sql` writes a tree back as SQL. A model's expressions are
//! read once into trees; compiling a request swaps column references for the
//! trees they stand for and has DuckDB write the result. Nothing from a model
//! or a request is pasted into SQL text: what reaches the query is what DuckDB
//! itself writes from a tree, quoting and precedence included.
//!
//! The parser runs on its own in-memory connection that holds no data and can
//! reach no file, URL or extension. It never executes what it reads.

use std::collections::HashSet;

use duckdb::Connection;
use serde_json::Value;

/// The parser connection and what it knows about functions.
pub(crate) struct Sql {
    conn: Connection,
    /// Lower-case names of every aggregate function DuckDB has loaded.
    aggregates: HashSet<String>,
    /// `SELECT 1` as a tree: the statement every expression is written in.
    template: Value,
}

/// A grouping or ordering construct a single expression cannot carry.
const CLAUSES: &[&str] = &["where_clause", "having", "qualify", "sample"];

impl Sql {
    pub(crate) fn open() -> Result<Self, String> {
        let config = crate::db::config().map_err(|e| e.to_string())?;
        let conn = Connection::open_in_memory_with_flags(config).map_err(|e| e.to_string())?;
        // The same functions the query will run with: ICU's time zone-aware
        // functions are loaded on every connection DuckLocal opens.
        crate::db::preload_extensions(&conn);
        conn.execute_batch(
            "SET enable_external_access = false;
             SET autoload_known_extensions = false;
             SET lock_configuration = true;",
        )
        .map_err(|e| e.to_string())?;
        let aggregates = {
            let mut statement = conn
                .prepare(
                    "SELECT DISTINCT lower(function_name) FROM duckdb_functions() \
                     WHERE function_type = 'aggregate'",
                )
                .map_err(|e| e.to_string())?;
            let names = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            names
                .collect::<Result<HashSet<_>, _>>()
                .map_err(|e| e.to_string())?
        };
        let mut sql = Self {
            conn,
            aggregates,
            template: Value::Null,
        };
        sql.template = sql.serialize("SELECT 1")?;
        Ok(sql)
    }

    /// The parse tree of `sql`, or DuckDB's reason it is not one statement it
    /// can serialize.
    fn serialize(&self, sql: &str) -> Result<Value, String> {
        let text: String = self
            .conn
            .query_row("SELECT json_serialize_sql(?::VARCHAR)", [sql], |row| {
                row.get(0)
            })
            .map_err(|e| e.to_string())?;
        let tree: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if tree["error"] == Value::Bool(true) {
            let message = tree["error_message"]
                .as_str()
                .unwrap_or("not a statement DuckDB can read")
                .to_string();
            return Err(message);
        }
        Ok(tree)
    }

    /// `text` as one expression's tree. It must be exactly one expression: no
    /// second item, no `FROM`, no clause, no subquery, no `*` and nothing a
    /// later statement could hide behind.
    pub(crate) fn expression(&self, text: &str) -> Result<Value, String> {
        if text.trim().is_empty() {
            return Err("the expression is empty".to_string());
        }
        // On its own line: a trailing `--` comment ends with the text.
        let tree = self.serialize(&format!("SELECT {text}\n"))?;
        let statements = tree["statements"].as_array().map_or(0, Vec::len);
        if statements != 1 {
            return Err(format!(
                "expected one expression, found {statements} statements"
            ));
        }
        let node = &tree["statements"][0]["node"];
        let single = node["type"] == "SELECT_NODE"
            && node["from_table"]["type"] == "EMPTY"
            && CLAUSES.iter().all(|clause| node[*clause].is_null())
            && empty(&node["group_expressions"])
            && empty(&node["group_sets"])
            && empty(&node["modifiers"])
            && empty(&node["cte_map"]["map"]);
        let items = node["select_list"].as_array().map_or(0, Vec::len);
        if !single || items != 1 {
            return Err("expected one expression and nothing around it".to_string());
        }
        let mut expression = node["select_list"][0].clone();
        expression["alias"] = Value::String(String::new());
        if let Some(class) = first_class(&expression, &["SUBQUERY", "STAR", "PARAMETER", "LAMBDA"])
        {
            return Err(match class {
                "SUBQUERY" => "a subquery could read what the model does not declare".to_string(),
                "STAR" => "* and COLUMNS() stand for columns the model does not name".to_string(),
                "PARAMETER" => "a parameter has no value here".to_string(),
                _ => "lambda functions are not supported yet".to_string(),
            });
        }
        Ok(expression)
    }

    /// The SQL DuckDB writes for `expression`.
    pub(crate) fn render(&self, expression: &Value) -> Result<String, String> {
        let mut tree = self.template.clone();
        tree["statements"][0]["node"]["select_list"] = Value::Array(vec![expression.clone()]);
        let text: String = self
            .conn
            .query_row(
                "SELECT json_deserialize_sql(?::JSON)",
                [tree.to_string()],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        text.strip_prefix("SELECT ")
            .map(str::to_string)
            .ok_or_else(|| format!("DuckDB wrote an unexpected statement: {text}"))
    }

    /// Whether `sql` is one SELECT statement, by DuckDB's parser: the check
    /// on every query the compiler assembles.
    pub(crate) fn is_one_select(&self, sql: &str) -> Result<(), String> {
        let tree = self.serialize(sql)?;
        let statements = tree["statements"].as_array().map_or(0, Vec::len);
        if statements != 1 || tree["statements"][0]["node"]["type"] != "SELECT_NODE" {
            return Err("the compiled SQL is not one SELECT".to_string());
        }
        Ok(())
    }

    /// The parts of a table name: `catalog.schema.table` as written in a
    /// dataset's `source`, which must name a table and nothing else.
    pub(crate) fn table_name(&self, text: &str) -> Result<Vec<String>, String> {
        // DuckDB reads `FROM 'file.csv'` as a table named for the file; a
        // source names a relation, and the folder's views say which file.
        if text.contains('\'') {
            return Err("not a table name".to_string());
        }
        let tree = self
            .serialize(&format!("SELECT * FROM {text}\n"))
            .map_err(|_| "not a table name".to_string())?;
        let statements = tree["statements"].as_array().map_or(0, Vec::len);
        let node = &tree["statements"][0]["node"];
        let table = &node["from_table"];
        if statements != 1
            || node["type"] != "SELECT_NODE"
            || table["type"] != "BASE_TABLE"
            || table["alias"]
                .as_str()
                .is_some_and(|alias| !alias.is_empty())
            || !table["sample"].is_null()
            || CLAUSES.iter().any(|clause| !node[*clause].is_null())
            || !empty(&node["modifiers"])
        {
            return Err("not a table name".to_string());
        }
        let parts: Vec<String> = ["catalog_name", "schema_name", "table_name"]
            .iter()
            .filter_map(|key| table[*key].as_str())
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect();
        if parts.is_empty() {
            return Err("not a table name".to_string());
        }
        Ok(parts)
    }

    pub(crate) fn is_aggregate(&self, node: &Value) -> bool {
        node["class"] == "FUNCTION"
            && node["is_operator"] != Value::Bool(true)
            && node["function_name"]
                .as_str()
                .is_some_and(|name| self.aggregates.contains(&name.to_lowercase()))
    }

    /// DuckDB's version, for what a compiled query records it was made with.
    pub(crate) fn version(&self) -> String {
        self.conn
            .query_row("SELECT version()", [], |row| row.get(0))
            .unwrap_or_default()
    }
}

fn empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

/// Every expression node in `value`, outermost first.
pub(crate) fn nodes(value: &Value) -> Vec<&Value> {
    fn walk<'a>(value: &'a Value, out: &mut Vec<&'a Value>) {
        match value {
            Value::Object(map) => {
                if map.contains_key("class") {
                    out.push(value);
                }
                map.values().for_each(|child| walk(child, out));
            }
            Value::Array(items) => items.iter().for_each(|child| walk(child, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(value, &mut out);
    out
}

fn first_class<'a>(value: &Value, classes: &[&'a str]) -> Option<&'a str> {
    nodes(value).into_iter().find_map(|node| {
        classes
            .iter()
            .find(|class| node["class"] == **class)
            .copied()
    })
}

/// A column reference's name parts, `["events", "user_name"]`.
pub(crate) fn column_names(node: &Value) -> Option<Vec<String>> {
    if node["class"] != "COLUMN_REF" {
        return None;
    }
    node["column_names"].as_array().map(|parts| {
        parts
            .iter()
            .filter_map(|part| part.as_str().map(str::to_string))
            .collect()
    })
}

/// A column reference to `parts`, as DuckDB's parser would have made it.
pub(crate) fn column(parts: &[String]) -> Value {
    serde_json::json!({
        "class": "COLUMN_REF",
        "type": "COLUMN_REF",
        "alias": "",
        "column_names": parts,
    })
}

/// Replace every column reference in `value`, outermost first: `f` gets its
/// name parts and returns what stands in its place. A replacement is not
/// walked again.
pub(crate) fn replace_columns<E>(
    value: &mut Value,
    f: &mut dyn FnMut(&[String]) -> Result<Value, E>,
) -> Result<(), E> {
    if let Some(parts) = column_names(value) {
        let mut replacement = f(&parts)?;
        if let (Some(alias), Some(target)) =
            (value.get("alias").cloned(), replacement.as_object_mut())
        {
            target.insert("alias".to_string(), alias);
        }
        *value = replacement;
        return Ok(());
    }
    match value {
        Value::Object(map) => {
            for child in map.values_mut() {
                replace_columns(child, f)?;
            }
        }
        Value::Array(items) => {
            for child in items {
                replace_columns(child, f)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// What an expression aggregates, as the compiler needs to know it.
#[derive(Debug, Default)]
pub(crate) struct Shape {
    /// It calls an aggregate function.
    pub aggregates: bool,
    /// It calls a window function.
    pub window: Option<String>,
    /// An aggregate sits inside another's arguments.
    pub nested: bool,
    /// Column references inside an aggregate's arguments, filter or order.
    pub aggregated: Vec<Vec<String>>,
    /// Column references outside every aggregate.
    pub bare: Vec<Vec<String>>,
    /// For each outermost aggregate call, the column references it reads.
    pub calls: Vec<Vec<Vec<String>>>,
}

impl Sql {
    pub(crate) fn shape(&self, value: &Value) -> Shape {
        fn walk(sql: &Sql, value: &Value, inside: Option<usize>, shape: &mut Shape) {
            match value {
                Value::Object(map) => {
                    let mut inside = inside;
                    if map.contains_key("class") {
                        if let Some(parts) = column_names(value) {
                            if let Some(call) = inside {
                                shape.calls[call].push(parts.clone());
                                shape.aggregated.push(parts);
                            } else {
                                shape.bare.push(parts);
                            }
                            return;
                        }
                        if value["class"] == "WINDOW" && shape.window.is_none() {
                            shape.window = Some(
                                value["function_name"]
                                    .as_str()
                                    .unwrap_or("window")
                                    .to_string(),
                            );
                        }
                        if sql.is_aggregate(value) {
                            shape.nested |= inside.is_some();
                            shape.aggregates = true;
                            if inside.is_none() {
                                shape.calls.push(Vec::new());
                                inside = Some(shape.calls.len() - 1);
                            }
                        }
                    }
                    map.values()
                        .for_each(|child| walk(sql, child, inside, shape));
                }
                Value::Array(items) => items
                    .iter()
                    .for_each(|child| walk(sql, child, inside, shape)),
                _ => {}
            }
        }
        let mut shape = Shape::default();
        walk(self, value, None, &mut shape);
        shape
    }
}

/// `"name"`, for SQL the compiler writes around DuckDB's own.
pub(crate) fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sql() -> Sql {
        Sql::open().unwrap()
    }

    #[test]
    fn an_expression_round_trips_through_duckdb() {
        let sql = sql();
        let tree = sql
            .expression("COUNT(events.event_id) FILTER (WHERE events.error_code = 'it''s')")
            .unwrap();
        assert_eq!(
            sql.render(&tree).unwrap(),
            "count(events.event_id) FILTER (WHERE (events.error_code = 'it''s'))"
        );
    }

    #[test]
    fn anything_but_one_bare_expression_is_refused() {
        let sql = sql();
        for text in [
            "",
            "1, 2",
            "1 FROM t",
            "1 WHERE true",
            "1); DROP TABLE t; --",
            "x) UNION ALL SELECT (1",
            "(SELECT 1)",
            "x IN (SELECT y FROM t)",
            "COLUMNS(*)",
            "$1",
            "list_transform(l, x -> x + 1)",
            "1 GROUP BY 1",
            "1 LIMIT 1",
            "1; SELECT 2",
        ] {
            assert!(sql.expression(text).is_err(), "{text:?} was accepted");
        }
    }

    #[test]
    fn a_trailing_comment_cannot_swallow_anything() {
        let sql = sql();
        let tree = sql.expression("count(x) -- a comment").unwrap();
        assert_eq!(sql.render(&tree).unwrap(), "count(x)");
    }

    #[test]
    fn replaced_columns_are_quoted_by_duckdb() {
        let sql = sql();
        let mut tree = sql.expression("sum(events.amount)").unwrap();
        replace_columns::<()>(&mut tree, &mut |parts| {
            Ok(column(&["odd \"name\"".to_string(), parts[1].clone()]))
        })
        .unwrap();
        assert_eq!(
            sql.render(&tree).unwrap(),
            "sum(\"odd \"\"name\"\"\".amount)"
        );
    }

    #[test]
    fn aggregates_are_found_by_duckdb_s_catalog() {
        let sql = sql();
        let shape = sql.shape(
            &sql.expression("sum(CASE WHEN e.code = 'x' THEN 1 END) * 1.0 / count(e.id) + e.bare")
                .unwrap(),
        );
        assert!(shape.aggregates);
        assert_eq!(shape.aggregated.len(), 2);
        assert_eq!(shape.bare, vec![vec!["e".to_string(), "bare".to_string()]]);
        for text in [
            "PERCENTILE_CONT(0.9) WITHIN GROUP (ORDER BY e.x)",
            "approx_count_distinct(e.x)",
            "median(e.x)",
            "count(*)",
        ] {
            assert!(
                sql.shape(&sql.expression(text).unwrap()).aggregates,
                "{text}"
            );
        }
        let window = sql.shape(
            &sql.expression("sum(count(e.x)) OVER (ORDER BY e.t)")
                .unwrap(),
        );
        assert_eq!(window.window.as_deref(), Some("sum"));
        assert!(!sql.shape(&sql.expression("lower(e.x)").unwrap()).aggregates);
    }

    #[test]
    fn a_source_is_a_table_name_and_nothing_else() {
        let sql = sql();
        assert_eq!(sql.table_name("events").unwrap(), vec!["events"]);
        assert_eq!(
            sql.table_name("warehouse.audit.\"Events\"").unwrap(),
            vec!["warehouse", "audit", "Events"]
        );
        for text in [
            "read_csv('x.csv')",
            "events e",
            "events WHERE true",
            "'x.csv'",
            "(SELECT 1)",
            "events; DROP TABLE x",
            "events, other",
        ] {
            assert!(sql.table_name(text).is_err(), "{text:?} was accepted");
        }
    }
}
