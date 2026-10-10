//! Checking GitHub for a newer release.
//!
//! The one request is the GitHub API's latest-release lookup, and it is only
//! made when asked: a click on the version in the status bar, the menu's
//! "Check for Updates…", or — once someone turns it on, like the online base
//! map — at launch, the `update_check` setting being `on`. Nothing is
//! downloaded or installed: a newer release shows in the status bar as a
//! link to its download page.

use std::sync::OnceLock;
use std::time::Duration;

use gpui_kit::{App, Global};

/// Where a newer release is downloaded from.
pub const RELEASES_URL: &str = "https://github.com/JetSquirrel/DuckLocal/releases/latest";
const LATEST_API: &str = "https://api.github.com/repos/JetSquirrel/DuckLocal/releases/latest";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Status {
    /// Not checked since launch.
    #[default]
    Idle,
    Checking,
    UpToDate,
    /// A newer release, by its tag (`v0.3.0`).
    Available(String),
    Failed(String),
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
    if let Err(error) = crate::history::set_setting("update_check", if on { "on" } else { "off" })
    {
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

/// Ask GitHub for the latest release, off the UI thread; every window
/// redraws with the answer. A check in flight is not started twice.
pub fn check(cx: &mut App) {
    if status(cx) == Status::Checking {
        return;
    }
    set(Status::Checking, cx);
    cx.spawn(async move |cx| {
        let status = match smol::unblock(latest_tag).await {
            Ok(tag) if is_newer(&tag, CURRENT) => Status::Available(tag),
            Ok(_) => Status::UpToDate,
            Err(error) => {
                tracing::warn!("update check failed: {error:#}");
                Status::Failed(error.to_string())
            }
        };
        cx.update(|cx| set(status, cx));
    })
    .detach();
}

fn set(status: Status, cx: &mut App) {
    cx.default_global::<Update>().status = status;
    cx.refresh_windows();
}

fn latest_tag() -> anyhow::Result<String> {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .user_agent(concat!(
                "DuckLocal/",
                env!("CARGO_PKG_VERSION"),
                " (+https://ducklocal.app)"
            ))
            .build()
            .into()
    });
    let body = agent
        .get(LATEST_API)
        .header("Accept", "application/vnd.github+json")
        .call()?
        .body_mut()
        .with_config()
        .limit(1024 * 1024)
        .read_to_string()?;
    let release: serde_json::Value = serde_json::from_str(&body)?;
    release["tag_name"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("the latest release has no tag"))
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
