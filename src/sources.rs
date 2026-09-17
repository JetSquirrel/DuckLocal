//! Resolving the paths DuckLocal is asked to open: command-line arguments,
//! picker results, and files dropped on the window.
//!
//! A path can mean three things — a data file to attach as a view, a directory
//! whose data files are all attached, or a database file to open. Expansion
//! walks the filesystem, so callers run it off the UI thread.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use crate::db::{self, expand_tilde};
use crate::i18n::trf;

/// How many data files one request may attach. A directory chosen by mistake
/// (a home directory, a repository root) can hold thousands of matching files;
/// attaching them all would tie up the connection for minutes.
pub const MAX_FILES: usize = 256;

/// What a set of requested paths resolved to.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Sources {
    /// Database file to open; `None` means the request named data files only.
    pub database: Option<String>,
    /// Data files to attach as views, in attachment order.
    pub files: Vec<String>,
    /// Paths that could not be used, each with a message for the user.
    pub problems: Vec<String>,
    /// Set when `files` was cut off at [`MAX_FILES`].
    pub truncated: bool,
}

/// Resolve every requested path, in the order given.
pub fn resolve(paths: &[String]) -> Sources {
    let mut sources = Sources::default();
    let mut seen = BTreeSet::new();
    for path in paths {
        resolve_one(path, &mut sources, &mut seen);
    }
    sources
}

pub fn resolve_dialog_path(raw: &str) -> Sources {
    let path = expand_tilde(raw);
    if !is_pattern(&path)
        && Path::new(&path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("duckdb"))
        && std::fs::metadata(&path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    {
        return Sources {
            database: Some(path),
            ..Sources::default()
        };
    }
    resolve(&[path])
}

fn resolve_one(raw: &str, sources: &mut Sources, seen: &mut BTreeSet<String>) {
    let path = expand_tilde(raw);

    if is_pattern(&path) {
        let matches = glob(&path);
        if matches.is_empty() {
            sources.problems.push(trf("error.glob_no_match", &[&path]));
        }
        for file in matches {
            add_file(file, sources, seen);
        }
        return;
    }

    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_dir() => {
            let mut found = Vec::new();
            collect_data_files(Path::new(&path), &mut found);
            if found.is_empty() {
                sources.problems.push(trf("error.no_data_files", &[&path]));
            }
            for file in found {
                add_file(file, sources, seen);
            }
        }
        // A file that is not a data file is a database to open: one the user
        // named, and one this app already creates with an extension it does
        // not recognise as data.
        Ok(_) => {
            if db::is_data_file(&path) {
                add_file(path, sources, seen);
            } else {
                sources.database = Some(path);
            }
        }
        Err(_) => sources.problems.push(trf("error.file_not_found", &[&path])),
    }
}

/// Collect the data files under `dir`, recursively, in a deterministic order.
/// Hidden entries are skipped, and so are symlinks: a link back up the tree
/// would not terminate, and the linked-to directory is normally walked where
/// it actually lives.
fn collect_data_files(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| !is_hidden(path))
        .collect();
    paths.sort();

    for path in paths {
        let Ok(file_type) = std::fs::symlink_metadata(&path).map(|meta| meta.file_type()) else {
            continue;
        };
        if file_type.is_dir() {
            collect_data_files(&path, out);
        } else if file_type.is_file() && db::is_data_file(&path.to_string_lossy()) {
            out.push(path.to_string_lossy().to_string());
        }
    }
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
}

pub fn canonical_file_path(path: &str) -> std::io::Result<String> {
    std::fs::canonicalize(expand_tilde(path)).map(|path| path.to_string_lossy().into_owned())
}

/// Add one file to the request, ignoring repeats and everything past the cap.
fn add_file(path: String, sources: &mut Sources, seen: &mut BTreeSet<String>) {
    let path = match canonical_file_path(&path) {
        Ok(path) => path,
        Err(_) => {
            sources.problems.push(trf("error.file_not_found", &[&path]));
            return;
        }
    };
    if !seen.insert(path.clone()) {
        return;
    }
    if sources.files.len() >= MAX_FILES {
        sources.truncated = true;
        return;
    }
    sources.files.push(path);
}

