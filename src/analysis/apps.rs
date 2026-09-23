//! Which apps are open, remembered between launches.
//!
//! An app tab is a document like any other, so it comes back the way a
//! registered data file does: the list is one `settings` value, written when
//! the set of tabs changes. A remembered directory that is gone, or that is no
//! longer an app, is dropped with a reason rather than reopened as an empty
//! tab — an empty tab would say "I lost your work" without saying which.

use std::path::{Path, PathBuf};

use crate::analysis::host;
use crate::i18n::trf;

/// The `settings` key the list lives under.
///
/// The value predates the "app" name and must not change: it is the key the
/// remembered tabs of every existing install live under.
pub const SETTING: &str = "analysis_panels";

/// One remembered app tab.
///
/// The field names are the stored JSON's, so they keep their old shape even
/// though the concept has been renamed: a stored list this build cannot read
/// would come back as no tabs at all.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OpenApp {
    pub path: String,
    /// The tab's title, kept so a renamed tab comes back renamed.
    pub title: String,
}

impl OpenApp {
    pub fn new(path: PathBuf, title: impl Into<String>) -> Self {
        Self {
            path: path.to_string_lossy().to_string(),
            title: title.into(),
        }
    }
}

/// The default title of an app tab: its directory's name.
pub fn title_for(directory: &Path) -> String {
    directory
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| directory.to_string_lossy().to_string())
}

/// What reopening the remembered apps produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Restored {
    /// Directories that are still apps, in the order they were remembered.
    pub apps: Vec<OpenApp>,
    /// One message per app that was dropped, naming it and what changed.
    pub problems: Vec<String>,
}

/// Separate the paths that name an app from the ones that do not.
///
/// An app is a directory with the entry file in it; everything else — data
/// files, folders of data, databases, globs — keeps the path behaviour it had.
pub fn split_paths(paths: &[String]) -> (Vec<PathBuf>, Vec<String>) {
    let mut apps = Vec::new();
    let mut rest = Vec::new();
    for path in paths {
        // `~` is expanded by the data path resolver too; an app has to test
        // the same directory that resolver would open.
        let expanded = PathBuf::from(crate::db::expand_tilde(path));
        if host::validate_application(&expanded).is_ok() {
            apps.push(expanded);
        } else {
            rest.push(path.clone());
        }
    }
    (apps, rest)
}

pub fn to_json(apps: &[OpenApp]) -> String {
    serde_json::to_string(apps).unwrap_or_else(|_| "[]".to_string())
}

/// Read back a remembered list. A value this build cannot read is no apps
/// rather than a launch that fails: the setting is a convenience, not data.
pub fn from_json(json: &str) -> Vec<OpenApp> {
    serde_json::from_str(json).unwrap_or_default()
}

/// Drop the apps that can no longer be opened, saying which and why.
pub fn keep_available(apps: Vec<OpenApp>) -> Restored {
    let mut restored = Restored::default();
    for app in apps {
        let path = Path::new(&app.path);
        match host::validate_application(path) {
            Ok(()) => restored.apps.push(app),
            Err(rejection) => restored.problems.push(match rejection {
                host::Rejection::NotADirectory => {
                    trf("analysis.restore.not_a_folder", &[&app.path, &app.title])
                }
                host::Rejection::NoEntryFile => {
                    trf("analysis.restore.no_entry", &[&app.path, &app.title])
                }
            }),
        }
    }
    restored
}

/// Write the open apps, replacing what was there.
pub fn remember(apps: &[OpenApp]) {
    if let Err(error) = crate::history::set_setting(SETTING, &to_json(apps)) {
        tracing::warn!("Could not remember the open analysis apps: {error}");
    }
}

/// The apps to reopen at launch, and what happened to the ones that could
/// not be.
pub fn restore() -> Restored {
    match crate::history::get_setting(SETTING) {
        Ok(Some(json)) => keep_available(from_json(&json)),
        Ok(None) => Restored::default(),
        Err(error) => {
            tracing::warn!("Could not read the open analysis apps: {error}");
            Restored::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory that removes itself, named for the test that made it.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ducklocal_apps_{label}_{}_{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::remove_dir_all(&path).ok();
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn app(&self) {
            std::fs::write(self.0.join(host::ENTRY), "export default class A {}").unwrap();
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn an_app_directory_is_told_apart_from_the_paths_that_are_data() {
        let directory = TempDir::new("split");
        directory.app();
        let data = TempDir::new("split_data");
        let csv = data.path().join("sales.csv");
        std::fs::write(&csv, "a\n1\n").unwrap();

        let paths = vec![
            directory.path().to_string_lossy().to_string(),
            csv.to_string_lossy().to_string(),
            data.path().to_string_lossy().to_string(),
            "/no/such/path".to_string(),
        ];
        let (apps, rest) = split_paths(&paths);

        assert_eq!(apps, vec![directory.path().to_path_buf()]);
        // The folder of data files is not an app, and neither is anything
        // that does not exist: those keep their data-path behaviour.
        assert_eq!(rest.len(), 3);
        assert!(rest.contains(&csv.to_string_lossy().to_string()));
    }

    #[test]
    fn the_remembered_list_survives_a_round_trip() {
        let apps = vec![
            OpenApp::new(PathBuf::from("/apps/sales"), "Sales"),
            OpenApp::new(PathBuf::from("/apps/orders"), "orders"),
        ];
        assert_eq!(from_json(&to_json(&apps)), apps);
    }

    #[test]
    fn a_remembered_list_this_build_cannot_read_is_no_apps() {
        assert_eq!(from_json("not json"), Vec::new());
        assert_eq!(from_json(""), Vec::new());
        assert_eq!(from_json("[{\"unexpected\": 1}]"), Vec::new());
    }

    #[test]
    fn restoring_keeps_the_apps_that_are_still_there() {
        let present = TempDir::new("restore_ok");
        present.app();
        let missing = present.path().join("gone");

        let restored = keep_available(vec![
            OpenApp::new(present.path().to_path_buf(), "kept"),
            OpenApp::new(missing.clone(), "dropped"),
        ]);

        assert_eq!(restored.apps.len(), 1);
        assert_eq!(restored.apps[0].title, "kept");
        assert_eq!(restored.problems.len(), 1);
        let problem = &restored.problems[0];
        assert!(problem.contains("dropped"), "{problem}");
        assert!(
            problem.contains(&missing.to_string_lossy().to_string()),
            "{problem}"
        );
    }

    #[test]
    fn restoring_drops_a_directory_that_is_no_longer_an_app() {
        let directory = TempDir::new("restore_no_entry");
        let restored = keep_available(vec![OpenApp::new(
            directory.path().to_path_buf(),
            "was an app",
        )]);

        assert!(restored.apps.is_empty());
        assert_eq!(restored.problems.len(), 1);
        assert!(
            restored.problems[0].contains(host::ENTRY),
            "{:?}",
            restored.problems
        );
    }

    #[test]
    fn a_title_falls_back_to_the_whole_path() {
        assert_eq!(title_for(Path::new("/apps/sales")), "sales");
        assert_eq!(title_for(Path::new("/")), "/");
    }
}
