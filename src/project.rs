//! A portable analysis: a folder with a small manifest saying which dashboards
//! it is and what they need, so it can be checked and reopened somewhere else.
//!
//! The manifest is optional and adds to what is already there. A `.dash` file
//! still says where its own data is, relative to itself, and `ducklocal check`
//! still judges it; `project check` runs that over each dashboard and adds what
//! a move breaks: files that did not come along, paths that were never going
//! to, extensions this machine lacks. Data is referenced, never copied, and
//! nothing here installs an extension or reaches a remote source.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::cli::{parse_args, Arg, CliError, FlagSpec};
use crate::spec::{model, proposal, source};

/// The manifest's name, in the project's folder.
pub const MANIFEST: &str = "ducklocal-project.json";
const VERSION: u32 = 1;
/// How deep `init` looks for dashboards.
const MAX_DEPTH: usize = 6;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    #[serde(default)]
    pub name: String,
    /// The dashboards, relative to the manifest, with forward slashes.
    #[serde(default)]
    pub dashboards: Vec<String>,
    /// Data the dashboards' own `source` blocks do not name: read by path in
    /// their SQL, or kept here for reference.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data: Vec<DataRef>,
    #[serde(default, skip_serializing_if = "Requires::is_empty")]
    pub requires: Requires,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataRef {
    pub name: String,
    pub path: String,
    /// A small stand-in that travels with the project, for when `path` is not
    /// to hand; said in the report, and never swapped in without asking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requires {
    /// The DuckDB version the work was done on, such as `v2.0.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duckdb: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
}

impl Requires {
    fn is_empty(&self) -> bool {
        self.duckdb.is_none() && self.extensions.is_empty()
    }
}

pub fn is_manifest(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == MANIFEST) && path.is_file()
}

pub fn load(path: &Path) -> Result<Manifest, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let manifest: Manifest =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if manifest.version != VERSION {
        return Err(format!(
            "{}: version {} is not one this DuckLocal reads",
            path.display(),
            manifest.version
        ));
    }
    Ok(manifest)
}

/// Whether a relative path stays inside the folder it is relative to.
fn stays_inside(path: &str) -> bool {
    let path = Path::new(path);
    let mut depth = 0i32;
    for part in path.components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    // A Windows path is plain text to Unix, and the reverse.
    !path.to_string_lossy().contains('\\') && !looks_absolute(&path.to_string_lossy())
}

fn looks_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || path.starts_with('\\')
        || path.starts_with('~')
        || (bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

fn is_remote(path: &str) -> bool {
    path.contains("://")
}

fn is_glob(path: &str) -> bool {
    path.contains(['*', '?', '['])
}

/// The dashboards a manifest may open: those inside its own folder. One that
/// points elsewhere would run SQL from somewhere nobody looked, on the strength
/// of a file passed along with the project.
pub fn dashboards_of(manifest_path: &Path) -> Result<Vec<PathBuf>, String> {
    let manifest = load(manifest_path)?;
    let root = source::base_of(manifest_path);
    Ok(manifest
        .dashboards
        .iter()
        .filter(|dashboard| stays_inside(dashboard))
        .map(|dashboard| root.join(dashboard))
        .collect())
}

/// One thing wrong, and what to do about it. Names paths and settings, never a
/// value that could be a secret.
fn problem(kind: &str, what: String, hint: String) -> Value {
    json!({"kind": kind, "what": what, "hint": hint})
}

/// Absolute or remote paths written straight into a query's SQL: a move cannot
/// carry these, and they do not show as sources.
fn paths_in_sql(sql: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = sql;
    while let Some(start) = rest.find('\'') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('\'') else { break };
        let literal = &after[..end];
        if looks_absolute(literal) || is_remote(literal) {
            found.push(literal.to_string());
        }
        rest = &after[end + 1..];
    }
    found
}

/// Whether anything exists at `path`, resolving a glob.
fn exists(path: &Path) -> bool {
    let text = path.to_string_lossy();
    if !is_glob(&text) {
        return path.exists();
    }
    crate::cli::open(None, false)
        .ok()
        .and_then(|conn| {
            conn.query_row(
                "SELECT count(*) FROM glob(?)",
                [text.replace('\\', "/")],
                |row| row.get::<_, i64>(0),
            )
            .ok()
        })
        .is_some_and(|count| count > 0)
}

