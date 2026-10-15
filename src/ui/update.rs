//! Keeping DuckLocal up to date, from dl.ducklocal.app (see `updater`).
//!
//! Nothing is fetched unless asked: a click on the version in the status bar,
//! the menu's "Check for Updates…", or — once someone turns it on, like the
//! online base map — at launch, the `update_check` setting being `on`.
//!
//! A check that finds a newer release goes on by itself, in the background:
//! the package for this machine downloads with its progress in the status
//! bar, is verified and staged beside the installed copy, and the status bar
//! then offers "Restart to update". A copy that cannot be replaced in place
//! offers the package's download link instead. Nothing replaces the running
//! version until the person clicks restart, and every step that fails stops
//! there, with the installed version untouched.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::{App, Global};

use crate::updater::install::{self, Installation, Reason, Staged};
use crate::updater::{download, manifest};

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Status {
    /// Not checked since launch.
    #[default]
    Idle,
    Checking,
    UpToDate,
    Downloading {
        version: String,
        done: u64,
        total: u64,
    },
    /// Verifying the package and staging it beside the installed copy.
    Preparing {
        version: String,
    },
    /// Staged: a restart puts it in place.
    Ready {
        version: String,
        staged: Staged,
    },
    /// Newer, but this copy cannot be replaced in place: its package's link.
    Manual {
        version: String,
        url: String,
        reason: Reason,
    },
    /// Newer, but the release has no package for this machine.
    NoPackage {
        version: String,
    },
    Failed(String),
}

impl Status {
    /// Work under way, which another check must not start over.
    fn is_busy(&self) -> bool {
        matches!(
            self,
            Status::Checking | Status::Downloading { .. } | Status::Preparing { .. }
        )
    }
}

#[derive(Default)]
struct Update {
    status: Status,
    /// `None` until read from the history store.
    at_launch: Option<bool>,
}

impl Global for Update {}

pub fn status(cx: &App) -> Status {
    cx.try_global::<Update>()
        .map(|update| update.status.clone())
        .unwrap_or_default()
}

/// Whether the check runs at launch: only once someone has turned it on.
pub fn at_launch(cx: &mut App) -> bool {
    *cx.default_global::<Update>()
        .at_launch
        .get_or_insert_with(|| {
            crate::history::get_setting("update_check")
                .ok()
                .flatten()
                .is_some_and(|value| value == "on")
        })
}

/// Turn the launch check on or off and remember the choice. Turning it on
/// checks now, rather than at the next launch.
pub fn toggle_at_launch(cx: &mut App) {
    let on = !at_launch(cx);
    cx.default_global::<Update>().at_launch = Some(on);
    if let Err(error) = crate::history::set_setting("update_check", if on { "on" } else { "off" }) {
        tracing::warn!("update_check not saved: {error:#}");
    }
    if on {
        check(cx);
    }
}

/// The launch half: check if the setting says to.
pub fn on_launch(cx: &mut App) {
    if at_launch(cx) {
        check(cx);
    }
}

