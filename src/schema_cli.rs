//! `ducklocal schema`: what there is to query, sized for a prompt.
//!
//! An agent about to write SQL needs the relations and their columns, and
//! learning them one `DESCRIBE` at a time costs a call per table. This answers
//! in one: data files (PATHs — files, folders and globs, expanded the way the
//! GUI expands a drop) and, with a database, its tables and views.
//!
//! A small catalog comes back whole. A large one would drown the prompt it is
//! meant for, so past [`FULL_RELATIONS`] relations or [`FULL_COLUMNS`] columns
//! only the [`SUMMARY_FULL`] largest keep their columns; the rest are listed
//! by name, kind, rows and column count, and the output says how to ask for
//! one of them whole (`--table`) or for everything (`--full`).
//!
//! `--stats` goes one level deeper on one relation: exact per-column
//! statistics (nulls, distinct, min/max, median, decimals, day coverage) —
//! what decides a chart's axis before one is drawn. It is `profile.rs`'s
//! report; `ducklocal profile` remains as its deprecated spelling.
//!
//! Every relation carries `from`: the SQL that reads it, ready to paste after
//! `FROM`. Row counts are what is cheap to know — a table's estimate, a
//! Parquet footer's count — and `null` where knowing would mean a scan.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::cli::{parse_args, Arg, CliError, FlagSpec};

pub(crate) const SCHEMA_HELP: &str = "\
Usage: ducklocal schema [PATH ...] [--database PATH] [--table NAME] [--full|--stats]

What there is to query, as JSON sized for a prompt: data files (PATHs are
files, folders or globs) and, with --database, a database's tables and views.
A small catalog lists every relation with its columns; a large one gives the
columns of the largest few, and name, kind, rows and column count for the rest.

Options:
  --database PATH    Existing database file, opened read-only
  --table NAME       One relation, whole: a table, a view, or a PATH's name
  --full             Every relation with its columns, however many
  --stats            Exact per-column statistics of one relation: the only one
                     named, or --table NAME. Type, nulls, distinct, min and
                     max, plus median and decimals for numbers and day
                     coverage for dates. A workbook's first sheet is used.

Output: {detail, relations: [{name, kind, from, rows, column_count, columns?}],
without_columns, hint?}. `from` is the SQL that reads the relation. `rows` is
an estimate for tables, exact for Parquet, and null where it would need a scan.
With --stats: {target, row_count, columns: [{name, type, nulls, distinct, ...}]}.
";

const SPEC: &[FlagSpec] = &[
    FlagSpec {
        name: "--database",
        takes_value: true,
    },
    FlagSpec {
        name: "--table",
        takes_value: true,
    },
    FlagSpec {
        name: "--full",
        takes_value: false,
    },
    FlagSpec {
        name: "--stats",
        takes_value: false,
    },
];

/// A catalog up to this many relations comes back whole.
const FULL_RELATIONS: usize = 12;
/// …and up to this many columns across them.
const FULL_COLUMNS: usize = 400;
/// How many relations keep their columns in a summary.
const SUMMARY_FULL: usize = 5;

/// One queryable thing, before the output decides how much of it to show.
struct Relation {
    name: String,
    kind: &'static str,
    from: String,
    path: Option<String>,
    /// What `--stats` profiles: a file's path as shown, or a table's name.
    target: String,
    rows: Option<i64>,
    columns: Vec<(String, String)>,
    /// Why its columns could not be read: the file is there but DuckDB could
    /// not describe it.
    error: Option<String>,
    /// A workbook's sheets, which have no table function to read them by.
    sheets: Option<Vec<String>>,
}

