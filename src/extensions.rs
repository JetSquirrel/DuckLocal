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
//! DuckDB keeps a `LOAD` for one connection only, and loads on its own only
//! what it compiles in or knows from its core repository. Anything else (a
//! community extension, say) is remembered once loaded, and loaded again on
//! every connection the window opens: see [`load_at_launch_of`].
//!
//! Blocking functions; call via `smol::unblock` from UI code.

use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};

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

/// NVIDIA's GPU engine for DuckDB, a core extension since v2.0.1. It is
/// built for Linux (glibc, x86-64 and arm64) only, its download is about
/// 700 MB, and, since it takes over every query rather than adding
/// functions, no query autoloads it.
pub const SIRIUS: &str = "sirius";

/// Whether DuckDB publishes a build of `name` for this platform, as far as
/// this build knows: only Sirius is known to have none outside Linux.
pub fn available_here(name: &str) -> bool {
    name != SIRIUS || cfg!(target_os = "linux")
}

/// Whether installing `name` is a download large enough to say so first.
pub fn is_large_download(name: &str) -> bool {
    name == SIRIUS
}

/// DuckDB's HTTP timeout while a large extension downloads, in seconds: the
/// default 30 cuts off Sirius's 700 MB on most connections. Sirius's own
/// install instructions use 900.
const LARGE_DOWNLOAD_TIMEOUT_SECS: u32 = 900;

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
    list.retain(|e| e.installed || available_here(&e.name));
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
    if action == ExtensionAction::Install && !available_here(name) {
        return Err(anyhow!(
            "DuckDB publishes no {name} for this platform: it runs on Linux only"
        ));
    }
    let engine_knows = engine_list_of(conn)?.iter().any(|e| e.name == name);
    let curated = COMMUNITY.iter().any(|(n, _)| *n == name);
    if !is_extension_name(name) || !(engine_knows || curated) {
        return Err(anyhow!("Unknown extension: {name}"));
    }
    match action {
        ExtensionAction::Install if engine_knows && is_large_download(name) => {
            install_large_of(conn, name)?
        }
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

/// Install a core extension whose download outlasts DuckDB's default HTTP
/// timeout. The timeout is raised through httpfs, as Sirius's instructions
/// do, and put back afterwards whatever the install did, so that later S3
/// and HTTP reads keep the usual one.
fn install_large_of(conn: &Connection, name: &str) -> Result<()> {
    conn.execute_batch(&format!(
        "INSTALL httpfs; LOAD httpfs; SET http_timeout = {LARGE_DOWNLOAD_TIMEOUT_SECS}"
    ))?;
    let installed = conn.execute_batch(&format!("INSTALL {name}"));
    let _ = conn.execute_batch("RESET http_timeout");
    Ok(installed?)
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
    if name == SIRIUS {
        // A core extension: the community repository has only its backends.
        return apply_of(conn, name, ExtensionAction::Install);
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

/// The `settings` key holding the extensions loaded on every connection the
/// window opens, as a JSON array of names.
pub const AT_LAUNCH_SETTING: &str = "extensions_at_launch";

/// Whether `ext` is one DuckDB leaves for someone to load in every session.
/// A builtin is always loaded and a core extension autoloads when a query
/// needs it; one from the community repository, another repository or a
/// file does neither.
///
/// Sirius is the exception among core extensions: no query names it, so
/// none autoloads it, and without a `LOAD` every query runs on the CPU.
pub fn needs_loading(ext: &ExtensionInfo) -> bool {
    ext.installed
        && matches!(ext.origin, Origin::Repository | Origin::Other)
        && (ext.name == SIRIUS
            || !ext
                .installed_from
                .as_deref()
                .is_some_and(|from| from.starts_with("core")))
}

/// The extensions on the launch list: loaded on every new connection.
pub fn at_launch() -> BTreeSet<String> {
    crate::history::get_setting(AT_LAUNCH_SETTING)
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
        .unwrap_or_default()
        .into_iter()
        // Each name reaches SQL as an identifier.
        .filter(|name| is_extension_name(name))
        .collect()
}

fn save_at_launch(names: &BTreeSet<String>) -> Result<()> {
    crate::history::set_setting(AT_LAUNCH_SETTING, &serde_json::to_string(names)?)
}

/// Put `name` on the launch list, or take it off.
pub fn set_at_launch(name: &str, on: bool) -> Result<()> {
    if !is_extension_name(name) {
        return Err(anyhow!("Not an extension name: {name}"));
    }
    let mut names = at_launch();
    let changed = if on {
        names.insert(name.to_string())
    } else {
        names.remove(name)
    };
    if changed {
        save_at_launch(&names)?;
    }
    Ok(())
}

/// The extensions [`needs_loading`] picks that the current connection had
/// loaded when last looked at. One loaded since is new and joins the launch
/// list; one taken off the list stays off while it is still loaded, since
/// DuckDB cannot unload it.
static SEEN: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn loaded_by_hand_of(conn: &Connection) -> Result<BTreeSet<String>> {
    Ok(engine_list_of(conn)?
        .into_iter()
        .filter(|ext| ext.loaded && needs_loading(ext))
        .map(|ext| ext.name)
        .collect())
}

/// Load everything on the launch list into `conn`, a connection just opened.
/// Returns a line for each extension that would not load (removed, or built
/// for another DuckDB), which stays on the list so that it loads again once
/// reinstalled. Nothing is downloaded: [`crate::db::config`] turns
/// autoinstall off, and `LOAD` never installs.
pub fn load_at_launch_of(conn: &Connection) -> Vec<String> {
    let mut problems = Vec::new();
    for name in at_launch() {
        if let Err(e) = conn.execute_batch(&format!("LOAD {name}")) {
            problems.push(crate::i18n::trf(
                "notify.extensions.not_loaded",
                &[&name, &e.to_string()],
            ));
        }
    }
    *SEEN.lock().unwrap_or_else(PoisonError::into_inner) =
        loaded_by_hand_of(conn).unwrap_or_default();
    problems
}

/// Put each extension loaded since the last look on the launch list: after
/// the sidebar's Load, and after SQL that [`may_load`].
pub fn remember_loaded_of(conn: &Connection) -> Result<()> {
    let now = loaded_by_hand_of(conn)?;
    let new: Vec<String> = {
        let mut seen = SEEN.lock().unwrap_or_else(PoisonError::into_inner);
        let new = now.difference(&seen).cloned().collect();
        *seen = now;
        new
    };
    if !new.is_empty() {
        let mut names = at_launch();
        names.extend(new);
        save_at_launch(&names)?;
    }
    Ok(())
}

/// Whether `sql` may have loaded an extension, so that
/// [`remember_loaded_of`] is worth running after it.
pub fn may_load(sql: &str) -> bool {
    sql.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|word| word.eq_ignore_ascii_case("load"))
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

    fn ext(origin: Origin, installed_from: Option<&str>) -> ExtensionInfo {
        ExtensionInfo {
            name: "x".to_string(),
            loaded: true,
            installed: origin != Origin::NotInstalled,
            origin,
            version: None,
            installed_from: installed_from.map(str::to_string),
            description: None,
            community: installed_from == Some(COMMUNITY_REPOSITORY),
        }
    }

    #[test]
    fn only_what_duckdb_will_not_load_itself_needs_loading() {
        assert!(needs_loading(&ext(Origin::Repository, Some("community"))));
        assert!(needs_loading(&ext(
            Origin::Repository,
            Some("https://example.com/ext")
        )));
        // Installed from a file.
        assert!(needs_loading(&ext(Origin::Other, None)));

        assert!(!needs_loading(&ext(Origin::Builtin, None)));
        // Core extensions autoload when a query needs them.
        assert!(!needs_loading(&ext(Origin::Repository, Some("core"))));
        assert!(!needs_loading(&ext(
            Origin::Repository,
            Some("core_nightly")
        )));
        assert!(!needs_loading(&ext(Origin::NotInstalled, None)));
    }

    #[test]
    fn sirius_is_the_core_extension_that_needs_loading() {
        let sirius = ExtensionInfo {
            name: SIRIUS.to_string(),
            ..ext(Origin::Repository, Some("core"))
        };
        assert!(needs_loading(&sirius));
        assert!(!needs_loading(&ExtensionInfo {
            origin: Origin::NotInstalled,
            installed: false,
            ..sirius
        }));
    }

    #[test]
    fn sirius_is_offered_on_linux_only() {
        assert_eq!(available_here(SIRIUS), cfg!(target_os = "linux"));
        assert!(available_here("httpfs"));
        let listed = list_of(&mem()).unwrap().iter().any(|e| e.name == SIRIUS);
        if !cfg!(target_os = "linux") {
            assert!(!listed);
            let refused = apply_of(&mem(), SIRIUS, ExtensionAction::Install);
            assert!(refused.unwrap_err().to_string().contains("Linux only"));
        }
    }

    #[test]
    fn the_launch_list_keeps_only_extension_names() {
        let _guard = crate::db::connection_guard();
        crate::history::with_test_history(|| {
            assert!(at_launch().is_empty());
            crate::history::set_setting(
                AT_LAUNCH_SETTING,
                r#"["mssql", "x; DROP TABLE t", "MSSQL", "gsheets"]"#,
            )
            .unwrap();
            assert_eq!(
                at_launch().into_iter().collect::<Vec<_>>(),
                ["gsheets", "mssql"]
            );

            set_at_launch("mssql", false).unwrap();
            set_at_launch("h3", true).unwrap();
            assert_eq!(
                at_launch().into_iter().collect::<Vec<_>>(),
                ["gsheets", "h3"]
            );
            assert!(set_at_launch("a b", true).is_err());

            // A value this build cannot read is no list, not a failure.
            crate::history::set_setting(AT_LAUNCH_SETTING, "not json").unwrap();
            assert!(at_launch().is_empty());
        });
    }

    #[test]
    fn an_extension_that_will_not_load_is_named_and_kept_on_the_list() {
        let _guard = crate::db::connection_guard();
        crate::history::with_test_history(|| {
            set_at_launch("ducklocal_no_such_extension", true).unwrap();
            // As the window opens one: autoinstall off, so nothing downloads.
            let conn = Connection::open_in_memory_with_flags(crate::db::config().unwrap()).unwrap();
            let problems = load_at_launch_of(&conn);
            assert_eq!(problems.len(), 1, "{problems:?}");
            assert!(
                problems[0].contains("ducklocal_no_such_extension"),
                "{problems:?}"
            );
            assert!(at_launch().contains("ducklocal_no_such_extension"));
        });
    }

    #[test]
    fn builtin_and_core_extensions_never_join_the_launch_list() {
        let _guard = crate::db::connection_guard();
        crate::history::with_test_history(|| {
            let conn = mem();
            load_at_launch_of(&conn);
            apply_of(&conn, "parquet", ExtensionAction::Load).unwrap();
            // ICU is from the core repository where it is installed at all.
            let _ = conn.execute_batch("LOAD icu");
            remember_loaded_of(&conn).unwrap();
            assert!(at_launch().is_empty());
        });
    }

    #[test]
    fn only_sql_that_says_load_is_looked_at_again() {
        for sql in [
            "LOAD mssql",
            "INSTALL mssql FROM community;\nload mssql;",
            "SELECT 1; LOAD 'x.duckdb_extension'",
        ] {
            assert!(may_load(sql), "{sql}");
        }
        for sql in [
            "SELECT * FROM loads",
            "SELECT download FROM t",
            "INSTALL mssql FROM community",
        ] {
            assert!(!may_load(sql), "{sql}");
        }
    }
}
