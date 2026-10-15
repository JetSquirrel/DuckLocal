//! The release feed: `https://dl.ducklocal.app/latest.json`, written by
//! `scripts/mirror-to-r2.sh` after every package of a release is uploaded.
//!
//! ```json
//! {
//!   "version": "0.2.4",
//!   "notes_url": "https://github.com/JetSquirrel/DuckLocal/releases/tag/v0.2.4",
//!   "assets": [
//!     { "os": "macos", "arch": "aarch64", "format": "dmg", "min_os": "12.0",
//!       "url": "https://dl.ducklocal.app/v0.2.4/ducklocal-macos-arm64.dmg",
//!       "size": 25846997, "sha256": "…" }
//!   ]
//! }
//! ```

use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;

/// The feed of the latest stable release.
pub const FEED: &str = "https://dl.ducklocal.app/latest.json";
/// Every URL the updater fetches starts with this.
const TRUSTED: &str = "https://dl.ducklocal.app/";
/// A feed is a few hundred bytes; anything this large is not one.
const FEED_LIMIT: u64 = 256 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes_url: Option<String>,
    pub assets: Vec<Asset>,
}

/// One package of a release, for one platform.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Asset {
    /// `macos`, `linux` or `windows`, as `std::env::consts::OS` says it.
    pub os: String,
    /// `aarch64` or `x86_64`, as `std::env::consts::ARCH` says it.
    pub arch: String,
    /// `dmg`, `tar.gz` or `zip`.
    pub format: String,
    #[serde(default)]
    pub min_os: Option<String>,
    pub url: String,
    pub size: u64,
    /// Lowercase hex.
    pub sha256: String,
}

impl Asset {
    /// The file name the package is kept under.
    pub fn file_name(&self) -> &str {
        self.url.rsplit('/').next().unwrap_or("package")
    }
}

impl Manifest {
    /// The package for `os` on `arch`, exactly: an x86_64 package is never
    /// offered to an arm64 Mac, nor a Linux one to Windows. A package that is
    /// not on dl.ducklocal.app is not offered at all.
    pub fn asset_for(&self, os: &str, arch: &str) -> Option<&Asset> {
        self.assets
            .iter()
            .find(|asset| asset.os == os && asset.arch == arch && is_trusted(&asset.url))
    }

    /// The package for this machine.
    pub fn asset_here(&self) -> Option<&Asset> {
        self.asset_for(std::env::consts::OS, std::env::consts::ARCH)
    }
}

/// Whether `url` is one the updater may fetch.
pub fn is_trusted(url: &str) -> bool {
    url.starts_with(TRUSTED) && !url[TRUSTED.len()..].contains("..")
}

/// The HTTP client for the feed and the packages: HTTPS only, no redirects
/// (a redirect could only lead off the trusted host), a connect timeout, and
/// a response timeout. A package may take a while; the read of its body is
/// bounded by its size instead (see `download`).
pub fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .user_agent(concat!(
                "DuckLocal/",
                env!("CARGO_PKG_VERSION"),
                " (+https://ducklocal.app)"
            ))
            .build()
            .into()
    })
}

/// Read the feed. Blocking — call it off the UI thread.
pub fn fetch() -> anyhow::Result<Manifest> {
    let body = agent()
        .get(FEED)
        .call()?
        .body_mut()
        .with_config()
        .limit(FEED_LIMIT)
        .read_to_string()?;
    parse(&body)
}

pub fn parse(body: &str) -> anyhow::Result<Manifest> {
    let manifest: Manifest = serde_json::from_str(body)
        .map_err(|error| anyhow::anyhow!("The update feed does not read: {error}"))?;
    for asset in &manifest.assets {
        let hex = asset.sha256.len() == 64 && asset.sha256.bytes().all(|b| b.is_ascii_hexdigit());
        if !hex {
            anyhow::bail!("The update feed lists {} without a SHA-256", asset.url);
        }
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED_TEXT: &str = r#"{
      "version": "0.3.0",
      "notes_url": "https://github.com/JetSquirrel/DuckLocal/releases/tag/v0.3.0",
      "assets": [
        {"os": "macos", "arch": "aarch64", "format": "dmg", "min_os": "12.0",
         "url": "https://dl.ducklocal.app/v0.3.0/ducklocal-macos-arm64.dmg",
         "size": 10, "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
        {"os": "linux", "arch": "x86_64", "format": "tar.gz",
         "url": "https://example.com/ducklocal-linux-x86_64.tar.gz",
         "size": 10, "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}
      ]
    }"#;

    #[test]
    fn a_package_matches_its_platform_exactly_and_on_the_trusted_host() {
        let manifest = parse(FEED_TEXT).unwrap();
        let mac = manifest.asset_for("macos", "aarch64").unwrap();
        assert_eq!(mac.file_name(), "ducklocal-macos-arm64.dmg");
        assert!(
            manifest.asset_for("macos", "x86_64").is_none(),
            "no near-miss arch"
        );
        assert!(manifest.asset_for("windows", "x86_64").is_none());
        assert!(
            manifest.asset_for("linux", "x86_64").is_none(),
            "a package off dl.ducklocal.app is never offered"
        );
    }

    #[test]
    fn only_dl_ducklocal_app_is_trusted() {
        assert!(is_trusted("https://dl.ducklocal.app/v1/x.dmg"));
        assert!(!is_trusted("http://dl.ducklocal.app/v1/x.dmg"));
        assert!(!is_trusted("https://dl.ducklocal.app.evil.com/x.dmg"));
        assert!(!is_trusted(
            "https://github.com/JetSquirrel/DuckLocal/x.dmg"
        ));
        assert!(!is_trusted("https://dl.ducklocal.app/../x.dmg"));
    }

    #[test]
    fn a_feed_without_digests_is_refused() {
        let bad = FEED_TEXT.replace(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "nope",
        );
        assert!(parse(&bad).is_err());
        assert!(parse("<html>").is_err());
    }
}