#[hotpath::measure]
pub(crate) fn run(args: &[OsString]) -> Result<String, CliError> {
    let mut paths = Vec::new();
    let mut database = None;
    let mut table = None;
    let mut full = false;
    let mut stats = false;
    for arg in parse_args("schema", args, SPEC, &[])? {
        match arg {
            Arg::Flag("--database", Some(value)) => {
                database = Some(crate::cli::database_path(&value)?);
            }
            Arg::Flag("--table", Some(value)) => {
                table = Some(
                    value
                        .into_string()
                        .map_err(|_| CliError::argument("--table must be UTF-8"))?,
                );
            }
            Arg::Flag("--full", None) => full = true,
            Arg::Flag("--stats", None) => stats = true,
            Arg::Positional(value) => paths.push(value.to_string_lossy().into_owned()),
            _ => {
                return Err(CliError::failure(
                    "internal",
                    "the argument walker produced a flag schema does not declare",
                ))
            }
        }
    }

    // PATHs resolve as the GUI resolves them, database files included: a
    // `.duckdb` named as a PATH is the same request as --database.
    let sources = crate::sources::resolve(&paths);
    if !sources.problems.is_empty() {
        return Err(CliError::argument(sources.problems.join("\n")));
    }
    if let Some(named) = sources.database {
        if database.is_some() {
            return Err(CliError::argument(
                "Name one database: a PATH that is a database and --database are both given",
            ));
        }
        database = Some(PathBuf::from(named));
    }
    if paths.is_empty() && database.is_none() {
        return Err(
            CliError::argument("Name data files, folders or globs, or --database PATH").with_hint(
                "e.g. ducklocal schema ./data/  or  ducklocal schema --database warehouse.duckdb",
            ),
        );
    }

    if stats && full {
        return Err(CliError::argument(
            "--stats and --full are two answers; ask for one",
        ));
    }

    let connection = crate::cli::open(database.clone(), false)?;
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut relations: Vec<Relation> = sources
        .files
        .iter()
        .map(|file| file_relation(&connection, file, &cwd))
        .collect();
    if database.is_some() {
        relations.extend(database_relations(&connection)?);
    }

    if let Some(wanted) = table {
        let found = relations
            .into_iter()
            .find(|r| names_match(r, &wanted))
            .ok_or_else(|| {
                CliError::argument(format!("No relation named {wanted:?}"))
                    .with_hint("Run the same command without --table to list the names")
            })?;
        if stats {
            drop(connection);
            return crate::cli::profile_target(&found.target, database);
        }
        return Ok(json!({
            "detail": "full",
            "relations": [entry(&found, true)],
            "without_columns": 0,
        })
        .to_string());
    }

    if stats {
        let [only] = relations.as_slice() else {
            return Err(CliError::argument(format!(
                "--stats profiles one relation; these arguments name {}",
                relations.len()
            ))
            .with_hint("Add --table NAME to pick one"));
        };
        let target = only.target.clone();
        drop(connection);
        return crate::cli::profile_target(&target, database);
    }

    let total_columns: usize = relations.iter().map(|r| r.columns.len()).sum();
    let whole = full || (relations.len() <= FULL_RELATIONS && total_columns <= FULL_COLUMNS);
    let keep = if whole {
        vec![true; relations.len()]
    } else {
        largest(&relations, SUMMARY_FULL)
    };
    let without_columns = keep.iter().filter(|kept| !**kept).count();
    let mut output = json!({
        "detail": if whole { "full" } else { "summary" },
        "relations": relations
            .iter()
            .zip(&keep)
            .map(|(relation, kept)| entry(relation, *kept))
            .collect::<Vec<_>>(),
        "without_columns": without_columns,
    });
    if !whole {
        output["hint"] = json!(format!(
            "{without_columns} relations are listed without columns; \
             add --table NAME for one of them, or --full for all"
        ));
    }
    Ok(output.to_string())
}

/// A relation as output: everything, or everything but the column list.
fn entry(relation: &Relation, with_columns: bool) -> Value {
    let mut value = json!({
        "name": relation.name,
        "kind": relation.kind,
        "from": relation.from,
        "rows": relation.rows,
        "column_count": relation.columns.len(),
    });
    if let Some(path) = &relation.path {
        value["path"] = json!(path);
    }
    if let Some(error) = &relation.error {
        value["error"] = json!(error);
    }
    if let Some(sheets) = &relation.sheets {
        value["sheets"] = json!(sheets);
    }
    if with_columns {
        value["columns"] = json!(relation
            .columns
            .iter()
            .map(|(name, ty)| json!({"name": name, "type": ty}))
            .collect::<Vec<_>>());
    }
    value
}

/// Which relations keep their columns in a summary: the `n` with the most
/// rows, unknown counts after known ones, catalog order breaking ties.
fn largest(relations: &[Relation], n: usize) -> Vec<bool> {
    let mut order: Vec<usize> = (0..relations.len()).collect();
    order.sort_by_key(|&ix| std::cmp::Reverse(relations[ix].rows.map_or(-1, |rows| rows)));
    let mut keep = vec![false; relations.len()];
    for &ix in order.iter().take(n) {
        keep[ix] = true;
    }
    keep
}

/// `--table` matches a relation's name as listed, its unqualified table name,
/// or — for a file — the path as given or its file name.
fn names_match(relation: &Relation, wanted: &str) -> bool {
    let wanted = wanted.trim();
    if relation.name.eq_ignore_ascii_case(wanted) {
        return true;
    }
    if let Some(last) = relation.name.rsplit('.').next() {
        if relation.kind != "file" && last.eq_ignore_ascii_case(wanted) {
            return true;
        }
    }
    relation.path.as_deref().is_some_and(|path| {
        path == wanted
            || Path::new(path)
                .file_name()
                .is_some_and(|name| name.to_string_lossy() == wanted)
    })
}