/// How a data path stands, and any problem with it. `base` is what it is
/// relative to, `root` the project's folder.
fn data_status(path: &str, base: &Path, root: &Path, problems: &mut Vec<Value>) -> &'static str {
    if is_remote(path) {
        return "remote";
    }
    let resolved = source::resolve(path, base);
    if !exists(&resolved) {
        return "missing";
    }
    if looks_absolute(path) {
        problems.push(problem(
            "not_portable",
            format!("{path} is an absolute path, so it will not move with the project"),
            "Move it into the project and use a relative path, or run `ducklocal project \
             relink` on each machine"
                .to_string(),
        ));
        return "absolute";
    }
    let inside = match (
        std::fs::canonicalize(&resolved),
        std::fs::canonicalize(root),
    ) {
        (Ok(file), Ok(root)) => file.starts_with(root),
        // A glob has no one file to ask about.
        _ => stays_inside(path),
    };
    if !inside {
        problems.push(problem(
            "not_portable",
            format!("{path} is outside the project folder, so it will not move with it"),
            "Move it into the project folder if it should travel".to_string(),
        ));
        return "outside";
    }
    "ok"
}

fn check_dashboard(
    relative: &str,
    root: &Path,
    problems: &mut Vec<Value>,
    limitations: &mut Vec<String>,
) -> Value {
    let path = root.join(relative);
    if !stays_inside(relative) {
        problems.push(problem(
            "not_portable",
            format!("Dashboard {relative} is outside the project folder, so it was not checked"),
            "List dashboards relative to the manifest, inside its folder, with forward slashes"
                .to_string(),
        ));
        return json!({"path": relative, "status": "outside", "checked": false});
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            problems.push(problem(
                "missing_dashboard",
                format!("Dashboard {relative}: {error}"),
                "Restore the file, or take it out of the manifest's dashboards".to_string(),
            ));
            return json!({"path": relative, "status": "missing", "checked": false});
        }
    };
    let base = source::base_of(&path);
    let mut sources = Vec::new();
    let mut remote = false;
    if let Ok(parsed) = model::parse(&text) {
        let (spec, _) = model::validate_partial(&parsed);
        for named in &spec.sources {
            let status = data_status(&named.path, &base, root, problems);
            remote |= status == "remote";
            if status == "missing" {
                problems.push(problem(
                    "missing_source",
                    format!(
                        "Source {:?} in {relative}: nothing at {}",
                        named.name, named.path
                    ),
                    format!(
                        "Bring the file back, or point at where it is now: ducklocal project \
                         relink --source {} --to PATH",
                        named.name
                    ),
                ));
            }
            sources.push(json!({"name": named.name, "path": named.path, "status": status}));
        }
        // A view's SQL is part of every query that names it.
        let written = spec
            .queries
            .iter()
            .map(|query| ("Query", &query.name, &query.sql))
            .chain(
                spec.views
                    .iter()
                    .map(|view| ("View", &view.name, &view.sql)),
            );
        for (kind, name, sql) in written {
            for literal in paths_in_sql(sql) {
                if is_remote(&literal) {
                    remote = true;
                } else {
                    problems.push(problem(
                        "not_portable",
                        format!("{kind} {name:?} in {relative} names {literal} directly"),
                        "Name it in a source block with a relative path, so it moves with the \
                         project"
                            .to_string(),
                    ));
                }
            }
        }
    }
    // Checking runs the queries, and this must not reach out on its own.
    if remote {
        limitations.push(format!(
            "{relative} reads remote data, so its queries were not run; it depends on that \
             address and any credentials still being good"
        ));
        return json!({
            "path": relative,
            "status": "remote",
            "checked": false,
            "sources": sources,
        });
    }
    let verdict = proposal::check(&path, &text);
    let status = if verdict.passed() { "ok" } else { "failed" };
    if let Some(failure) = &verdict.failure {
        // A missing file is already reported, with its way out.
        if !sources.iter().any(|source| source["status"] == "missing") {
            problems.push(problem(
                "failed_check",
                format!("{relative}: {failure}"),
                format!("See `ducklocal check {relative}`"),
            ));
        }
    }
    if !verdict.unresolved.is_empty() {
        limitations.push(format!(
            "{relative}: {} read tables not available here, so went unchecked",
            verdict.unresolved.join(", ")
        ));
    }
    if !verdict.truncated.is_empty() {
        limitations.push(format!(
            "{relative}: {} hit the row budget, so what they show is not all of it",
            verdict.truncated.join(", ")
        ));
    }
    json!({
        "path": relative,
        "status": status,
        "checked": true,
        "failure": verdict.failure,
        "unresolved": verdict.unresolved,
        "empty": verdict.empty,
        "truncated": verdict.truncated,
        "sources": sources,
    })
}

