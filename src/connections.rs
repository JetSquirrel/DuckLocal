//! What the window's connection reaches beyond this machine, as `ducklocal open
//! --state` reports it: metadata a person and an agent can both read, and never
//! a key. Reading it makes no network request: `duckdb_databases()` is local,
//! where listing an attached server's tables is not (`schema::load_catalog`).

use duckdb::Connection;
use serde_json::{json, Value};

use crate::session::{self, Reads};

/// A database the window's connection has attached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Database {
    pub name: String,
    /// `duckdb`, or the extension type an attach named.
    pub kind: String,
    pub path: Option<String>,
    pub read_only: bool,
}

impl Database {
    /// Whether reading it leaves this machine. A kind this does not know is
    /// taken as local only when its path does not look like an address.
    fn is_remote(&self) -> bool {
        let kind = self.kind.to_ascii_lowercase();
        let path = self.path.as_deref().unwrap_or("");
        matches!(
            kind.as_str(),
            "quack" | "postgres" | "postgres_scanner" | "mysql" | "mysql_scanner" | "motherduck"
        ) || path.contains("://")
            || path.starts_with("quack:")
            || path.starts_with("md:")
    }

    fn describe(&self) -> Value {
        let remote = self.is_remote();
        json!({
            "name": self.name,
            "kind": self.kind,
            "location": if remote { "remote" } else { "local" },
            // A local path is what `database` already reports; an address may
            // carry a key.
            "address": self.path.as_deref().map(|path| {
                if remote { session::safe_address(path) } else { path.to_string() }
            }),
            "read_only": self.read_only,
        })
    }
}