/// Whether `path` is a pattern to expand here. Only `*` and `?` are patterns;
/// the shell normally expands them before DuckLocal starts, so this matters
/// for the quoted form (`ducklocal './data/*.csv'`) and for dropped paths.
fn is_pattern(path: &str) -> bool {
    path.contains('*') || path.contains('?')
}

/// Expand a pattern in every path component against the filesystem.
fn glob(pattern: &str) -> Vec<String> {
    let path = Path::new(pattern);
    let mut dirs: Vec<PathBuf> = if path.is_absolute() {
        vec![PathBuf::from("/")]
    } else {
        vec![PathBuf::from(".")]
    };

    for component in path.components() {
        let part = match component {
            Component::CurDir | Component::RootDir => continue,
            // `..` is a location, not a pattern: follow it literally.
            Component::ParentDir => "..".to_string(),
            Component::Normal(name) => name.to_string_lossy().to_string(),
            Component::Prefix(_) => return Vec::new(),
        };

        let mut next = Vec::new();
        if is_pattern(&part) {
            for dir in &dirs {
                let Ok(entries) = std::fs::read_dir(dir) else {
                    continue;
                };
                let mut matches: Vec<PathBuf> = entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| {
                        !is_hidden(path)
                            && path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .is_some_and(|name| match_pattern(&part, name))
                    })
                    .collect();
                matches.sort();
                next.extend(matches);
            }
        } else {
            next.extend(dirs.iter().map(|dir| dir.join(&part)));
        }
        dirs = next;
    }

    dirs.into_iter()
        .filter(|path| path.is_file())
        .map(|path| path.to_string_lossy().to_string())
        .collect()
}

