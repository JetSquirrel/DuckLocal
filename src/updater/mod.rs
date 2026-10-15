//! Updating DuckLocal in place, the way most desktop apps do: find a newer
//! release, download its package for this machine in the background, verify
//! it, and swap it in when the person restarts.
//!
//! ```text
//! src/updater/manifest.rs  dl.ducklocal.app/latest.json: what the latest release is
//! src/updater/download.rs  fetching a package, checked against its size and SHA-256
//! src/updater/install.rs   where this copy is installed, staging and swapping it
//! ```
//!
//! Everything comes from dl.ducklocal.app, never GitHub, which many people in
//! mainland China cannot reach at a usable speed. The feed and the packages
//! are fetched over HTTPS from that host only, with no redirects followed. A
//! package must match its manifest's size and SHA-256; on macOS the new app
//! must also carry a valid signature from the same team and bundle id as the
//! running one. Any check that fails stops the update before anything on
//! disk is replaced.
//!
//! A copy that cannot be replaced — a development build, an app macOS runs
//! from a translocated copy, a read-only folder — is told where to download
//! the package instead. The UI half, and the state the status bar shows, is
//! `ui::update`. Nothing here runs unless the person asks for a check or has
//! turned the launch check on.

pub mod download;
pub mod install;
pub mod manifest;

use std::path::PathBuf;

/// Where downloaded packages are kept until they are installed: beside the
/// history store, one file per release.
pub fn cache_dir() -> anyhow::Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "DuckLocal")
        .ok_or_else(|| anyhow::anyhow!("Cannot resolve the app data directory"))?;
    let dir = dirs.data_dir().join("updates");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
