//! Host-side reload watching.
//!
//! `gpui_shell::Watcher` is not available to an embedding host: it hangs off
//! `ShellRoot::with_application`, which is `pub(crate)`, so `runtime.watch` and
//! `runtime.refresh` both fail with "does not contain a loaded script
//! application". This is the replacement — a directory stamp polled on a GPUI
//! timer, with a debounce so a save that rewrites several files at once is one
//! reload rather than one per file.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How often the directory is stamped.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How long the tree has to sit still before a change is acted on. An editor
/// that writes through a rename, or a formatter that rewrites every module,
/// lands inside one window.
pub const DEBOUNCE: Duration = Duration::from_millis(200);

/// One file the runtime would load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub path: PathBuf,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

/// Every `.js`/`.mjs` file the runtime could load, by path.
///
/// Compared as a whole: a file added, removed, resized, or touched all change
/// it, and a tree that did not move compares equal, which is what keeps an idle
/// window from reloading every 250ms.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppFiles {
    files: Vec<FileStamp>,
}

impl AppFiles {
    /// Stamp `root`. Unreadable files still get an entry, with `len` zero, so
    /// a permission change is a change like any other. A missing directory
    /// stamps empty rather than failing: the watcher keeps polling, and the
    /// window is already showing why the application did not load.
    pub fn capture(root: &Path) -> Self {
        let mut files = Vec::new();
        collect(root, &mut files);
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Self { files }
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }
}

fn collect(directory: &Path, out: &mut Vec<FileStamp>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Dotfiles last a keystroke in `.git`; the build directories belong to
        // other tools and change for reasons the script did not cause.
        if name.starts_with('.') || name == "node_modules" || name == "target" {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect(&path, out);
            continue;
        }
        if !matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("js") | Some("mjs")
        ) {
            continue;
        }
        let metadata = entry.metadata().ok();
        out.push(FileStamp {
            path,
            len: metadata.as_ref().map(|data| data.len()).unwrap_or(0),
            modified: metadata.and_then(|data| data.modified().ok()),
        });
    }
}

/// Turns a stream of "did the stamp change" answers into one reload.
///
/// Reports `true` once the tree has been unchanged for [`DEBOUNCE`] since the
/// last change, and never twice for one burst: a first change is the burst, so
/// nothing fires until the stamp comes back the same twice.
#[derive(Debug, Default)]
pub struct Debounce {
    changed_at: Option<Instant>,
}

impl Debounce {
    pub fn new() -> Self {
        Self::default()
    }

    /// `changed` is this poll's answer. `true` means "reload now".
    pub fn observe(&mut self, now: Instant, changed: bool) -> bool {
        if changed {
            self.changed_at = Some(now);
            return false;
        }
        match self.changed_at {
            Some(at) if now.duration_since(at) >= DEBOUNCE => {
                self.changed_at = None;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporary directory that removes itself. Tests here run in parallel,
    /// so the name carries the test's own label.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ducklocal_watch_{label}_{}_{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::remove_dir_all(&path).ok();
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.0.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, contents).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn an_untouched_tree_stamps_equal() {
        let dir = TempDir::new("idle");
        dir.write(
            "main.js",
            "export default class A { render() { return 1; } }",
        );
        dir.write("lib/panel.mjs", "export const x = 1;");

        assert_eq!(AppFiles::capture(dir.path()), AppFiles::capture(dir.path()));
        assert_eq!(AppFiles::capture(dir.path()).file_count(), 2);
    }

    #[test]
    fn only_javascript_is_stamped() {
        let dir = TempDir::new("extensions");
        dir.write("main.js", "1");
        dir.write("module.mjs", "2");
        dir.write("notes.md", "3");
        dir.write("data.json", "4");
        dir.write("plugin.ts", "5");

        assert_eq!(AppFiles::capture(dir.path()).file_count(), 2);
    }

    #[test]
    fn editor_and_build_directories_are_skipped() {
        let dir = TempDir::new("skipped");
        dir.write("main.js", "1");
        dir.write(".cache/bundled.js", "2");
        dir.write("node_modules/left-pad/index.js", "3");
        dir.write("target/bundle.js", "4");

        let files = AppFiles::capture(dir.path());
        assert_eq!(files.file_count(), 1);
        assert!(files.files[0].path.ends_with("main.js"));
    }

    #[test]
    fn adding_a_file_is_a_change() {
        let dir = TempDir::new("added");
        dir.write("main.js", "1");
        let before = AppFiles::capture(dir.path());

        dir.write("more.js", "2");

        assert_ne!(before, AppFiles::capture(dir.path()));
    }

    #[test]
    fn removing_a_file_is_a_change() {
        let dir = TempDir::new("removed");
        dir.write("main.js", "1");
        dir.write("more.js", "2");
        let before = AppFiles::capture(dir.path());

        std::fs::remove_file(dir.path().join("more.js")).unwrap();

        assert_ne!(before, AppFiles::capture(dir.path()));
    }

    #[test]
    fn resizing_a_file_is_a_change_even_when_the_clock_has_not_moved() {
        let dir = TempDir::new("resized");
        dir.write("main.js", "1");
        let before = AppFiles::capture(dir.path());

        dir.write("main.js", "much longer contents than before");

        let after = AppFiles::capture(dir.path());
        assert_ne!(before, after);
        assert_ne!(before.files[0].len, after.files[0].len);
    }

    #[test]
    fn a_missing_directory_stamps_empty_rather_than_failing() {
        let missing = std::env::temp_dir().join("ducklocal_watch_does_not_exist");
        std::fs::remove_dir_all(&missing).ok();
        assert_eq!(AppFiles::capture(&missing).file_count(), 0);
    }

    #[test]
    fn one_burst_of_changes_fires_one_reload() {
        let start = Instant::now();
        let mut debounce = Debounce::new();

        // The change itself is not the reload: the writes are still landing.
        assert!(!debounce.observe(start, true));
        assert!(!debounce.observe(start + POLL_INTERVAL, true));

        // The tree has been stable for a full debounce window.
        assert!(debounce.observe(start + POLL_INTERVAL * 2, false));
        // And not a second time for the same burst.
        assert!(!debounce.observe(start + POLL_INTERVAL * 3, false));
    }

    #[test]
    fn a_lone_change_waits_out_the_debounce() {
        let start = Instant::now();
        let mut debounce = Debounce::new();

        assert!(!debounce.observe(start, true));
        // Stable, but not yet for long enough.
        assert!(!debounce.observe(start + Duration::from_millis(100), false));
        assert!(debounce.observe(start + DEBOUNCE, false));
    }

    #[test]
    fn a_second_save_after_a_reload_is_a_second_reload() {
        let start = Instant::now();
        let mut debounce = Debounce::new();

        assert!(!debounce.observe(start, true));
        assert!(debounce.observe(start + DEBOUNCE, false));

        let later = start + Duration::from_secs(5);
        assert!(!debounce.observe(later, true));
        assert!(debounce.observe(later + DEBOUNCE, false));
    }

    #[test]
    fn an_idle_tree_never_fires() {
        let start = Instant::now();
        let mut debounce = Debounce::new();
        for tick in 0..20 {
            assert!(!debounce.observe(start + POLL_INTERVAL * tick, false));
        }
    }
}