/// DuckDB's version, and how each extension stands on this machine.
fn engine() -> (Option<String>, Vec<(String, bool, bool)>) {
    let Ok(conn) = crate::cli::open(None, false) else {
        return (None, Vec::new());
    };
    let version = conn
        .query_row("SELECT library_version FROM pragma_version()", [], |row| {
            row.get(0)
        })
        .ok();
    let extensions = conn
        .prepare("SELECT extension_name, installed, loaded FROM duckdb_extensions()")
        .and_then(|mut statement| {
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect()
        })
        .unwrap_or_default();
    (version, extensions)
}

fn check_project(manifest_path: &Path) -> Result<Value, CliError> {
    let manifest = load(manifest_path).map_err(|e| CliError::failure("project", e))?;
    let root = source::base_of(manifest_path);
    let root = if root.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        root
    };
    let mut problems = Vec::new();
    let mut limitations = vec![
        "Sources are checked for being there, not for holding the same data: a changed file \
         gives different results with no warning here"
            .to_string(),
    ];
    if manifest.dashboards.is_empty() {
        problems.push(problem(
            "no_dashboards",
            "The manifest lists no dashboards".to_string(),
            format!("Add their paths to dashboards in {MANIFEST}"),
        ));
    }
    let dashboards: Vec<Value> = manifest
        .dashboards
        .iter()
        .map(|dashboard| check_dashboard(dashboard, &root, &mut problems, &mut limitations))
        .collect();

    let data: Vec<Value> = manifest
        .data
        .iter()
        .map(|entry| {
            let status = data_status(&entry.path, &root, &root, &mut problems);
            let sample = entry.sample.as_ref().map(|sample| {
                json!({
                    "path": sample,
                    "there": source::resolve(sample, &root).exists(),
                })
            });
            if status == "missing" {
                let hint = match &entry.sample {
                    Some(sample) => format!(
                        "Bring it back, or relink it: ducklocal project relink --data {} --to \
                         PATH. A sample is at {sample}",
                        entry.name
                    ),
                    None => format!(
                        "Bring it back, or relink it: ducklocal project relink --data {} --to PATH",
                        entry.name
                    ),
                };
                problems.push(problem(
                    "missing_data",
                    format!("Data {:?}: nothing at {}", entry.name, entry.path),
                    hint,
                ));
            }
            if status == "remote" {
                limitations.push(format!(
                    "Data {:?} is remote and was not reached; it may need credentials this \
                     machine does not have",
                    entry.name
                ));
            }
            json!({"name": entry.name, "path": entry.path, "status": status, "sample": sample})
        })
        .collect();

    let (version, installed) = engine();
    if let Some(wanted) = &manifest.requires.duckdb {
        if version.as_deref() != Some(wanted.as_str()) {
            limitations.push(format!(
                "This work was done on DuckDB {wanted}, and this is {}: results and SQL \
                 behaviour can differ",
                version.as_deref().unwrap_or("an unknown version")
            ));
        }
    }
    let extensions: Vec<Value> = manifest
        .requires
        .extensions
        .iter()
        .map(|name| {
            let known = installed.iter().find(|(known, _, _)| known == name);
            let status = match known {
                Some((_, _, true)) => "loaded",
                Some((_, true, _)) => "installed",
                _ => "missing",
            };
            if status == "missing" {
                problems.push(problem(
                    "missing_extension",
                    format!("Extension {name} is not installed"),
                    "Nothing installs it on its own. If you want it, use the Extensions tab, \
                         which downloads it from DuckDB's server, then check again"
                        .to_string(),
                ));
            }
            json!({"name": name, "status": status})
        })
        .collect();
    let ok = problems.iter().all(|p| p["kind"] == "not_portable")
        && dashboards
            .iter()
            .all(|dashboard| dashboard["status"] != "failed");
    Ok(json!({
        "project": manifest.name,
        "manifest": manifest_path.to_string_lossy(),
        "ok": ok,
        "portable": problems.iter().all(|p| p["kind"] != "not_portable"),
        "ducklocal": env!("CARGO_PKG_VERSION"),
        "duckdb": version,
        "dashboards": dashboards,
        "data": data,
        "extensions": extensions,
        "problems": problems,
        "limitations": limitations,
    }))
}

