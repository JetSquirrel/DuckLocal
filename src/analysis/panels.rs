//! Which panels are open, remembered between launches.
//!
//! A panel tab is a document like any other, so it comes back the way a
//! registered data file does: the list is one `settings` value, written when
//! the set of tabs changes. A remembered directory that is gone, or that is no
//! longer a panel, is dropped with a reason rather than reopened as an empty
//! tab — an empty tab would say "I lost your work" without saying which.

use std::path::{Path, PathBuf};

use crate::analysis::host;
use crate::i18n::trf;

/// The `settings` key the list lives under.
pub const SETTING: &str = "analysis_panels";

/// One remembered panel tab.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OpenPanel {
    pub path: String,
    /// The tab's title, kept so a renamed tab comes back renamed.
    pub title: String,
}

impl OpenPanel {
    pub fn new(path: PathBuf, title: impl Into<String>) -> Self {
        Self {
            path: path.to_string_lossy().to_string(),
            title: title.into(),
        }
    }
}

/// The default title of a panel tab: its directory's name.
pub fn title_for(directory: &Path) -> String {
    directory
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| directory.to_string_lossy().to_string())
}

/// What reopening the remembered panels produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Restored {
    /// Directories that are still panels, in the order they were remembered.
    pub panels: Vec<OpenPanel>,
    /// One message per panel that was dropped, naming it and what changed.
    pub problems: Vec<String>,
}

/// Separate the paths that name a panel from the ones that do not.
///
/// A panel is a directory with the entry file in it; everything else — data
/// files, folders of data, databases, globs — keeps the path behaviour it had.
pub fn split_paths(paths: &[String]) -> (Vec<PathBuf>, Vec<String>) {
    let mut panels = Vec::new();
    let mut rest = Vec::new();
    for path in paths {
        // `~` is expanded by the data path resolver too; a panel has to test
        // the same directory that resolver would open.
        let expanded = PathBuf::from(crate::db::expand_tilde(path));
        if host::validate_application(&expanded).is_ok() {
            panels.push(expanded);
        } else {
            rest.push(path.clone());
        }
    }
    (panels, rest)
}

pub fn to_json(panels: &[OpenPanel]) -> String {
    serde_json::to_string(panels).unwrap_or_else(|_| "[]".to_string())
}

/// Read back a remembered list. A value this build cannot read is no panels
/// rather than a launch that fails: the setting is a convenience, not data.
pub fn from_json(json: &str) -> Vec<OpenPanel> {
    serde_json::from_str(json).unwrap_or_default()
}

/// Drop the panels that can no longer be opened, saying which and why.
pub fn keep_available(panels: Vec<OpenPanel>) -> Restored {
    let mut restored = Restored::default();
    for panel in panels {
        let path = Path::new(&panel.path);
        match host::validate_application(path) {
            Ok(()) => restored.panels.push(panel),
            Err(rejection) => restored.problems.push(match rejection {
                host::Rejection::NotADirectory => trf(
                    "analysis.restore.not_a_folder",
                    &[&panel.path, &panel.title],
                ),
                host::Rejection::NoEntryFile => {
                    trf("analysis.restore.no_entry", &[&panel.path, &panel.title])
                }
            }),
        }
    }
    restored
}

/// Write the open panels, replacing what was there.
pub fn remember(panels: &[OpenPanel]) {
    if let Err(error) = crate::history::set_setting(SETTING, &to_json(panels)) {
        tracing::warn!("Could not remember the open analysis panels: {error}");
    }
}

/// The panels to reopen at launch, and what happened to the ones that could
/// not be.
pub fn restore() -> Restored {
    match crate::history::get_setting(SETTING) {
        Ok(Some(json)) => keep_available(from_json(&json)),
        Ok(None) => Restored::default(),
        Err(error) => {
            tracing::warn!("Could not read the open analysis panels: {error}");
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
                "ducklocal_panels_{label}_{}_{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::remove_dir_all(&path).ok();
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn panel(&self) {
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
    fn a_panel_directory_is_told_apart_from_the_paths_that_are_data() {
        let directory = TempDir::new("split");
        directory.panel();
        let data = TempDir::new("split_data");
        let csv = data.path().join("sales.csv");
        std::fs::write(&csv, "a\n1\n").unwrap();

        let paths = vec![
            directory.path().to_string_lossy().to_string(),
            csv.to_string_lossy().to_string(),
            data.path().to_string_lossy().to_string(),
            "/no/such/path".to_string(),
        ];
        let (panels, rest) = split_paths(&paths);

        assert_eq!(panels, vec![directory.path().to_path_buf()]);
        // The folder of data files is not a panel, and neither is anything
        // that does not exist: those keep their data-path behaviour.
        assert_eq!(rest.len(), 3);
        assert!(rest.contains(&csv.to_string_lossy().to_string()));
    }

    #[test]
    fn the_remembered_list_survives_a_round_trip() {
        let panels = vec![
            OpenPanel::new(PathBuf::from("/panels/sales"), "Sales"),
            OpenPanel::new(PathBuf::from("/panels/orders"), "orders"),
        ];
        assert_eq!(from_json(&to_json(&panels)), panels);
    }

    #[test]
    fn a_remembered_list_this_build_cannot_read_is_no_panels() {
        assert_eq!(from_json("not json"), Vec::new());
        assert_eq!(from_json(""), Vec::new());
        assert_eq!(from_json("[{\"unexpected\": 1}]"), Vec::new());
    }

    #[test]
    fn restoring_keeps_the_panels_that_are_still_there() {
        let present = TempDir::new("restore_ok");
        present.panel();
        let missing = present.path().join("gone");

        let restored = keep_available(vec![
            OpenPanel::new(present.path().to_path_buf(), "kept"),
            OpenPanel::new(missing.clone(), "dropped"),
        ]);

        assert_eq!(restored.panels.len(), 1);
        assert_eq!(restored.panels[0].title, "kept");
        assert_eq!(restored.problems.len(), 1);
        let problem = &restored.problems[0];
        assert!(problem.contains("dropped"), "{problem}");
        assert!(
            problem.contains(&missing.to_string_lossy().to_string()),
            "{problem}"
        );
    }

    #[test]
    fn restoring_drops_a_directory_that_is_no_longer_a_panel() {
        let directory = TempDir::new("restore_no_entry");
        let restored = keep_available(vec![OpenPanel::new(
            directory.path().to_path_buf(),
            "was a panel",
        )]);

        assert!(restored.panels.is_empty());
        assert_eq!(restored.problems.len(), 1);
        assert!(
            restored.problems[0].contains(host::ENTRY),
            "{:?}",
            restored.problems
        );
    }

    #[test]
    fn a_title_falls_back_to_the_whole_path() {
        assert_eq!(title_for(Path::new("/panels/sales")), "sales");
        assert_eq!(title_for(Path::new("/")), "/");
    }
}
