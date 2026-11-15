//! DuckDB extensions: what the engine knows of, what is installed and loaded,
//! and the three statements that change that — `INSTALL`, `LOAD` and
//! `UPDATE EXTENSIONS`.
//!
//! Everything is read from `duckdb_extensions()` rather than kept here: the
//! engine is the only one that knows what a `LOAD` in a query tab, or an
//! autoload behind `read_parquet`, did a moment ago. The one exception is
//! [`COMMUNITY`]: the engine lists a community extension only once it is
//! installed, so a few that people ask for are named here until then. Any
//! other community extension installs by name ([`install_community_of`]).
//!
//! Blocking functions; call via `smol::unblock` from UI code.

use anyhow::{anyhow, Result};
use duckdb::Connection;

/// How an extension reached this build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Compiled into the binary: always there, never installed or updated.
    Builtin,
    /// Downloaded from a repository (`core`, `community`, a custom URL).
    Repository,
    /// Installed from a local file or by some other route DuckDB reports.
    Other,
    NotInstalled,
}

#[derive(Clone, Debug)]
pub struct ExtensionInfo {
    pub name: String,
    pub loaded: bool,
    pub installed: bool,
    pub origin: Origin,
    /// A release tag (`v1.4.3`) or a commit hash for repository builds.
    pub version: Option<String>,
    /// `core`, `community`, or a repository URL.
    pub installed_from: Option<String>,
    pub description: Option<String>,
    /// From DuckDB's community repository: third-party code that DuckDB
    /// signs but does not review.
    pub community: bool,
}

/// Community extensions listed before they are installed, with what they do.
/// Keep it short: anything else installs by name from the same page.
pub const COMMUNITY: &[(&str, &str)] = &[
    (
        "mssql",
        "Attach Microsoft SQL Server and Azure SQL databases over TDS",
    ),
    ("gsheets", "Read and write Google Sheets"),
    ("bigquery", "Attach Google BigQuery projects and query them"),
    (
        "read_stat",
        "Read SAS (.sas7bdat, .xpt), SPSS (.sav) and Stata (.dta) files",
    ),
    ("h3", "Uber's H3 hexagonal geospatial index"),
    ("crypto", "Cryptographic hash functions and HMAC"),
];

/// The community repository's name in `INSTALL ... FROM`.
const COMMUNITY_REPOSITORY: &str = "community";

impl ExtensionInfo {
    pub fn can_install(&self) -> bool {
        !self.installed
    }

    pub fn can_load(&self) -> bool {
        self.installed && !self.loaded
    }

