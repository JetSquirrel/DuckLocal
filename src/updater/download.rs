//! Fetching a package, checked as it arrives: no more bytes than its manifest
//! says, and the SHA-256 the manifest gives. A package that fails either is
//! deleted, never installed.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::manifest::{self, Asset};

/// Download `asset` into `dir`, calling `progress` with the bytes so far.
/// A copy already there that checks out is used as is. Blocking — call it
/// off the UI thread.
pub fn fetch(asset: &Asset, dir: &Path, progress: impl Fn(u64)) -> anyhow::Result<PathBuf> {
    if !manifest::is_trusted(&asset.url) {
        anyhow::bail!(
            "Refusing to download {} from outside dl.ducklocal.app",
            asset.url
        );
    }
    let path = dir.join(asset.file_name());
    if path.exists() {
        if check(&path, asset).is_ok() {
            progress(asset.size);
            return Ok(path);
        }
        std::fs::remove_file(&path)?;
    }
    let partial = dir.join(format!("{}.part", asset.file_name()));
    let result = (|| -> anyhow::Result<()> {
        let mut response = manifest::agent().get(&asset.url).call()?;
        let mut body = response.body_mut().as_reader().take(asset.size + 1);
        let mut file = std::fs::File::create(&partial)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 64 * 1024];
        let mut done = 0u64;
        loop {
            let read = body.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            file.write_all(&buffer[..read])?;
            hasher.update(&buffer[..read]);
            done += read as u64;
            progress(done.min(asset.size));
        }
        file.sync_all()?;
        verify(done, &hex(&hasher.finalize()), asset)
    })();
    match result {
        Ok(()) => {
            std::fs::rename(&partial, &path)?;
            Ok(path)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&partial);
            Err(error)
        }
    }
}

/// Whether the file at `path` is `asset`, byte for byte.
fn check(path: &Path, asset: &Asset) -> anyhow::Result<()> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let size = std::io::copy(&mut file, &mut hasher)?;
    verify(size, &hex(&hasher.finalize()), asset)
}

fn verify(size: u64, sha256: &str, asset: &Asset) -> anyhow::Result<()> {
    if size != asset.size {
        anyhow::bail!(
            "{} is {size} bytes, not the {} the release lists",
            asset.file_name(),
            asset.size
        );
    }
    if !sha256.eq_ignore_ascii_case(&asset.sha256) {
        anyhow::bail!("{} does not match the release's SHA-256", asset.file_name());
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Delete every package in `dir` but `keep`: an older download is never
/// installed once a newer one is around.
pub fn prune(dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path != keep && path.is_file() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(size: u64, sha256: &str) -> Asset {
        Asset {
            os: "macos".into(),
            arch: "aarch64".into(),
            format: "dmg".into(),
            min_os: None,
            url: "https://dl.ducklocal.app/v1/ducklocal-macos-arm64.dmg".into(),
            size,
            sha256: sha256.into(),
        }
    }

    #[test]
    fn a_package_must_match_its_size_and_digest() {
        let dir = std::env::temp_dir().join(format!("ducklocal-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.dmg");
        std::fs::write(&path, b"duck").unwrap();
        let sha = hex(&Sha256::digest(b"duck"));
        assert!(check(&path, &asset(4, &sha)).is_ok());
        assert!(check(&path, &asset(4, &sha.to_uppercase())).is_ok());
        assert!(check(&path, &asset(5, &sha)).is_err(), "wrong size");
        assert!(
            check(&path, &asset(4, &"0".repeat(64))).is_err(),
            "wrong digest"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn nothing_is_fetched_from_another_host() {
        let mut off = asset(4, &"0".repeat(64));
        off.url = "https://github.com/x.dmg".into();
        let error = fetch(&off, &std::env::temp_dir(), |_| {}).unwrap_err();
        assert!(error.to_string().contains("dl.ducklocal.app"));
    }
}
