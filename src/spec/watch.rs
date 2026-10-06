//! Watching a `.dash` file for edits made outside the window.
//!
//! A file stamp polled on a GPUI timer, with a debounce so a save that
//! rewrites the file several times at once is one reload rather than several.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How often the file is stamped.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How long the file has to sit still before a change is acted on. An editor
/// that writes through a rename lands inside one window.
pub const DEBOUNCE: Duration = Duration::from_millis(200);

/// One file's size and modification time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub path: PathBuf,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

impl FileStamp {
    /// Stamp one file. A missing or unreadable file stamps as `len` zero and
    /// no mtime, so it appearing later is a change like any other.
    pub fn capture(path: &Path) -> Self {
        let metadata = std::fs::metadata(path).ok();
        Self {
            path: path.to_path_buf(),
            len: metadata.as_ref().map(|data| data.len()).unwrap_or(0),
            modified: metadata.and_then(|data| data.modified().ok()),
        }
    }
}

/// Turns a stream of "did the stamp change" answers into one reload.
///
/// Reports `true` once the file has been unchanged for [`DEBOUNCE`] since the
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
    fn a_single_file_stamps_and_changes() {
        let dir = TempDir::new("filestamp");
        dir.write("spec.dash", "plot \"p\" {}");

        let before = FileStamp::capture(&dir.path().join("spec.dash"));
        assert_eq!(before.len, 11);
        assert_eq!(before, FileStamp::capture(&dir.path().join("spec.dash")));

        dir.write("spec.dash", "plot \"p\" { x = 1 }");
        assert_ne!(before, FileStamp::capture(&dir.path().join("spec.dash")));

        // A missing file stamps as zero-length without failing.
        let gone = FileStamp::capture(&dir.path().join("gone.dash"));
        assert_eq!(gone.len, 0);
        assert_eq!(gone.modified, None);
    }

    #[test]
    fn one_burst_of_changes_fires_one_reload() {
        let start = Instant::now();
        let mut debounce = Debounce::new();

        // The change itself is not the reload: the writes are still landing.
        assert!(!debounce.observe(start, true));
        assert!(!debounce.observe(start + POLL_INTERVAL, true));

        // The file has been stable for a full debounce window.
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
    fn an_idle_file_never_fires() {
        let start = Instant::now();
        let mut debounce = Debounce::new();
        for tick in 0..20 {
            assert!(!debounce.observe(start + POLL_INTERVAL * tick, false));
        }
    }
}