/// The folder a project keeps drafts for `ducklocal propose --from` in. What
/// is in it is a change someone offered to a dashboard, not a dashboard: once
/// applied it is a second copy, and until then a report nobody approved.
const DRAFTS: &str = "proposals";

/// The spec files under `dir`, as paths from `root`: `found` the dashboards,
/// `drafts` the ones in a [`DRAFTS`] folder.
fn find_dashboards(
    dir: &Path,
    root: &Path,
    depth: usize,
    draft: bool,
    found: &mut Vec<String>,
    drafts: &mut Vec<String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            if depth < MAX_DEPTH {
                let draft = draft || name == DRAFTS;
                find_dashboards(&path, root, depth + 1, draft, found, drafts);
            }
        } else if crate::spec::tabs::is_spec(&path) {
            if let Ok(relative) = path.strip_prefix(root) {
                let relative = relative.to_string_lossy().replace('\\', "/");
                if draft { &mut *drafts } else { &mut *found }.push(relative);
            }
        }
    }
}

fn init(dir: &Path, name: Option<String>) -> Result<Value, CliError> {
    if !dir.is_dir() {
        return Err(CliError::failure(
            "io",
            format!("{} is not a folder", dir.display()),
        ));
    }
    let path = dir.join(MANIFEST);
    if path.exists() {
        return Err(
            CliError::failure("exists", format!("{} is already there", path.display()))
                .with_hint("Edit it, or see how it stands with `ducklocal project check`"),
        );
    }
    let mut dashboards = Vec::new();
    let mut drafts = Vec::new();
    find_dashboards(dir, dir, 0, false, &mut dashboards, &mut drafts);
    let folder = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let manifest = Manifest {
        version: VERSION,
        name: name.unwrap_or_else(|| {
            folder
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        }),
        dashboards,
        data: Vec::new(),
        requires: Requires {
            duckdb: engine().0,
            extensions: Vec::new(),
        },
    };
    write_manifest(&path, &manifest)?;
    // Said, not dropped in silence: a report kept in a folder of that name
    // by chance is one line in the manifest away.
    Ok(json!({"manifest": path.to_string_lossy(), "project": manifest, "drafts": drafts}))
}

fn write_manifest(path: &Path, manifest: &Manifest) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(manifest).map_err(|e| CliError::failure("io", e))?;
    proposal::write_whole(path, (text + "\n").as_bytes()).map_err(|e| CliError::failure("io", e))
}

/// `text` with the `path` of `source "name"` set to `to`, by position: nothing
/// else in the file moves. `None` when it has no such source.
fn with_source_path(text: &str, name: &str, to: &str) -> Option<String> {
    let parsed = model::parse(text).ok()?;
    let (spec, _) = model::validate_partial(&parsed);
    let named = spec.sources.iter().find(|source| source.name == name)?;
    let after_name = named.name_span.1.min(text.len());
    let rest = &text[after_name..];
    let key = rest.find("path")?;
    let open = key + rest[key..].find('"')?;
    let close = open + 1 + rest[open + 1..].find('"')?;
    let mut out = String::with_capacity(text.len() + to.len());
    out.push_str(&text[..after_name + open + 1]);
    out.push_str(to);
    out.push_str(&text[after_name + close..]);
    // A rewrite that does not read back as intended is not written at all.
    let reread = model::parse(&out).ok()?;
    let (spec, _) = model::validate_partial(&reread);
    spec.sources
        .iter()
        .any(|source| source.name == name && source.path == to)
        .then_some(out)
}

/// `to` as a dashboard or manifest in `base` should write it. Inside the project
/// it is relative to `base`, `..` and all, with forward slashes, so it moves
/// with the folder; outside, it can only be absolute, and `check` says so.
fn written_path(to: &Path, base: &Path, root: &Path) -> String {
    let absolute = |path: &Path| -> PathBuf {
        let path = std::fs::canonicalize(path)
            .or_else(|_| std::path::absolute(path))
            .unwrap_or_else(|_| path.to_path_buf());
        PathBuf::from(plain(&path))
    };
    let (to, base, root) = (absolute(to), absolute(base), absolute(root));
    let text = if to.starts_with(&root) {
        relative_to(&to, &base).to_string_lossy().into_owned()
    } else {
        to.to_string_lossy().into_owned()
    };
    text.replace('\\', "/")
}