/// The attached databases, without touching any of them.
pub(crate) fn databases_of(conn: &Connection) -> anyhow::Result<Vec<Database>> {
    let mut statement = conn.prepare(
        "SELECT database_name, type, path, readonly FROM duckdb_databases()
         WHERE NOT internal ORDER BY database_oid",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(Database {
            name: row.get(0)?,
            kind: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            path: row.get(2)?,
            read_only: row.get::<_, Option<bool>>(3)?.unwrap_or(false),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// `connections` in the window's state. `databases` is `None` while a query
/// holds the connection: asking must not make the window wait for it.
pub(crate) fn describe(s3: Option<&crate::s3::S3Config>, databases: Option<&[Database]>) -> Value {
    json!({
        "s3": s3.map(|config| json!({
            "endpoint": session::safe_address(&config.endpoint),
            "region": session::safe_address(&config.region),
        })),
        "databases": databases.map(|databases| {
            databases.iter().map(Database::describe).collect::<Vec<_>>()
        }),
    })
}

/// A file attached as a view: the name SQL reads it by, and where it is.
pub(crate) struct AttachedFile<'a> {
    pub name: &'a str,
    pub path: &'a str,
}

/// How far a statement's named sources reach: `local`, `remote`, or `unknown`
/// when the SQL cannot say. Read from the SQL alone, so a local view over
/// remote data reads as local (`docs/agent-handoff.md`).
pub(crate) fn reach(
    sql: &str,
    attached: &[AttachedFile],
    databases: Option<&[Database]>,
    s3_configured: bool,
) -> Value {
    let unknown = || json!({"reach": "unknown", "remote_sources": [], "uses_credentials": false});
    let Some(reads) = session::reads_of(sql) else {
        return unknown();
    };
    let Reads {
        files_and_tables,
        qualifiers,
        bare,
        other_function,
    } = reads;
    let is_remote_database = |name: &str| {
        databases
            .into_iter()
            .flatten()
            .any(|database| database.is_remote() && database.name.eq_ignore_ascii_case(name))
    };
    let attached_address = |name: &str| {
        attached
            .iter()
            .find(|file| file.name.eq_ignore_ascii_case(name) && file.path.contains("://"))
            .map(|file| file.path)
    };

    let mut remote: Vec<String> = Vec::new();
    let mut uses_credentials = false;
    let mut note_address = |address: &str| {
        uses_credentials |= s3_configured && address.starts_with("s3://");
        remote.push(session::safe_address(address));
    };
    for name in &files_and_tables {
        if name.contains("://") {
            note_address(name);
        }
    }
    for name in &bare {
        if let Some(address) = attached_address(name) {
            note_address(address);
        }
    }
    for qualifier in &qualifiers {
        if is_remote_database(qualifier) {
            remote.push(qualifier.clone());
        }
    }
    remote.sort();
    remote.dedup();
    if !remote.is_empty() {
        return json!({
            "reach": "remote",
            "remote_sources": remote,
            "uses_credentials": uses_credentials,
        });
    }

    // Nothing named is remote. That is only an answer if every name could be
    // told apart: a function may read anywhere, and with a remote database
    // attached a bare table may be in it after a `USE`.
    let any_remote_database = match databases {
        Some(databases) => databases.iter().any(Database::is_remote),
        None => true,
    };
    let unattached_bare = bare.iter().any(|name| {
        !attached
            .iter()
            .any(|file| file.name.eq_ignore_ascii_case(name))
            && !session::names_a_file(name, std::path::Path::new(""))
    });
    if other_function || (any_remote_database && unattached_bare) {
        return unknown();
    }
    json!({"reach": "local", "remote_sources": [], "uses_credentials": false})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database(name: &str, kind: &str, path: Option<&str>) -> Database {
        Database {
            name: name.into(),
            kind: kind.into(),
            path: path.map(str::to_string),
            read_only: true,
        }
    }

    fn reach_of(sql: &str, databases: &[Database]) -> Value {
        let attached = [
            AttachedFile {
                name: "orders",
                path: "/data/orders.csv",
            },
            AttachedFile {
                name: "events",
                path: "s3://logs/events.parquet",
            },
        ];
        reach(sql, &attached, Some(databases), true)
    }

    #[test]
    fn a_remote_database_is_told_from_a_local_one() {
        for remote in [
            database("a", "quack", Some("quack:localhost:9494")),
            database("b", "postgres", None),
            database("c", "duckdb", Some("s3://bucket/x.duckdb")),
            database("d", "something_new", Some("https://example.com/x")),
            database("e", "duckdb", Some("md:warehouse")),
        ] {
            assert!(remote.is_remote(), "{remote:?}");
        }
        for local in [
            database("a", "duckdb", Some("/data/x.duckdb")),
            database("b", "sqlite", Some("C:\\data\\x.sqlite")),
            database("c", "duckdb", None),
        ] {
            assert!(!local.is_remote(), "{local:?}");
        }
    }

    #[test]
    fn no_key_survives_into_the_description() {
        let config = crate::s3::S3Config {
            endpoint: "s3.example.com".into(),
            region: "eu-west-1".into(),
            key_id: "AKIDEXAMPLEVALUE".into(),
            secret: "wJalrXUtnFEMIEXAMPLEVALUE".into(),
        };
        let databases = [
            database(
                "a",
                "postgres",
                Some("postgres://me:hunter2@db.example.com/x"),
            ),
            database(
                "b",
                "duckdb",
                Some("https://example.com/x.duckdb?sig=hunter3"),
            ),
            database("c", "quack", Some("quack:host:9494?token=hunter4")),
        ];
        let text = describe(Some(&config), Some(&databases)).to_string();
        for leaked in [
            "AKIDEXAMPLEVALUE",
            "wJalrXUtnFEMI",
            "hunter2",
            "hunter3",
            "hunter4",
        ] {
            assert!(!text.contains(leaked), "{leaked}: {text}");
        }
        assert!(text.contains("s3.example.com"));
        assert!(text.contains("https://example.com/x.duckdb"));
        // Nor through a debug print of the configuration.
        let printed = format!("{config:?}");
        assert!(!printed.contains("wJalrXUtnFEMI"), "{printed}");
        assert!(!printed.contains("AKIDEXAMPLEVALUE"), "{printed}");
    }

    #[test]
    fn asking_while_a_query_runs_says_it_could_not_look() {
        let described = describe(None, None);
        assert_eq!(described["s3"], Value::Null);
        assert_eq!(described["databases"], Value::Null);
    }

    #[test]
    fn the_listing_reads_the_connections_own_databases() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("ATTACH ':memory:' AS side (READ_ONLY)")
            .ok();
        conn.execute_batch("ATTACH ':memory:' AS other").unwrap();
        let databases = databases_of(&conn).unwrap();
        let other = databases.iter().find(|d| d.name == "other").unwrap();
        assert_eq!(other.kind, "duckdb");
        assert!(!other.is_remote());
    }

    #[test]
    fn a_statement_reaches_what_it_names() {
        let quack = database("warehouse", "quack", Some("quack:host:9494"));
        let local = database("archive", "duckdb", Some("/data/a.duckdb"));
        let both = [local.clone(), quack];
        let only_local = [local];

        let far = reach_of("SELECT * FROM 's3://b/x.parquet?sig=abc'", &only_local);
        assert_eq!(far["reach"], "remote");
        assert_eq!(far["uses_credentials"], true);
        assert_eq!(far["remote_sources"], json!(["s3://b/x.parquet"]));

        let far = reach_of(
            "SELECT * FROM read_csv('https://example.com/x.csv')",
            &only_local,
        );
        assert_eq!(far["reach"], "remote");
        assert_eq!(far["uses_credentials"], false);

        let far = reach_of("SELECT * FROM warehouse.main.t", &both);
        assert_eq!(far["remote_sources"], json!(["warehouse"]));
        let far = reach_of("SELECT * FROM warehouse.t JOIN orders USING (id)", &both);
        assert_eq!(far["reach"], "remote");
        // An attached file that lives elsewhere is read through its view.
        let far = reach_of("SELECT * FROM events", &only_local);
        assert_eq!(far["reach"], "remote");
        assert_eq!(far["uses_credentials"], true);

        for sql in [
            "SELECT * FROM 'a.csv'",
            "SELECT * FROM read_parquet('/data/a.parquet')",
            "SELECT * FROM orders",
            "SELECT * FROM archive.main.t",
            "SELECT * FROM range(3)",
            "WITH c AS (SELECT 1) SELECT * FROM c",
            "SELECT 1",
        ] {
            assert_eq!(reach_of(sql, &only_local)["reach"], "local", "{sql}");
        }
        assert_eq!(reach_of("SELECT * FROM orders", &both)["reach"], "local");

        for (sql, databases) in [
            // After a USE it could be in the remote database.
            ("SELECT * FROM t", &both[..]),
            // Could read anywhere.
            ("SELECT * FROM glob('s3://b/*')", &only_local[..]),
            // Not a SELECT: nothing can be read off it.
            ("COPY t TO 'x.csv'", &only_local[..]),
            ("not sql at all", &only_local[..]),
        ] {
            assert_eq!(reach_of(sql, databases)["reach"], "unknown", "{sql}");
        }
        // Without the list, a bare name might be anywhere.
        let attached: [AttachedFile; 0] = [];
        assert_eq!(
            reach("SELECT * FROM t", &attached, None, false)["reach"],
            "unknown"
        );
    }
}