/// A data file as a relation: its columns per DuckDB's reader, and its row
/// count when the format keeps one.
#[hotpath::measure]
fn file_relation(connection: &duckdb::Connection, file: &str, cwd: &Path) -> Relation {
    let shown = Path::new(file)
        .strip_prefix(cwd)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file.to_string());
    let literal = format!("'{}'", shown.replace('\'', "''"));
    if crate::db::is_excel_file(file) {
        return Relation {
            name: shown.clone(),
            kind: "workbook",
            from: String::new(),
            target: shown.clone(),
            path: Some(shown),
            rows: None,
            columns: Vec::new(),
            error: Some(
                "A workbook has no table function; open it in DuckLocal, or see its first \
                 sheet's columns with --stats"
                    .to_string(),
            ),
            sheets: crate::excel::sheets(file).ok(),
        };
    }
    let reader = crate::db::data_file_reader(file).unwrap_or("read_csv_auto");
    let absolute = format!("'{}'", file.replace('\'', "''"));
    let (columns, error) =
        match describe(connection, &format!("SELECT * FROM {reader}({absolute})")) {
            Ok(columns) => (columns, None),
            Err(message) => (Vec::new(), Some(message)),
        };
    let rows = (reader == "read_parquet")
        .then(|| {
            connection
                .query_row(
                    &format!("SELECT sum(num_rows)::BIGINT FROM parquet_file_metadata({absolute})"),
                    [],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .ok()
                .flatten()
        })
        .flatten();
    Relation {
        name: shown.clone(),
        kind: "file",
        // A quoted path is how DuckDB reads a file by its extension; the
        // explicit reader is only needed where the extension is ambiguous.
        from: if reader == "read_csv_auto" && !file.to_ascii_lowercase().ends_with(".csv") {
            format!("{reader}({literal})")
        } else {
            literal
        },
        target: shown.clone(),
        path: Some(shown),
        rows,
        columns,
        error,
        sheets: None,
    }
}

/// Every table and view the database holds, named as SQL on this connection
/// reaches it: bare in the default schema, `schema.table` elsewhere, and
/// `database.schema.table` outside the attached file.
fn database_relations(connection: &duckdb::Connection) -> Result<Vec<Relation>, CliError> {
    let current: String = connection
        .query_row("SELECT current_database()", [], |row| row.get(0))
        .map_err(|e| CliError::failure("database", e))?;
    let catalog = crate::schema::load_catalog_of(connection)
        .map_err(|e| CliError::failure("database", format!("{e:#}")))?;
    Ok(catalog
        .into_iter()
        .flat_map(|database| database.tables)
        .filter(|table| table.database != "temp")
        .map(|table| {
            let parts: Vec<&str> = if table.database != current {
                vec![&table.database, &table.schema, &table.name]
            } else if table.schema != "main" {
                vec![&table.schema, &table.name]
            } else {
                vec![&table.name]
            };
            Relation {
                name: parts.join("."),
                kind: match table.kind {
                    crate::schema::NodeKind::Table => "table",
                    crate::schema::NodeKind::View => "view",
                },
                from: parts
                    .iter()
                    .map(|part| quote(part))
                    .collect::<Vec<_>>()
                    .join("."),
                path: None,
                target: parts.join("."),
                rows: table.estimated_rows,
                columns: table
                    .columns
                    .into_iter()
                    .map(|column| (column.name, column.data_type))
                    .collect(),
                error: None,
                sheets: None,
            }
        })
        .collect())
}

/// An identifier as SQL: bare when it can be, double-quoted when not.
fn quote(name: &str) -> String {
    let mut chars = name.chars();
    let plain = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if plain {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

#[hotpath::measure]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn relation(name: &str, rows: Option<i64>) -> Relation {
        Relation {
            name: name.into(),
            kind: "table",
            from: name.into(),
            path: None,
            target: name.into(),
            rows,
            columns: Vec::new(),
            error: None,
            sheets: None,
        }
    }

    #[test]
    fn a_summary_keeps_the_largest_relations_whole() {
        let relations = [
            relation("small", Some(10)),
            relation("view", None),
            relation("big", Some(1_000)),
            relation("mid", Some(100)),
        ];
        assert_eq!(largest(&relations, 2), vec![false, false, true, true]);
        // Unknown counts come last, but still come when there is room.
        assert_eq!(largest(&relations, 4), vec![true; 4]);
    }

    #[test]
    fn identifiers_are_quoted_only_when_they_must_be() {
        assert_eq!(quote("orders"), "orders");
        assert_eq!(quote("Orders"), "\"Orders\"");
        assert_eq!(quote("order items"), "\"order items\"");
        assert_eq!(quote("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn table_names_match_qualified_bare_or_by_file_name() {
        let mut table = relation("sales.orders", None);
        assert!(names_match(&table, "sales.orders"));
        assert!(names_match(&table, "ORDERS"));
        table.kind = "file";
        table.name = "data/orders.csv".into();
        table.path = Some("data/orders.csv".into());
        assert!(names_match(&table, "orders.csv"));
        assert!(!names_match(&table, "orders"));
    }
}