/// Windows' canonical form carries a prefix nothing else writes.
fn plain(path: &Path) -> String {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => rest.to_string(),
        _ => text.into_owned(),
    }
}

fn relative_to(to: &Path, base: &Path) -> PathBuf {
    let to: Vec<_> = to.components().collect();
    let base: Vec<_> = base.components().collect();
    let shared = to.iter().zip(&base).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in shared..base.len() {
        out.push("..");
    }
    for part in &to[shared..] {
        out.push(part);
    }
    out
}

fn relink(
    manifest_path: &Path,
    source_name: Option<&str>,
    data_name: Option<&str>,
    to: &str,
) -> Result<Value, CliError> {
    let mut manifest = load(manifest_path).map_err(|e| CliError::failure("project", e))?;
    let root = source::base_of(manifest_path);
    let target = PathBuf::from(crate::db::expand_tilde(to));
    if is_remote(to) || to.contains('"') {
        return Err(CliError::argument(
            "--to names a local path, without quotation marks",
        ));
    }
    if !exists(&target) {
        return Err(CliError::argument(format!("--to: nothing at {to}")));
    }
    let mut changed = Vec::new();
    if let Some(name) = data_name {
        let written = written_path(&target, &root, &root);
        let entry = manifest
            .data
            .iter_mut()
            .find(|entry| entry.name == name)
            .ok_or_else(|| {
                CliError::failure("not_found", format!("The manifest has no data {name:?}"))
            })?;
        entry.path = written;
        write_manifest(manifest_path, &manifest)?;
        changed.push(manifest_path.to_string_lossy().into_owned());
    }
    if let Some(name) = source_name {
        if let Some(problem) = source::unreadable(to) {
            return Err(CliError::argument(problem));
        }
        // Worked out for every dashboard first, so a failure changes none.
        let mut edits = Vec::new();
        for dashboard in manifest.dashboards.iter().filter(|d| stays_inside(d)) {
            let path = root.join(dashboard);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let written = written_path(&target, &source::base_of(&path), &root);
            if let Some(updated) = with_source_path(&text, name, &written) {
                edits.push((path, text, updated));
            }
        }
        if edits.is_empty() {
            return Err(CliError::failure(
                "not_found",
                format!("No dashboard in the project has a source {name:?}"),
            ));
        }
        for (path, text, updated) in edits {
            let backup = proposal::backup_path_for(&path);
            proposal::write_whole(&backup, text.as_bytes())
                .and_then(|()| proposal::write_whole(&path, updated.as_bytes()))
                .map_err(|e| CliError::failure("io", e))?;
            changed.push(path.to_string_lossy().into_owned());
        }
    }
    Ok(json!({"changed": changed, "backups": "beside each dashboard, as NAME.dash.bak"}))
}

pub(crate) const HELP: &str = "\
Usage: ducklocal project init [DIR] [--name NAME]
       ducklocal project check [PATH]
       ducklocal project relink [PATH] (--source NAME | --data NAME) --to PATH

Keep an analysis movable: a folder with a ducklocal-project.json saying which
dashboards it holds and what they need. Optional: a lone .dash file works as it
always has. Data is referenced, never copied or uploaded. Opening the manifest
in the window opens its dashboards. PATH is the folder or the manifest, by
default the current folder.

  init    Write a manifest listing the dashboards found under DIR, and this
          DuckDB's version. Refuses to replace one (kind exists). A .dash file
          in a folder named proposals is a draft for `ducklocal propose`, so it
          is left out of the manifest and listed under \"drafts\".
  check   Run `ducklocal check` over each dashboard and report what a move would
          break. Never reaches the network: a dashboard reading remote data is
          left unrun, and says so. Installs nothing.
  relink  Point a source at where its file is now, in every dashboard that
          names it (each kept as NAME.dash.bak), or a manifest data entry.
          Ask the person first: this changes their dashboards.