    /// Only repository installs have a newer build to fetch; a builtin is
    /// as new as the binary.
    pub fn can_update(&self) -> bool {
        self.installed && self.origin == Origin::Repository
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionAction {
    Install,
    Load,
    Update,
}

/// Every extension the engine knows of: loaded first, then installed, then
/// the rest, each group by name.
pub fn list_of(conn: &Connection) -> Result<Vec<ExtensionInfo>> {
    let mut list = engine_list_of(conn)?;
    for (name, description) in COMMUNITY {
        if !list.iter().any(|e| e.name == *name) {
            list.push(ExtensionInfo {
                name: (*name).to_string(),
                loaded: false,
                installed: false,
                origin: Origin::NotInstalled,
                version: None,
                installed_from: None,
                description: Some((*description).to_string()),
                community: true,
            });
        }
    }
    list.sort_by(|a, b| {
        (b.loaded, b.installed)
            .cmp(&(a.loaded, a.installed))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(list)
}

/// What `duckdb_extensions()` lists, and nothing else.
fn engine_list_of(conn: &Connection) -> Result<Vec<ExtensionInfo>> {
    let mut stmt = conn.prepare(
        "SELECT extension_name, loaded, installed, install_mode, extension_version, \
                installed_from, description \
         FROM duckdb_extensions() \
         ORDER BY loaded DESC, installed DESC, extension_name",
    )?;
    let rows = stmt.query_map([], |row| {
        let non_empty = |s: Option<String>| s.filter(|s| !s.trim().is_empty());
        let mode: Option<String> = row.get(3)?;
        let installed: bool = row.get::<_, Option<bool>>(2)?.unwrap_or(false);
        let origin = match mode.as_deref() {
            Some("STATICALLY_LINKED") => Origin::Builtin,
            Some("REPOSITORY") => Origin::Repository,
            _ if installed => Origin::Other,
            _ => Origin::NotInstalled,
        };
        let installed_from: Option<String> = non_empty(row.get(5)?);
        Ok(ExtensionInfo {
            community: installed_from.as_deref() == Some(COMMUNITY_REPOSITORY),
            name: row.get(0)?,
            loaded: row.get::<_, Option<bool>>(1)?.unwrap_or(false),
            installed,
            origin,
            version: non_empty(row.get(4)?),
            installed_from,
            description: non_empty(row.get(6)?),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Install, load or update one extension by name.
///
/// The name is checked against what [`list_of`] lists rather than
/// interpolated as given: these statements take an identifier, not a
/// parameter. A name the engine does not know installs from the community
/// repository.
pub fn apply_of(conn: &Connection, name: &str, action: ExtensionAction) -> Result<()> {
    let engine_knows = engine_list_of(conn)?.iter().any(|e| e.name == name);
    let curated = COMMUNITY.iter().any(|(n, _)| *n == name);
    if !is_extension_name(name) || !(engine_knows || curated) {
        return Err(anyhow!("Unknown extension: {name}"));
    }
    match action {
        ExtensionAction::Install if engine_knows => {
            conn.execute_batch(&format!("INSTALL {name}"))?
        }
        ExtensionAction::Install => return install_community_of(conn, name),
        ExtensionAction::Load => conn.execute_batch(&format!("LOAD {name}"))?,
        // Returns a row per extension with what changed; the list re-read
        // afterwards is what shows it.
        ExtensionAction::Update => {
            let mut stmt = conn.prepare(&format!("UPDATE EXTENSIONS ({name})"))?;
            let mut rows = stmt.query([])?;
            while rows.next()?.is_some() {}
        }
    }
    Ok(())
}

/// Install `name` from DuckDB's community repository, for one a person typed.
///
/// A name with no build for this DuckDB is the usual failure, since
/// community builds trail new releases; it is reported as that rather than
/// as DuckDB's HTTP error.
pub fn install_community_of(conn: &Connection, name: &str) -> Result<()> {
    if !is_extension_name(name) {
        return Err(anyhow!("Not an extension name: {name}"));
    }
    match conn.execute_batch(&format!("INSTALL {name} FROM {COMMUNITY_REPOSITORY}")) {
        Ok(()) => Ok(()),
        Err(e) if e.to_string().contains("404") => {
            let version: String = conn
                .query_row("SELECT library_version FROM pragma_version()", [], |r| {
                    r.get(0)
                })
                .unwrap_or_else(|_| "this version".to_string());
            Err(anyhow!(
                "The community repository has no {name} for DuckDB {version}: \
                 the name may be wrong, or it is not built for this version yet"
            ))
        }
        Err(e) => Err(e.into()),
    }
}

/// What DuckDB names extensions with: lowercase letters, digits and `_`.
/// Anything else is refused before it reaches SQL.
pub fn is_extension_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn bundled_extensions_are_builtin_and_loaded() {
        let list = list_of(&mem()).unwrap();
        // The build bundles parquet and json (Cargo.toml features).
        for name in ["parquet", "json"] {
            let ext = list.iter().find(|e| e.name == name).unwrap();
            assert_eq!(ext.origin, Origin::Builtin, "{name}");
            assert!(ext.loaded && ext.installed, "{name}");
            assert!(!ext.can_install() && !ext.can_load() && !ext.can_update());
        }
        // Loaded ones sort first.
        let first_unloaded = list.iter().position(|e| !e.loaded).unwrap_or(list.len());
        assert!(list[first_unloaded..].iter().all(|e| !e.loaded));
    }

    #[test]
    fn loading_a_builtin_is_harmless_and_unknown_names_are_refused() {
        let conn = mem();
        apply_of(&conn, "parquet", ExtensionAction::Load).unwrap();
        for name in ["no_such_extension", "parquet; DROP TABLE x", ""] {
            assert!(
                apply_of(&conn, name, ExtensionAction::Install).is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn curated_community_extensions_are_listed_until_installed() {
        let list = list_of(&mem()).unwrap();
        for (name, _) in COMMUNITY {
            assert!(is_extension_name(name), "{name}");
            let ext = list.iter().find(|e| e.name == *name).unwrap();
            assert!(ext.community && ext.can_install(), "{name}");
            assert!(ext.description.is_some(), "{name}");
        }
        // No duplicates, and the merged list keeps the order.
        let mut names: Vec<_> = list.iter().map(|e| e.name.as_str()).collect();
        names.dedup();
        assert_eq!(names.len(), list.len());
        let first_unloaded = list.iter().position(|e| !e.loaded).unwrap_or(list.len());
        assert!(list[first_unloaded..].iter().all(|e| !e.loaded));
    }

    #[test]
    fn typed_names_are_checked_before_any_sql() {
        for name in ["", "MSSQL", "a b", "x;LOAD y", "../x", "é", &"a".repeat(65)] {
            assert!(!is_extension_name(name), "{name:?}");
            assert!(install_community_of(&mem(), name).is_err(), "{name:?}");
        }
        assert!(is_extension_name("read_stat"));
        // A community extension that is not installed cannot be loaded.
        assert!(apply_of(&mem(), "mssql", ExtensionAction::Load).is_err());
    }
}