/// Look for a newer release and, when there is one, fetch and stage it —
/// all off the UI thread; every window redraws as it goes. Work under way,
/// or an update already staged, is not started over.
pub fn check(cx: &mut App) {
    let current = status(cx);
    if current.is_busy() || matches!(current, Status::Ready { .. }) {
        return;
    }
    set(Status::Checking, cx);
    cx.spawn(async move |cx| {
        let manifest = match smol::unblock(manifest::fetch).await {
            Ok(manifest) => manifest,
            Err(error) => {
                tracing::warn!("update check failed: {error:#}");
                cx.update(|cx| set(Status::Failed(error.to_string()), cx));
                return;
            }
        };
        let version = manifest.version.clone();
        if !is_newer(&version, CURRENT) {
            cx.update(|cx| set(Status::UpToDate, cx));
            return;
        }
        let Some(asset) = manifest.asset_here().cloned() else {
            cx.update(|cx| set(Status::NoPackage { version }, cx));
            return;
        };
        let installation = install::installation();
        if let Installation::Manual(reason) = installation {
            let url = asset.url.clone();
            cx.update(|cx| {
                set(
                    Status::Manual {
                        version,
                        url,
                        reason,
                    },
                    cx,
                )
            });
            return;
        }

        // Download, with the bytes so far read off a counter twice a second.
        let done = Arc::new(AtomicU64::new(0));
        let total = asset.size;
        cx.update(|cx| {
            set(
                Status::Downloading {
                    version: version.clone(),
                    done: 0,
                    total,
                },
                cx,
            )
        });
        let counter = done.clone();
        let fetching = {
            let asset = asset.clone();
            smol::unblock(move || -> anyhow::Result<std::path::PathBuf> {
                let dir = crate::updater::cache_dir()?;
                let package = download::fetch(&asset, &dir, |bytes| {
                    counter.store(bytes, Ordering::Relaxed)
                })?;
                download::prune(&dir, &package);
                Ok(package)
            })
        };
        let fetching = smol::spawn(fetching);
        while !fetching.is_finished() {
            smol::Timer::after(Duration::from_millis(500)).await;
            let bytes = done.load(Ordering::Relaxed);
            let version = version.clone();
            cx.update(|cx| {
                if matches!(status(cx), Status::Downloading { done, .. } if done != bytes) {
                    set(
                        Status::Downloading {
                            version,
                            done: bytes,
                            total,
                        },
                        cx,
                    );
                }
            });
        }
        let package = match fetching.await {
            Ok(package) => package,
            Err(error) => {
                tracing::warn!("update download failed: {error:#}");
                cx.update(|cx| set(Status::Failed(error.to_string()), cx));
                return;
            }
        };

        cx.update(|cx| {
            set(
                Status::Preparing {
                    version: version.clone(),
                },
                cx,
            )
        });
        let staging = {
            let version = version.clone();
            smol::unblock(move || install::stage(&package, &installation, &version))
        };
        let status = match staging.await {
            Ok(staged) => Status::Ready { version, staged },
            Err(error) => {
                tracing::warn!("update not staged: {error:#}");
                Status::Failed(format!("{error:#}"))
            }
        };
        cx.update(|cx| set(status, cx));
    })
    .detach();
}

/// Put the staged version in place and quit; it opens once this process has
/// exited. A swap that fails leaves the installed version where it was.
pub fn restart(cx: &mut App) {
    let Status::Ready { staged, .. } = status(cx) else {
        return;
    };
    match install::apply(&staged) {
        Ok(()) => cx.quit(),
        Err(error) => {
            tracing::warn!("update not applied: {error:#}");
            set(Status::Failed(format!("{error:#}")), cx);
        }
    }
}

fn set(status: Status, cx: &mut App) {
    cx.default_global::<Update>().status = status;
    cx.refresh_windows();
}

/// Whether release `tag` is later than version `current`: numerically,
/// part by part, a `v` prefix and any `-suffix` ignored.
pub fn is_newer(tag: &str, current: &str) -> bool {
    fn parts(version: &str) -> Vec<u64> {
        let version = version.trim().trim_start_matches(['v', 'V']);
        let version = version.split(['-', '+']).next().unwrap_or(version);
        version
            .split('.')
            .map(|part| {
                let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
                digits.parse().unwrap_or(0)
            })
            .collect()
    }
    let (mut tag, mut current) = (parts(tag), parts(current));
    let len = tag.len().max(current.len());
    tag.resize(len, 0);
    current.resize(len, 0);
    tag > current
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(is_newer("v0.3.0", "0.2.0"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.99.1"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
        assert!(!is_newer("0.2", "0.2.0"));
        assert!(!is_newer("v0.1.9", "0.2.0"));
        assert!(!is_newer("v0.2.0-beta.1", "0.2.0"));
    }
}