The manifest, relative paths with forward slashes:
  {\"version\": 1, \"name\": \"orders\",
   \"dashboards\": [\"reports/orders.dash\"],
   \"data\": [{\"name\": \"archive\", \"path\": \"data/old.csv\", \"sample\": \"data/s.csv\"}],
   \"requires\": {\"duckdb\": \"v2.0.0\", \"extensions\": [\"httpfs\"]}}

check prints {project, manifest, ok, portable, ducklocal, duckdb, dashboards,
data, extensions, problems, limitations} and exits 0 whatever it found, so read
ok: false means something is missing or failed, portable: false that a path will
not survive a move. Each problem has {kind, what, hint}. limitations says what
passing does not cover. Exit 2 for arguments, 1 with kind project for a manifest
that does not read, or not_found.
";

const SPEC: &[FlagSpec] = &[
    FlagSpec {
        name: "--name",
        takes_value: true,
    },
    FlagSpec {
        name: "--source",
        takes_value: true,
    },
    FlagSpec {
        name: "--data",
        takes_value: true,
    },
    FlagSpec {
        name: "--to",
        takes_value: true,
    },
];

fn manifest_at(path: Option<PathBuf>) -> Result<PathBuf, CliError> {
    let path = path.unwrap_or_else(|| PathBuf::from("."));
    let manifest = if path.is_dir() {
        path.join(MANIFEST)
    } else {
        path
    };
    if !manifest.is_file() {
        return Err(
            CliError::failure("not_found", format!("No {}", manifest.display()))
                .with_hint("Make one with `ducklocal project init`"),
        );
    }
    Ok(manifest)
}

/// `ducklocal project`.
pub(crate) fn run(args: &[OsString]) -> Result<String, CliError> {
    let Some(action) = args.first().and_then(|arg| arg.to_str()) else {
        return Err(CliError::argument("Say what to do: init, check or relink"));
    };
    let mut path = None;
    let (mut name, mut source_name, mut data_name, mut to) = (None, None, None, None);
    let utf8 = |value: OsString, what: &str| {
        value
            .into_string()
            .map_err(|_| CliError::argument(format!("{what} must be UTF-8")))
    };
    for arg in parse_args("project", &args[1..], SPEC, &[])? {
        match arg {
            Arg::Flag("--name", Some(value)) => name = Some(utf8(value, "--name")?),
            Arg::Flag("--source", Some(value)) => source_name = Some(utf8(value, "--source")?),
            Arg::Flag("--data", Some(value)) => data_name = Some(utf8(value, "--data")?),
            Arg::Flag("--to", Some(value)) => to = Some(utf8(value, "--to")?),
            Arg::Positional(value) if path.is_none() => path = Some(PathBuf::from(value)),
            Arg::Positional(_) => return Err(CliError::argument("Name one project")),
            _ => {
                return Err(CliError::failure(
                    "internal",
                    "project accepted a flag it does not handle",
                ))
            }
        }
    }
    let relinking = source_name.is_some() || data_name.is_some() || to.is_some();
    let output = match action {
        "init" if !relinking => init(&path.unwrap_or_else(|| PathBuf::from(".")), name)?,
        "check" if !relinking && name.is_none() => check_project(&manifest_at(path)?)?,
        "relink" if name.is_none() => {
            let to = to.ok_or_else(|| CliError::argument("Say where with --to PATH"))?;
            if source_name.is_some() == data_name.is_some() {
                return Err(CliError::argument("Name one of --source and --data"));
            }
            relink(
                &manifest_at(path)?,
                source_name.as_deref(),
                data_name.as_deref(),
                &to,
            )?
        }
        "init" | "check" | "relink" => {
            return Err(CliError::argument(format!(
                "Those options do not go with {action}; see ducklocal project --help"
            )))
        }
        other => {
            return Err(CliError::argument(format!(
                "Unknown project action: {other}; use init, check or relink"
            )))
        }
    };
    Ok(output.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_relative_path_inside_stays_inside() {
        for inside in ["a.dash", "reports/a.dash", "./a.dash", "b/../a.dash"] {
            assert!(stays_inside(inside), "{inside}");
        }
        for outside in [
            "../a.dash",
            "a/../../b.dash",
            "/etc/a.dash",
            "C:/a.dash",
            r"C:\a.dash",
            r"reports\a.dash",
            "~/a.dash",
        ] {
            assert!(!stays_inside(outside), "{outside}");
        }
    }

    #[test]
    fn a_query_naming_a_fixed_or_remote_path_is_found() {
        assert_eq!(
            paths_in_sql("SELECT * FROM '/data/a.csv' JOIN 'b.csv' ON x = 'y'"),
            vec!["/data/a.csv"]
        );
        assert_eq!(
            paths_in_sql("SELECT * FROM read_csv('s3://b/a.csv'), 'C:/x.csv'"),
            vec!["s3://b/a.csv", "C:/x.csv"]
        );
        assert!(paths_in_sql("SELECT 'it''s' AS a").is_empty());
    }

    #[test]
    fn relinking_changes_that_one_path_and_nothing_else() {
        let text = "// orders\nsource \"a\" {\n  path = \"old.csv\"\n}\n\nsource \"b\" {\n  \
                    path = \"b.csv\"\n}\n\nquery \"q\" {\n  sql = <<SQL\nSELECT 'path' FROM a\nSQL\n}\n";
        let updated = with_source_path(text, "a", "data/new.csv").unwrap();
        assert_eq!(updated, text.replace("old.csv", "data/new.csv"));
        let updated = with_source_path(text, "b", "x.csv").unwrap();
        assert_eq!(updated, text.replace("b.csv", "x.csv"));
        assert_eq!(with_source_path(text, "nope", "x.csv"), None);
        assert_eq!(with_source_path("not a spec {", "a", "x.csv"), None);
    }

    #[test]
    fn a_path_is_written_relative_to_where_it_will_be_read() {
        let dir = std::env::temp_dir().join(format!("ducklocal_project_w_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("p/reports")).unwrap();
        std::fs::create_dir_all(dir.join("p/data")).unwrap();
        std::fs::write(
            dir.join("p/data/a.csv"),
            "n
",
        )
        .unwrap();
        std::fs::write(
            dir.join("p/reports/b.csv"),
            "n
",
        )
        .unwrap();
        std::fs::write(
            dir.join("elsewhere.csv"),
            "n
",
        )
        .unwrap();
        let (root, reports) = (dir.join("p"), dir.join("p/reports"));
        let written = |to: &str, base: &Path| written_path(&dir.join(to), base, &root);
        assert_eq!(written("p/data/a.csv", &reports), "../data/a.csv");
        assert_eq!(written("p/reports/b.csv", &reports), "b.csv");
        assert_eq!(written("p/data/a.csv", &root), "data/a.csv");
        // Outside the project: absolute, since no relative path would travel.
        let outside = written("elsewhere.csv", &reports);
        assert!(looks_absolute(&outside), "{outside}");
        assert!(!outside.starts_with("//"), "no Windows prefix: {outside}");
        assert!(!outside.contains('\\'), "{outside}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// What the window does with a manifest it is handed: its dashboards, as
    /// any dropped `.dash` would open, and only those that are there and inside.
    #[test]
    fn the_window_opens_a_manifest_as_its_dashboards() {
        let dir = std::env::temp_dir().join(format!("ducklocal_project_t_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("r")).unwrap();
        std::fs::write(dir.join("r/a.dash"), "").unwrap();
        std::fs::write(
            dir.join("x.csv"),
            "n
",
        )
        .unwrap();
        let path = dir.join(MANIFEST);
        std::fs::write(
            &path,
            r#"{"version": 1, "dashboards": ["r/a.dash", "absent.dash", "../out.dash"]}"#,
        )
        .unwrap();
        let text = |p: &Path| p.to_string_lossy().into_owned();
        let (specs, rest) = crate::spec::tabs::split(vec![text(&path), text(&dir.join("x.csv"))]);
        assert_eq!(specs.len(), 1, "{specs:?}");
        assert!(specs[0].ends_with("r/a.dash"), "{specs:?}");
        assert_eq!(rest, vec![text(&dir.join("x.csv"))]);

        // One that does not read goes on as data, whose open says what is wrong.
        std::fs::write(&path, "{ nope").unwrap();
        let (specs, rest) = crate::spec::tabs::split(vec![text(&path)]);
        assert!(specs.is_empty());
        assert_eq!(rest, vec![text(&path)]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_manifest_of_another_version_says_so() {
        let dir = std::env::temp_dir().join(format!("ducklocal_project_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(MANIFEST);
        std::fs::write(&path, r#"{"version": 99}"#).unwrap();
        assert!(load(&path).unwrap_err().contains("version 99"));
        std::fs::write(&path, "{ nope").unwrap();
        assert!(load(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_manifest_opens_only_the_dashboards_inside_it() {
        let dir = std::env::temp_dir().join(format!("ducklocal_project_o_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(MANIFEST);
        std::fs::write(
            &path,
            r#"{"version": 1, "dashboards": ["a.dash", "../evil.dash", "/tmp/evil.dash"]}"#,
        )
        .unwrap();
        assert_eq!(dashboards_of(&path).unwrap(), vec![dir.join("a.dash")]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