/// Match one path component against a `*` / `?` pattern: `*` stands for any
/// run of characters, `?` for exactly one. There is no escape character.
fn match_pattern(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut p, mut n) = (0, 0);
    // Last `*` seen, and the name position it was matched at, so a partial
    // match can be extended instead of restarted.
    let mut star = None;
    let mut resume = 0;

    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            resume = n;
            p += 1;
        } else if let Some(star_ix) = star {
            p = star_ix + 1;
            resume += 1;
            n = resume;
        } else {
            return false;
        }
    }

    pattern[p..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ducklocal_sources_{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "a\n1\n").unwrap();
    }

    #[test]
    fn pattern_matching() {
        assert!(match_pattern("*.csv", "costs.csv"));
        assert!(match_pattern("costs.*", "costs.csv"));
        assert!(match_pattern("c?st?.csv", "costs.csv"));
        assert!(match_pattern("*", "anything"));
        assert!(match_pattern("logs-*", "logs-2026-09"));
        assert!(!match_pattern("*.csv", "costs.parquet"));
        assert!(!match_pattern("costs.csv", "costs.csv.gz"));
        assert!(!match_pattern("?.csv", "a1.csv"));
    }

    #[test]
    fn directory_expands_recursively_and_hidden_entries_are_skipped() {
        let dir = scratch("dir");
        touch(&dir.join("b.csv"));
        touch(&dir.join("a.parquet"));
        touch(&dir.join("nested/deep/c.json"));
        touch(&dir.join(".hidden.csv"));
        touch(&dir.join("notes.md"));

        let sources = resolve(&[dir.to_string_lossy().to_string()]);
        let names: Vec<String> = sources
            .files
            .iter()
            .map(|path| {
                Path::new(path)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            })
            .collect();

        // Depth-first, and sorted at each level: the walk order is what the
        // views are created in, so it has to be stable across runs.
        assert_eq!(names, ["a.parquet", "b.csv", "c.json"]);
        assert!(sources.problems.is_empty());
        assert_eq!(sources.database, None);
    }

    #[test]
    fn directory_without_data_files_is_reported() {
        let dir = scratch("no_data");
        touch(&dir.join("README.md"));

        let sources = resolve(&[dir.to_string_lossy().to_string()]);

        assert!(sources.files.is_empty());
        assert_eq!(sources.problems.len(), 1);
    }

    #[test]
    fn glob_expands_matches_in_the_last_component() {
        let dir = scratch("glob");
        touch(&dir.join("data/logs-a.csv"));
        touch(&dir.join("data/logs-b.csv"));
        touch(&dir.join("data/other.parquet"));
        let pattern = format!("{}/*/*.csv", dir.to_string_lossy());

        let sources = resolve(&[pattern]);

        assert_eq!(sources.files.len(), 2);
        assert!(sources.files.iter().all(|path| path.ends_with(".csv")));
        assert!(sources.problems.is_empty());
    }

    #[test]
    fn unmatched_glob_is_reported() {
        let dir = scratch("no_match");
        let pattern = format!("{}/*.csv", dir.to_string_lossy());

        let sources = resolve(&[pattern]);

        assert!(sources.files.is_empty());
        assert_eq!(sources.problems.len(), 1);
    }

    #[test]
    fn database_and_data_files_split_by_kind() {
        let dir = scratch("mixed");
        let csv = dir.join("events.csv");
        let duckdb = dir.join("warehouse.duckdb");
        touch(&csv);
        touch(&duckdb);

        let sources = resolve(&[
            csv.to_string_lossy().to_string(),
            duckdb.to_string_lossy().to_string(),
        ]);

        assert_eq!(
            sources.files,
            [canonical_file_path(csv.to_str().unwrap()).unwrap()]
        );
        assert_eq!(sources.database, Some(duckdb.to_string_lossy().to_string()));
    }

    #[test]
    fn repeated_paths_are_attached_once() {
        let dir = scratch("repeats");
        let csv = dir.join("events.csv");
        touch(&csv);

        let path = csv.to_string_lossy().to_string();
        let sources = resolve(&[
            path.clone(),
            dir.join("./events.csv").to_string_lossy().into_owned(),
            dir.to_string_lossy().to_string(),
        ]);

        assert_eq!(sources.files, [canonical_file_path(&path).unwrap()]);
    }

    #[test]
    fn missing_path_is_reported() {
        let sources = resolve(&["/nonexistent/ducklocal/missing.csv".to_string()]);

        assert!(sources.files.is_empty());
        assert_eq!(sources.problems.len(), 1);
    }

    #[test]
    fn only_explicit_database_dialog_paths_allow_creation() {
        let dir = scratch("create_dialog");
        for name in ["new.duckdb", "new.DUCKDB"] {
            let path = dir.join(name).to_string_lossy().into_owned();
            assert_eq!(resolve_dialog_path(&path).database, Some(path.clone()));
            let ordinary = resolve(&[path]);
            assert!(ordinary.database.is_none());
            assert_eq!(ordinary.problems.len(), 1);
        }
        for name in [
            "missing.csv",
            "missing",
            "missing.txt",
            "*.duckdb",
            "?.duckdb",
        ] {
            let path = dir.join(name).to_string_lossy().into_owned();
            let sources = resolve_dialog_path(&path);
            assert!(sources.database.is_none(), "{name}");
            assert_eq!(sources.problems.len(), 1, "{name}");
        }
    }

    #[test]
    fn relative_and_absolute_paths_resolve_to_one_file() {
        let relative = format!("target/source-path-{}.csv", std::process::id());
        touch(Path::new(&relative));
        let absolute = canonical_file_path(&relative).unwrap();
        let sources = resolve(&[relative.clone(), absolute.clone(), format!("./{relative}")]);
        assert_eq!(sources.files, [absolute]);
        assert!(sources.problems.is_empty());
        std::fs::remove_file(relative).unwrap();
    }

    #[test]
    fn files_past_the_cap_are_dropped_and_reported() {
        let dir = scratch("cap");
        let mut sources = Sources::default();
        let mut seen = BTreeSet::new();
        for ix in 0..MAX_FILES + 3 {
            let path = dir.join(format!("file-{ix}.csv"));
            touch(&path);
            add_file(path.to_string_lossy().into_owned(), &mut sources, &mut seen);
        }

        assert_eq!(sources.files.len(), MAX_FILES);
        assert!(sources.truncated);
    }
}
