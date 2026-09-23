//! Which dashboard specs are open, remembered between launches.
//!
//! The same deal apps get (`src/analysis/apps.rs`): a dashboard tab is a
//! document, so the set of open ones is one `settings` value, written when the
//! set changes. A remembered file that is gone is dropped with a reason rather
//! than reopened as an empty tab. The entry shape is the apps' `OpenApp` —
//! a path and a title is all either kind needs.

use std::path::{Path, PathBuf};

use crate::analysis::apps::OpenApp;
use crate::i18n::trf;

/// The extension a dashboard spec carries.
pub const EXTENSION: &str = "dash";

/// The `settings` key the list lives under.
pub const SETTING: &str = "dashboard_tabs";

/// A path names a dashboard when it is a file with the spec's extension.
pub fn is_spec(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case(EXTENSION))
}

/// Pull the dashboard specs out of a path list, leaving the rest in order.
///
/// Runs after the apps' split: a directory with a `main.js` never reaches
/// here, and a `.dash` file inside one is the app's business, not a tab's.
pub fn split(paths: Vec<String>) -> (Vec<PathBuf>, Vec<String>) {
    let mut specs = Vec::new();
    let mut rest = Vec::new();
    for path in paths {
        let expanded = PathBuf::from(crate::db::expand_tilde(&path));
        if is_spec(&expanded) {
            specs.push(expanded);
        } else {
            rest.push(path);
        }
    }
    (specs, rest)
}

/// The default title of a dashboard tab: the file's stem.
pub fn title_for(path: &Path) -> String {
    path.file_stem()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

/// What reopening the remembered dashboards produced: the ones still on disk,
/// and one message per one that is not.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Restored {
    pub specs: Vec<OpenApp>,
    pub problems: Vec<String>,
}

pub fn to_json(specs: &[OpenApp]) -> String {
    serde_json::to_string(specs).unwrap_or_else(|_| "[]".to_string())
}

/// Read back a remembered list. A value this build cannot read is no
/// dashboards rather than a launch that fails.
pub fn from_json(json: &str) -> Vec<OpenApp> {
    serde_json::from_str(json).unwrap_or_default()
}

/// Drop the specs that are no longer there, saying which.
pub fn keep_available(specs: Vec<OpenApp>) -> Restored {
    let mut restored = Restored::default();
    for spec in specs {
        let path = Path::new(&spec.path);
        if is_spec(path) {
            restored.specs.push(spec);
        } else {
            restored
                .problems
                .push(trf("dashboard.restore.gone", &[&spec.path, &spec.title]));
        }
    }
    restored
}

/// Write the open dashboards, replacing what was there.
pub fn remember(specs: &[OpenApp]) {
    if let Err(error) = crate::history::set_setting(SETTING, &to_json(specs)) {
        tracing::warn!("Could not remember the open dashboards: {error}");
    }
}

/// The dashboards to reopen at launch, and what happened to the ones that
/// could not be.
pub fn restore() -> Restored {
    match crate::history::get_setting(SETTING) {
        Ok(Some(json)) => keep_available(from_json(&json)),
        Ok(None) => Restored::default(),
        Err(error) => {
            tracing::warn!("Could not read the open dashboards: {error}");
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
                "ducklocal_specs_{label}_{}_{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::remove_dir_all(&path).ok();
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn spec(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, "query \"q\" { sql = \"SELECT 1\" }").unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn a_dash_file_is_told_apart_from_the_paths_that_are_data() {
        let directory = TempDir::new("split");
        let spec = directory.spec("sales.dash");
        let csv = directory.spec("sales.csv");
        let missing = directory.0.join("gone.dash");

        let (specs, rest) = split(vec![
            spec.to_string_lossy().to_string(),
            csv.to_string_lossy().to_string(),
            missing.to_string_lossy().to_string(),
        ]);

        assert_eq!(specs, vec![spec]);
        // A non-spec file keeps its data behaviour; a spec that is not there
        // is a mistake to report, not a tab to open.
        assert_eq!(rest.len(), 2);
    }

    #[test]
    fn the_remembered_list_survives_a_round_trip() {
        let specs = vec![OpenApp::new(PathBuf::from("/dash/sales.dash"), "Sales")];
        assert_eq!(from_json(&to_json(&specs)), specs);
        assert_eq!(from_json("not json"), Vec::new());
    }

    #[test]
    fn restoring_drops_a_spec_that_is_gone() {
        let directory = TempDir::new("restore");
        let present = directory.spec("kept.dash");
        let gone = directory.0.join("gone.dash");

        let restored = keep_available(vec![
            OpenApp::new(present.clone(), "kept"),
            OpenApp::new(gone.clone(), "dropped"),
        ]);

        assert_eq!(restored.specs.len(), 1);
        assert_eq!(restored.specs[0].path, present.to_string_lossy());
        assert_eq!(restored.problems.len(), 1);
        assert!(restored.problems[0].contains("dropped"), "{restored:?}");
    }

    #[test]
    fn a_title_is_the_file_stem() {
        assert_eq!(title_for(Path::new("/dash/sales.dash")), "sales");
        assert_eq!(title_for(Path::new("/")), "/");
    }
}
