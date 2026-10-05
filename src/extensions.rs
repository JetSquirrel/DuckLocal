//! DuckDB extensions: what the engine knows of, what is installed and loaded,
//! and the three statements that change that — `INSTALL`, `LOAD` and
//! `UPDATE EXTENSIONS`.
//!
//! Everything is read from `duckdb_extensions()` rather than kept here: the
//! engine is the only one that knows what a `LOAD` in a query tab, or an
//! autoload behind `read_parquet`, did a moment ago.
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
}

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
        Ok(ExtensionInfo {
            name: row.get(0)?,
            loaded: row.get::<_, Option<bool>>(1)?.unwrap_or(false),
            installed,
            origin,
            version: non_empty(row.get(4)?),
            installed_from: non_empty(row.get(5)?),
            description: non_empty(row.get(6)?),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Install, load or update one extension by name.
///
/// The name is checked against what `duckdb_extensions()` lists rather than
/// interpolated as given: these statements take an identifier, not a
/// parameter.
pub fn apply_of(conn: &Connection, name: &str, action: ExtensionAction) -> Result<()> {
    let known = list_of(conn)?.into_iter().any(|e| e.name == name);
    if !known || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(anyhow!("Unknown extension: {name}"));
    }
    match action {
        ExtensionAction::Install => conn.execute_batch(&format!("INSTALL {name}"))?,
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
}
