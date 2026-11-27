//! Updating DuckLocal from dl.ducklocal.app.
//!
//! The check is one request for `latest/latest.json` on dl.ducklocal.app, the
//! R2 mirror the release workflow fills (GitHub is slow or unreachable for
//! many users in mainland China). It is only made when asked: a click on the
//! version in the status bar, the menu's "Check for Updates…", or — once
//! someone turns it on, like the online base map — at launch, the
//! `update_check` setting being `on`.
//!
//! A newer release is downloaded right away, checked against the manifest's
//! SHA-256 (and on macOS against the running app's signing team, on Windows
//! against the PE64 GUI header), and unpacked beside the cache. It replaces
//! the installed app when DuckLocal quits or restarts: at once on "Restart to
//! update" (after guarding unsaved in-memory tables and views), else on the
//! next quit for directly writable installs. Where the app cannot replace
//! itself — a development build, an unwritable location on Unix, or a
//! platform with no package — the status bar links to the package on
//! dl.ducklocal.app instead.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use gpui_kit::{App, Global, Subscription};
use sha2::{Digest, Sha256};

const DOWNLOAD_HOST: &str = "https://dl.ducklocal.app";
const MANIFEST_URL: &str = "https://dl.ducklocal.app/latest/latest.json";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Status {
    /// Not checked since launch.
    #[default]
    Idle,
    Checking,
    UpToDate,
    /// A newer release this copy cannot install itself: the status bar
    /// links to its package. `url` is `None` on a platform with no package.
    Available {
        tag: String,
        url: Option<String>,
    },
    /// A newer release being downloaded and unpacked.
    Downloading {
        tag: String,
        downloaded: u64,
        total: Option<u64>,
    },
    /// A newer release unpacked and waiting for DuckLocal to quit.
    Ready(String),
    /// Putting the downloaded release in place before restarting.
    Installing(String),
    Failed(String),
}

#[derive(Default)]
struct Update {
    status: Status,
    /// `None` until read from the history store.
    at_launch: Option<bool>,
    /// Installs or relaunches the downloaded release when DuckLocal quits.
    _on_quit: Option<Subscription>,
}

impl Global for Update {}

/// How the installed copy can be replaced in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InstallMode {
    /// Target directory is directly writable by the current user.
    Direct,
    /// Windows only: target directory exists but requires UAC elevation
    /// (for example `C:\Program Files\DuckLocal\`).
    #[cfg_attr(not(windows), allow(dead_code))]
    Elevated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InstallTarget {
    path: PathBuf,
    #[cfg_attr(not(windows), allow(dead_code))]
    mode: InstallMode,
}

/// An unpacked release: its version tag, the new app (macOS) or executable,
/// and the installed target it replaces.
#[derive(Clone)]
struct Staged {
    tag: String,
    new: PathBuf,
    target: InstallTarget,
}

/// What `latest/latest.json` lists: the release, and each package's digest
/// and size.
#[derive(serde::Deserialize)]
struct Manifest {
    version: String,
    packages: std::collections::HashMap<String, Package>,
}

#[derive(serde::Deserialize, Clone)]
struct Package {
    sha256: String,
    size: u64,
}

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

/// Ask dl.ducklocal.app for the latest release, off the UI thread, and
/// download a newer one; every window redraws as it goes. A check or
/// download in flight is not started twice, and a downloaded release is not
/// fetched again.
pub fn check(cx: &mut App) {
    if matches!(
        status(cx),
        Status::Checking | Status::Downloading { .. } | Status::Ready(_) | Status::Installing(_)
    ) {
        return;
    }
    set(Status::Checking, cx);
    cx.spawn(async move |cx| {
        let manifest = match smol::unblock(fetch_manifest).await {
            Ok(manifest) => manifest,
            Err(error) => {
                tracing::warn!("update check failed: {error:#}");
                cx.update(|cx| set(Status::Failed(error.to_string()), cx));
                return;
            }
        };
        let tag = manifest.version.clone();
        if !is_newer(&tag, CURRENT) {
            cx.update(|cx| set(Status::UpToDate, cx));
            return;
        }
        let package = PACKAGE.and_then(|name| Some((name, manifest.packages.get(name)?.clone())));
        let (Some((name, package)), Some(target)) = (package, install_target()) else {
            let url = PACKAGE.map(|name| package_url(&tag, name));
            cx.update(|cx| set(Status::Available { tag, url }, cx));
            return;
        };
        let total = Some(package.size);
        cx.update(|cx| {
            set(
                Status::Downloading {
                    tag: tag.clone(),
                    downloaded: 0,
                    total,
                },
                cx,
            );
        });
        let (progress_tx, progress_rx) = smol::channel::unbounded::<u64>();
        let stage_task = {
            let tag = tag.clone();
            smol::unblock(move || {
                let result = stage(&tag, name, &package, target, |downloaded| {
                    let _ = progress_tx.try_send(downloaded);
                });
                drop(progress_tx);
                result
            })
        };
        while let Ok(downloaded) = progress_rx.recv().await {
            cx.update(|cx| {
                set(
                    Status::Downloading {
                        tag: tag.clone(),
                        downloaded,
                        total,
                    },
                    cx,
                );
            });
        }
        let staged = stage_task.await;
        cx.update(|cx| match staged {
            Ok(staged) => {
                *staged_release() = Some(staged);
                ensure_on_quit(cx);
                set(Status::Ready(tag.clone()), cx);
                crate::ui::notify_update_ready(tag, cx);
            }
            Err(error) => {
                tracing::warn!("update download failed: {error:#}");
                set(Status::Failed(error.to_string()), cx);
            }
        });
    })
    .detach();
}

/// Quit, put the downloaded release in place, and open it.
///
/// Guards unsaved in-memory tables and views via `save_database::restart_for_update`
/// before proceeding to [`execute_restart`].
pub fn restart(cx: &mut App) {
    if matches!(status(cx), Status::Installing(_)) {
        return;
    }
    if staged_release().is_some() {
        crate::ui::save_database::restart_for_update(cx);
    }
}

/// Complete a restart once unsaved work has been saved, discarded, or absent.
#[cfg(windows)]
pub(crate) fn execute_restart(cx: &mut App) {
    if matches!(status(cx), Status::Installing(_)) {
        return;
    }
    let Some(staged) = staged_release().take() else {
        return;
    };
    ensure_on_quit(cx);
    let tag = staged.tag.clone();
    set(Status::Installing(tag.clone()), cx);

    cx.spawn(async move |cx| {
        let staged_for_swap = staged.clone();
        let outcome = smol::unblock(move || apply_windows_staged(&staged_for_swap)).await;
        cx.update(|cx| match outcome {
            Ok(WindowsApplyOutcome::Applied) => {
                *relaunch_target() = Some(staged.target.path.clone());
                // `STAGED` is already cleared, so `on_app_quit` will only close
                // databases and relaunch the swapped binary.
                cx.quit();
            }
            Ok(WindowsApplyOutcome::Cancelled) => {
                *staged_release() = Some(staged);
                set(Status::Ready(tag), cx);
            }
            Err(error) => {
                tracing::warn!("installing the update failed: {error:#}");
                let message = error.to_string();
                *staged_release() = Some(staged);
                set(Status::Failed(message.clone()), cx);
                crate::ui::notify_update_failed(message, cx);
            }
        });
    })
    .detach();
}

/// Complete a restart once unsaved work has been saved, discarded, or absent.
#[cfg(not(windows))]
pub(crate) fn execute_restart(cx: &mut App) {
    if matches!(status(cx), Status::Installing(_)) {
        return;
    }
    if let Some(tag) = staged_release().as_ref().map(|staged| staged.tag.clone()) {
        ensure_on_quit(cx);
        set(Status::Installing(tag), cx);
        // The quit hook installs it; only this way does it reopen.
        RELAUNCH.store(true, Ordering::Relaxed);
        cx.quit();
    }
}

fn ensure_on_quit(cx: &mut App) {
    if cx.default_global::<Update>()._on_quit.is_none() {
        let on_quit = cx.on_app_quit(|_| {
            install();
            async {}
        });
        cx.default_global::<Update>()._on_quit = Some(on_quit);
    }
}

#[cfg(not(windows))]
static RELAUNCH: AtomicBool = AtomicBool::new(false);

/// The downloaded release, once unpacked; the quit hook or restart flow takes it.
fn staged_release() -> std::sync::MutexGuard<'static, Option<Staged>> {
    static STAGED: Mutex<Option<Staged>> = Mutex::new(None);
    STAGED.lock().unwrap_or_else(|error| error.into_inner())
}

/// On Windows, the already-swapped executable path to relaunch from `on_app_quit`.
#[cfg(windows)]
fn relaunch_target() -> std::sync::MutexGuard<'static, Option<PathBuf>> {
    static TARGET: Mutex<Option<PathBuf>> = Mutex::new(None);
    TARGET.lock().unwrap_or_else(|error| error.into_inner())
}

fn set(status: Status, cx: &mut App) {
    cx.default_global::<Update>().status = status;
    cx.refresh_windows();
}

/// The package this build updates from, as the release names it.
const PACKAGE: Option<&str> = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some("ducklocal-macos-arm64.dmg")
} else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some("ducklocal-linux-x86_64.tar.gz")
} else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
    Some("ducklocal-windows-x86_64.zip")
} else {
    None
};

/// A release's package on dl.ducklocal.app. The versioned copy, which never
/// changes, rather than `latest/`, which a newer release can replace mid
/// download.
fn package_url(tag: &str, name: &str) -> String {
    format!("{DOWNLOAD_HOST}/{tag}/{name}")
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        // No overall timeout: a package is tens of megabytes on whatever
        // line the user has. Connecting and the first byte still give up.
        ureq::Agent::config_builder()
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

fn fetch_manifest() -> anyhow::Result<Manifest> {
    let body = agent()
        .get(MANIFEST_URL)
        .call()?
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_string()?;
    Ok(serde_json::from_str(&body)?)
}

/// Strip a Windows `\\?\` verbatim path prefix (and map `\\?\UNC\server\share`
/// back to `\\server\share`) so paths passed to Win32 APIs and relaunch
/// commands stay standard Win32 paths.
fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{unc}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}

fn current_executable() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if cfg!(windows) {
        // Avoid `canonicalize()` on Windows: it yields `\\?\C:\…` verbatim
        // paths and can fail on mapped/virtual drives.
        Some(strip_verbatim_prefix(exe))
    } else {
        exe.canonicalize().ok()
    }
}

/// What a release replaces: the `.app` bundle the executable sits in on
/// macOS, the executable elsewhere — `None` when this copy cannot replace
/// itself. A development build never does: `cargo run` would overwrite
/// `target/` with a release.
fn install_target() -> Option<InstallTarget> {
    if cfg!(debug_assertions) {
        return None;
    }
    let exe = current_executable()?;
    classify_install_target(exe)
}

fn classify_install_target(exe: PathBuf) -> Option<InstallTarget> {
    let target = if cfg!(target_os = "macos") {
        exe.ancestors()
            .find(|path| path.extension().is_some_and(|ext| ext == "app"))?
            .to_path_buf()
    } else {
        exe
    };
    let dir = target.parent()?;
    if !dir.is_dir() {
        return None;
    }
    let probe = dir.join(format!(".ducklocal-update-probe-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(file) => {
            drop(file);
            let _ = std::fs::remove_file(&probe);
            Some(InstallTarget {
                path: target,
                mode: InstallMode::Direct,
            })
        }
        Err(error) if cfg!(windows) && error.kind() == std::io::ErrorKind::PermissionDenied => {
            // Windows install under a protected folder (e.g. `Program Files`):
            // auto-update still runs, elevating only the swap helper via UAC.
            Some(InstallTarget {
                path: target,
                mode: InstallMode::Elevated,
            })
        }
        Err(_) => None,
    }
}

/// Where downloaded releases are unpacked.
fn updates_dir() -> anyhow::Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "DuckLocal")
        .ok_or_else(|| anyhow::anyhow!("no cache directory"))?;
    Ok(dirs.cache_dir().join("updates"))
}

/// Download package `name` of release `tag`, check it, and unpack the app
/// or executable that replaces `target`.
fn stage(
    tag: &str,
    name: &str,
    package: &Package,
    target: InstallTarget,
    on_progress: impl FnMut(u64),
) -> anyhow::Result<Staged> {
    let dir = updates_dir()?.join(tag);
    // A half-finished earlier attempt is started over.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(name);
    download(&package_url(tag, name), package, &file, on_progress)?;
    let new = unpack(&file, &dir)?;
    verify(&new, &target.path)?;
    Ok(Staged {
        tag: tag.to_string(),
        new,
        target,
    })
}

/// Fetch `url` into `path`, failing unless it is `package`'s size and hash.
/// Reports progress to `on_progress` throttled to roughly 100ms or >= 1%.
fn download(
    url: &str,
    package: &Package,
    path: &Path,
    mut on_progress: impl FnMut(u64),
) -> anyhow::Result<()> {
    let mut response = agent().get(url).call()?;
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(package.size + 1)
        .reader();
    let mut out = std::fs::File::create(path)?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut last_reported_bytes = 0u64;
    let mut last_reported_at = Instant::now();
    let min_step = (package.size / 100).max(1);
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        out.write_all(&buffer[..read])?;
        size += read as u64;
        if size.saturating_sub(last_reported_bytes) >= min_step
            || last_reported_at.elapsed() >= Duration::from_millis(100)
        {
            last_reported_bytes = size;
            last_reported_at = Instant::now();
            on_progress(size);
        }
    }
    if size != last_reported_bytes {
        on_progress(size);
    }
    out.sync_all()?;
    anyhow::ensure!(
        size == package.size,
        "the download is {size} bytes, not {}",
        package.size
    );
    let digest = format!("{:x}", hasher.finalize());
    anyhow::ensure!(
        digest.eq_ignore_ascii_case(&package.sha256),
        "the download does not match its checksum"
    );
    Ok(())
}

/// The app (macOS) or executable inside downloaded package `file`, copied
/// out into `dir`.
///
/// The macOS tools here, and the ones `verify` and `install_staged` run, go
/// by absolute path: an app started from a terminal inherits that shell's
/// PATH, and an update that checks its own signature must not run whatever
/// `codesign` comes first on it.
#[cfg(target_os = "macos")]
fn unpack(file: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
    let mount = dir.join("mount");
    std::fs::create_dir_all(&mount)?;
    run(std::process::Command::new("/usr/bin/hdiutil")
        .args([
            "attach",
            "-nobrowse",
            "-readonly",
            "-noautoopen",
            "-mountpoint",
        ])
        .arg(&mount)
        .arg(file))?;
    let app = dir.join("DuckLocal.app");
    let copied = run(std::process::Command::new("/usr/bin/ditto")
        .arg(mount.join("DuckLocal.app"))
        .arg(&app));
    let _ = run(std::process::Command::new("/usr/bin/hdiutil")
        .args(["detach", "-force"])
        .arg(&mount));
    copied?;
    Ok(app)
}

#[cfg(windows)]
fn unpack(file: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
    unpack_windows_zip(file, dir)
}

#[cfg(all(not(target_os = "macos"), not(windows)))]
fn unpack(file: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
    let out = dir.join("unpacked");
    std::fs::create_dir_all(&out)?;
    run(std::process::Command::new("tar")
        .arg("-xf")
        .arg(file)
        .arg("-C")
        .arg(&out))?;
    let exe = out.join("ducklocal-linux-x86_64").join("ducklocal");
    anyhow::ensure!(exe.is_file(), "the package holds no {}", exe.display());
    Ok(exe)
}

/// Extract `ducklocal.exe` from a Windows release zip in pure Rust (no
/// `tar.exe` subprocess, no console window flash, zip-slip safe).
#[cfg(windows)]
fn unpack_windows_zip(file: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
    const PREFERRED: &str = "ducklocal-windows-x86_64/ducklocal.exe";

    let reader = std::fs::File::open(file)?;
    let mut archive = zip::ZipArchive::new(reader)?;
    let out = dir.join("unpacked");
    std::fs::create_dir_all(&out)?;

    let mut preferred_ix = None;
    let mut fallback_ix = None;
    let mut saw_unsafe_exe = false;

    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let Some(enclosed) = entry.enclosed_name() else {
            let raw = entry.name().replace('\\', "/");
            if raw.eq_ignore_ascii_case("ducklocal.exe")
                || raw.to_ascii_lowercase().ends_with("/ducklocal.exe")
            {
                saw_unsafe_exe = true;
            }
            continue;
        };
        let normalized = enclosed.to_string_lossy().replace('\\', "/");
        if normalized.eq_ignore_ascii_case(PREFERRED) {
            preferred_ix = Some(i);
            break;
        }
        let is_exe = enclosed
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("ducklocal.exe"));
        let depth = enclosed.components().count();
        if is_exe && (depth == 1 || depth == 2) && fallback_ix.is_none() {
            fallback_ix = Some(i);
        }
    }

    let Some(index) = preferred_ix.or(fallback_ix) else {
        if saw_unsafe_exe {
            anyhow::bail!("the package contains an unsafe path for ducklocal.exe");
        }
        anyhow::bail!("the package holds no ducklocal.exe");
    };

    let mut entry = archive.by_index(index)?;
    let enclosed = entry
        .enclosed_name()
        .ok_or_else(|| anyhow::anyhow!("the package contains an unsafe path"))?;
    let dest = out.join(enclosed);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut dest_file = std::fs::File::create(&dest)?;
    std::io::copy(&mut entry, &mut dest_file)?;
    dest_file.sync_all()?;
    anyhow::ensure!(dest.is_file(), "the package holds no {}", dest.display());
    Ok(dest)
}

/// On macOS, verify that the new app's signature holds and matches the running
/// app's signing team. On Windows, verify the PE64 GUI executable header.
#[cfg(target_os = "macos")]
fn verify(new: &Path, target: &Path) -> anyhow::Result<()> {
    run(std::process::Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(new))?;
    let team = |app: &Path| -> anyhow::Result<String> {
        let output = std::process::Command::new("/usr/bin/codesign")
            .args(["-dv", "--verbose=2"])
            .arg(app)
            .output()?;
        // codesign describes a signature on stderr.
        team_identifier(&String::from_utf8_lossy(&output.stderr))
            .ok_or_else(|| anyhow::anyhow!("{} has no signing team", app.display()))
    };
    let (new_team, current_team) = (team(new)?, team(target)?);
    anyhow::ensure!(
        new_team == current_team,
        "the new app is signed by {new_team}, not {current_team}"
    );
    Ok(())
}

#[cfg(windows)]
fn verify(new: &Path, _target: &Path) -> anyhow::Result<()> {
    verify_windows_pe(new)
}

#[cfg(all(not(target_os = "macos"), not(windows)))]
fn verify(_new: &Path, _target: &Path) -> anyhow::Result<()> {
    Ok(())
}

/// Verify a Windows x86_64 GUI PE executable: DOS `MZ` header, `PE\0\0`
/// signature, `AMD64` machine (`0x8664`), PE32+ optional header (`0x020B`),
/// and Windows GUI subsystem (`2`), matching `scripts/check-windows.py`.
#[cfg_attr(not(windows), allow(dead_code))]
fn verify_windows_pe(path: &Path) -> anyhow::Result<()> {
    use std::io::{Seek, SeekFrom};

    const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
    const IMAGE_NT_OPTIONAL_HDR64_MAGIC: u16 = 0x020B;
    const IMAGE_SUBSYSTEM_WINDOWS_GUI: u16 = 2;

    let mut file = std::fs::File::open(path)?;
    let mut dos = [0u8; 64];
    file.read_exact(&mut dos).map_err(|_| {
        anyhow::anyhow!("{} is too small to be a Windows executable", path.display())
    })?;
    anyhow::ensure!(
        &dos[0..2] == b"MZ",
        "{} is missing the DOS MZ header",
        path.display()
    );
    let pe_offset = u32::from_le_bytes([dos[0x3C], dos[0x3D], dos[0x3E], dos[0x3F]]);
    file.seek(SeekFrom::Start(u64::from(pe_offset)))?;

    let mut header = [0u8; 24 + 70];
    file.read_exact(&mut header)
        .map_err(|_| anyhow::anyhow!("{} has a truncated PE header", path.display()))?;
    anyhow::ensure!(
        &header[0..4] == b"PE\0\0",
        "{} is missing the PE signature",
        path.display()
    );
    let machine = u16::from_le_bytes([header[4], header[5]]);
    anyhow::ensure!(
        machine == IMAGE_FILE_MACHINE_AMD64,
        "{} is not an x86_64 executable (machine {machine:#06x})",
        path.display()
    );
    let opt_size = u16::from_le_bytes([header[20], header[21]]);
    anyhow::ensure!(
        opt_size >= 70,
        "{} has a truncated PE optional header ({opt_size} bytes)",
        path.display()
    );
    let magic = u16::from_le_bytes([header[24], header[25]]);
    anyhow::ensure!(
        magic == IMAGE_NT_OPTIONAL_HDR64_MAGIC,
        "{} is not a PE32+ executable (magic {magic:#06x})",
        path.display()
    );
    let subsystem = u16::from_le_bytes([header[24 + 68], header[24 + 69]]);
    anyhow::ensure!(
        subsystem == IMAGE_SUBSYSTEM_WINDOWS_GUI,
        "{} is not a Windows GUI executable (subsystem {subsystem})",
        path.display()
    );
    Ok(())
}

/// The `TeamIdentifier` in `codesign -dv` output; `None` for an ad-hoc or
/// missing signature.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn team_identifier(description: &str) -> Option<String> {
    description
        .lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|team| !team.is_empty() && *team != "not set")
        .map(str::to_string)
}

#[cfg(not(windows))]
fn run(command: &mut std::process::Command) -> anyhow::Result<()> {
    let output = command.output()?;
    anyhow::ensure!(
        output.status.success(),
        "{:?} failed: {}",
        command.get_program(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

#[cfg(windows)]
enum WindowsApplyOutcome {
    Applied,
    Cancelled,
}

#[cfg(windows)]
fn apply_windows_staged(staged: &Staged) -> anyhow::Result<WindowsApplyOutcome> {
    let pid = std::process::id();
    match staged.target.mode {
        InstallMode::Direct => {
            swap_windows_exe(&staged.new, &staged.target.path, pid)?;
            Ok(WindowsApplyOutcome::Applied)
        }
        InstallMode::Elevated => {
            let helper_exe = current_executable().unwrap_or_else(|| staged.target.path.clone());
            match crate::windows::run_elevated_apply_helper(
                &helper_exe,
                &staged.new,
                &staged.target.path,
                pid,
            )? {
                crate::windows::ElevatedOutcome::Succeeded => Ok(WindowsApplyOutcome::Applied),
                crate::windows::ElevatedOutcome::Cancelled => Ok(WindowsApplyOutcome::Cancelled),
            }
        }
    }
}

/// Entry point for the elevated `__apply-update` helper invoked via UAC on
/// Windows. Runs before CLI/logging/history initialization and returns a
/// process exit code.
#[cfg(windows)]
pub(crate) fn run_elevated_apply(args: &[std::ffi::OsString]) -> i32 {
    match apply_update_elevated(args) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

#[cfg(windows)]
fn apply_update_elevated(args: &[std::ffi::OsString]) -> anyhow::Result<()> {
    let (src, dst, pid) = parse_apply_update_args(args)?;
    let src = strip_verbatim_prefix(src);
    let dst = strip_verbatim_prefix(dst);

    anyhow::ensure!(dst.is_file(), "target executable does not exist");
    anyhow::ensure!(
        dst.extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe")),
        "target is not a .exe file"
    );
    verify_windows_pe(&src)?;

    sweep_stale_binaries(&dst);
    let old = swap_windows_exe(&src, &dst, pid)?;
    let _ = crate::windows::schedule_delete_on_reboot(&old);
    Ok(())
}

#[cfg_attr(not(windows), allow(dead_code))]
fn parse_apply_update_args(args: &[std::ffi::OsString]) -> anyhow::Result<(PathBuf, PathBuf, u32)> {
    let mut src: Option<PathBuf> = None;
    let mut dst: Option<PathBuf> = None;
    let mut pid: Option<u32> = None;
    let mut i = 0;
    while i < args.len() {
        let Some(flag) = args[i].to_str() else {
            anyhow::bail!("unexpected non-UTF-8 argument");
        };
        let value = args
            .get(i + 1)
            .ok_or_else(|| anyhow::anyhow!("missing value for {flag}"))?;
        match flag {
            "--src" => src = Some(PathBuf::from(value)),
            "--dst" => dst = Some(PathBuf::from(value)),
            "--pid" => {
                let parsed: u32 = value
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("invalid --pid"))?
                    .parse()?;
                anyhow::ensure!(parsed != 0, "invalid --pid 0");
                pid = Some(parsed);
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
        i += 2;
    }
    Ok((
        src.ok_or_else(|| anyhow::anyhow!("missing --src"))?,
        dst.ok_or_else(|| anyhow::anyhow!("missing --dst"))?,
        pid.ok_or_else(|| anyhow::anyhow!("missing --pid"))?,
    ))
}

#[cfg_attr(not(windows), allow(dead_code))]
fn sibling_with_suffix(dst: &Path, suffix: &str) -> anyhow::Result<PathBuf> {
    let dir = dst
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", dst.display()))?;
    let name = dst
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{} has no file name", dst.display()))?;
    let mut out = name.to_os_string();
    out.push(suffix);
    Ok(dir.join(out))
}

#[cfg_attr(not(windows), allow(dead_code))]
fn is_transient_lock_error(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::PermissionDenied
        || matches!(err.raw_os_error(), Some(5 | 32 | 33))
}

#[cfg_attr(not(windows), allow(dead_code))]
fn retry_rename(from: &Path, to: &Path) -> std::io::Result<()> {
    const ATTEMPTS: usize = 8;
    const DELAY: Duration = Duration::from_millis(75);
    let mut last_err = None;
    for attempt in 0..ATTEMPTS {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(err) if is_transient_lock_error(&err) && attempt + 1 < ATTEMPTS => {
                last_err = Some(err);
                std::thread::sleep(DELAY);
            }
            Err(err) => return Err(err),
        }
    }
    Err(last_err.expect("at least one rename attempt ran"))
}

/// Atomically replace `dst` with `src` on Windows, keeping the running binary
/// moved aside as `<dst>.old-<pid>` and rolling back if placing the new binary
/// fails.
#[cfg_attr(not(windows), allow(dead_code))]
fn swap_windows_exe(src: &Path, dst: &Path, pid: u32) -> anyhow::Result<PathBuf> {
    swap_windows_exe_with(src, dst, pid, retry_rename)
}

#[cfg_attr(not(windows), allow(dead_code))]
fn swap_windows_exe_with(
    src: &Path,
    dst: &Path,
    pid: u32,
    promote_new: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(src.is_file(), "staged update missing: {}", src.display());
    anyhow::ensure!(
        dst.is_file(),
        "target executable missing: {}",
        dst.display()
    );

    let new = sibling_with_suffix(dst, &format!(".new-{pid}"))?;
    let old = sibling_with_suffix(dst, &format!(".old-{pid}"))?;

    let _ = std::fs::remove_file(&new);
    let _ = std::fs::remove_file(&old);

    if let Err(err) = std::fs::copy(src, &new) {
        let _ = std::fs::remove_file(&new);
        return Err(anyhow::anyhow!(
            "cannot copy update to {}: {err}",
            new.display()
        ));
    }

    if let Err(err) = retry_rename(dst, &old) {
        let _ = std::fs::remove_file(&new);
        return Err(anyhow::anyhow!(
            "cannot move current executable aside to {}: {err}",
            old.display()
        ));
    }

    if let Err(err) = promote_new(&new, dst) {
        let rollback = retry_rename(&old, dst);
        let _ = std::fs::remove_file(&new);
        if let Err(rb_err) = rollback {
            anyhow::bail!("cannot place new executable ({err}); rollback also failed ({rb_err})");
        }
        anyhow::bail!("cannot place new executable at {}: {err}", dst.display());
    }

    Ok(old)
}

fn close_databases_for_restart() {
    crate::db::interrupt();
    if let Err(error) = crate::db::close() {
        tracing::warn!("closing database before restart failed: {error:#}");
    }
    if let Err(error) = crate::history::close() {
        tracing::warn!("closing history before restart failed: {error:#}");
    }
}

/// Put the staged release in place, or relaunch the swapped binary on Windows.
/// Runs as DuckLocal quits.
#[cfg(windows)]
fn install() {
    if let Some(target) = relaunch_target().take() {
        close_databases_for_restart();
        if let Err(error) = spawn_windows_relaunch(&target) {
            tracing::warn!("relaunching the updated app failed: {error:#}");
        }
        return;
    }
    let Some(staged) = staged_release().take() else {
        return;
    };
    // Never trigger an unexpected UAC prompt during a normal quit.
    if staged.target.mode != InstallMode::Direct {
        return;
    }
    if let Err(error) = swap_windows_exe(&staged.new, &staged.target.path, std::process::id()) {
        tracing::warn!("installing the update on quit failed: {error:#}");
    }
}

#[cfg(windows)]
fn spawn_windows_relaunch(target: &Path) -> anyhow::Result<()> {
    let target = strip_verbatim_prefix(target.to_path_buf());
    std::process::Command::new(target)
        .env(
            crate::windows::UPDATE_WAIT_PID_ENV,
            std::process::id().to_string(),
        )
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(not(windows))]
fn install() {
    let Some(staged) = staged_release().take() else {
        return;
    };
    let relaunch = RELAUNCH.load(Ordering::Relaxed);
    if relaunch {
        close_databases_for_restart();
    }
    if let Err(error) = install_staged(&staged, relaunch) {
        tracing::warn!("installing the update failed: {error:#}");
    }
}

#[cfg(target_os = "macos")]
fn install_staged(staged: &Staged, relaunch: bool) -> anyhow::Result<()> {
    // The bundle cannot be swapped while it runs: a shell that outlives
    // DuckLocal waits for it to exit, moves the old app aside, the new one
    // in — the old one back if that fails — and opens the result.
    const SCRIPT: &str = r#"
pid=$1; new=$2; app=$3; relaunch=$4
while kill -0 "$pid" 2>/dev/null; do sleep 0.2; done
old="$app.old-$$"
if mv "$app" "$old"; then
  if mv "$new" "$app"; then rm -rf "$old"; else mv "$old" "$app"; fi
fi
if [ "$relaunch" = 1 ]; then /usr/bin/open "$app"; fi
"#;
    std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(SCRIPT)
        .arg("ducklocal-update")
        .arg(std::process::id().to_string())
        .arg(&staged.new)
        .arg(&staged.target.path)
        .arg(if relaunch { "1" } else { "0" })
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(all(not(target_os = "macos"), not(windows)))]
fn install_staged(staged: &Staged, relaunch: bool) -> anyhow::Result<()> {
    // A running executable can be unlinked/renamed on Linux: copy the new one
    // beside it and rename it into place.
    let target = &staged.target.path;
    let incoming = target.with_extension("update");
    std::fs::copy(&staged.new, &incoming)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&incoming, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&incoming, target)?;
    if relaunch {
        std::process::Command::new(target).spawn()?;
    }
    Ok(())
}

/// Legacy backup path used by earlier Windows versions (`ducklocal.old`).
#[cfg_attr(not(windows), allow(dead_code))]
fn old_executable(target: &Path) -> PathBuf {
    target.with_extension("old")
}

/// Sweep leftover update binaries beside `exe`: legacy `<stem>.old` /
/// `<stem>.update` plus `<exe>.old-*` and `<exe>.new-*`.
#[cfg_attr(not(windows), allow(dead_code))]
fn sweep_stale_binaries(exe: &Path) {
    let Some(dir) = exe.parent() else {
        return;
    };
    let Some(exe_name) = exe.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let old_prefix = format!("{exe_name}.old-").to_ascii_lowercase();
    let new_prefix = format!("{exe_name}.new-").to_ascii_lowercase();
    let legacy_old = old_executable(exe)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_ascii_lowercase);
    let legacy_update = exe
        .with_extension("update")
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_ascii_lowercase);

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_ascii_lowercase) else {
            continue;
        };
        let is_stale = name.starts_with(&old_prefix)
            || name.starts_with(&new_prefix)
            || legacy_old.as_deref() == Some(name.as_str())
            || legacy_update.as_deref() == Some(name.as_str());
        if is_stale {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Remove what the last update left: its download, and on Windows the
/// executable(s) it replaced. Run at launch, before a check can start another.
pub fn clean_up() {
    if let Ok(dir) = updates_dir() {
        let _ = std::fs::remove_dir_all(dir);
    }
    #[cfg(windows)]
    if let Some(exe) = current_executable() {
        sweep_stale_binaries(&exe);
    }
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
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// The promise on the network page: nothing asks for an update at launch
    /// until someone turns it on.
    #[gpui_kit::test]
    fn no_check_at_launch_until_turned_on(cx: &mut gpui_kit::TestAppContext) {
        let _guard = crate::db::connection_guard();
        crate::history::with_test_history(|| {
            assert!(!cx.update(super::at_launch));
            cx.update(super::on_launch);
            assert_eq!(cx.update(|cx| super::status(cx)), super::Status::Idle);
        });
    }

    use super::{
        is_newer, package_url, parse_apply_update_args, strip_verbatim_prefix, swap_windows_exe,
        swap_windows_exe_with, sweep_stale_binaries, team_identifier, verify_windows_pe, Manifest,
    };

    fn unique_test_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ducklocal_update_{label}_{}_{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn build_test_pe(machine: u16, magic: u16, subsystem: u16) -> Vec<u8> {
        let pe_offset: usize = 0x80;
        let mut data = vec![0u8; pe_offset + 24 + 240];
        data[0..2].copy_from_slice(b"MZ");
        data[0x3C..0x40].copy_from_slice(&(pe_offset as u32).to_le_bytes());
        data[pe_offset..pe_offset + 4].copy_from_slice(b"PE\0\0");
        data[pe_offset + 4..pe_offset + 6].copy_from_slice(&machine.to_le_bytes());
        data[pe_offset + 20..pe_offset + 22].copy_from_slice(&240u16.to_le_bytes());
        data[pe_offset + 24..pe_offset + 26].copy_from_slice(&magic.to_le_bytes());
        data[pe_offset + 24 + 68..pe_offset + 24 + 70].copy_from_slice(&subsystem.to_le_bytes());
        data
    }

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

    #[test]
    fn the_manifest_reads_as_the_mirror_script_writes_it() {
        let manifest: Manifest = serde_json::from_str(
            r#"{"version":"v0.3.0","packages":{
                "ducklocal-macos-arm64.dmg":{"sha256":"ab12","size":42}}}"#,
        )
        .unwrap();
        assert_eq!(manifest.version, "v0.3.0");
        let package = &manifest.packages["ducklocal-macos-arm64.dmg"];
        assert_eq!((package.sha256.as_str(), package.size), ("ab12", 42));
    }

    #[test]
    fn packages_download_from_their_release_not_latest() {
        assert_eq!(
            package_url("v0.3.0", "ducklocal-macos-arm64.dmg"),
            "https://dl.ducklocal.app/v0.3.0/ducklocal-macos-arm64.dmg"
        );
    }

    #[test]
    fn only_a_real_team_signs() {
        let signed = "Identifier=app.ducklocal\nTeamIdentifier=ABCDE12345\nSealed Resources";
        assert_eq!(team_identifier(signed).as_deref(), Some("ABCDE12345"));
        assert_eq!(team_identifier("TeamIdentifier=not set\n"), None);
        assert_eq!(team_identifier("Signature=adhoc\n"), None);
    }

    #[test]
    fn verbatim_prefixes_are_stripped() {
        assert_eq!(
            strip_verbatim_prefix(PathBuf::from(
                r"\\?\C:\Program Files\DuckLocal\ducklocal.exe"
            )),
            PathBuf::from(r"C:\Program Files\DuckLocal\ducklocal.exe")
        );
        assert_eq!(
            strip_verbatim_prefix(PathBuf::from(r"\\?\UNC\fileserver\share\ducklocal.exe")),
            PathBuf::from(r"\\fileserver\share\ducklocal.exe")
        );
        assert_eq!(
            strip_verbatim_prefix(PathBuf::from(r"C:\Portable\ducklocal.exe")),
            PathBuf::from(r"C:\Portable\ducklocal.exe")
        );
    }

    #[test]
    fn pe_header_validation_accepts_amd64_gui_and_rejects_invalid() {
        let dir = unique_test_dir("pe");

        let valid = dir.join("valid.exe");
        std::fs::write(&valid, build_test_pe(0x8664, 0x020B, 2)).unwrap();
        assert!(verify_windows_pe(&valid).is_ok());

        let not_mz = dir.join("not_mz.exe");
        std::fs::write(&not_mz, b"not a PE executable at all").unwrap();
        assert!(verify_windows_pe(&not_mz).is_err());

        let bad_sig = dir.join("bad_sig.exe");
        let mut bytes = build_test_pe(0x8664, 0x020B, 2);
        bytes[0x80..0x84].copy_from_slice(b"NOPE");
        std::fs::write(&bad_sig, bytes).unwrap();
        assert!(verify_windows_pe(&bad_sig).is_err());

        let wrong_arch = dir.join("x86.exe");
        std::fs::write(&wrong_arch, build_test_pe(0x014C, 0x020B, 2)).unwrap();
        assert!(verify_windows_pe(&wrong_arch).is_err());

        let wrong_magic = dir.join("pe32.exe");
        std::fs::write(&wrong_magic, build_test_pe(0x8664, 0x010B, 2)).unwrap();
        assert!(verify_windows_pe(&wrong_magic).is_err());

        let console_subsys = dir.join("console.exe");
        std::fs::write(&console_subsys, build_test_pe(0x8664, 0x020B, 3)).unwrap();
        assert!(verify_windows_pe(&console_subsys).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn windows_swap_replaces_target_and_rolls_back_on_failure() {
        let dir = unique_test_dir("swap");
        let staged = dir.join("staged.exe");
        let target = dir.join("ducklocal.exe");
        std::fs::write(&staged, b"new-binary-v2").unwrap();
        std::fs::write(&target, b"old-binary-v1").unwrap();

        let old = swap_windows_exe(&staged, &target, 4242).unwrap();
        assert_eq!(old, dir.join("ducklocal.exe.old-4242"));
        assert_eq!(std::fs::read(&target).unwrap(), b"new-binary-v2");
        assert_eq!(std::fs::read(&old).unwrap(), b"old-binary-v1");
        assert!(!dir.join("ducklocal.exe.new-4242").exists());

        // Now force the final promote step to fail and verify rollback restores `target`.
        std::fs::write(&staged, b"broken-v3").unwrap();
        let err = swap_windows_exe_with(&staged, &target, 4243, |_, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "simulated AV lock",
            ))
        })
        .unwrap_err();
        assert!(err.to_string().contains("cannot place new executable"));
        assert_eq!(std::fs::read(&target).unwrap(), b"new-binary-v2");
        assert!(!dir.join("ducklocal.exe.old-4243").exists());
        assert!(!dir.join("ducklocal.exe.new-4243").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_sweeps_stale_update_files_only() {
        let dir = unique_test_dir("sweep");
        let exe = dir.join("ducklocal.exe");
        let stale = [
            dir.join("ducklocal.old"),
            dir.join("ducklocal.update"),
            dir.join("ducklocal.exe.old-101"),
            dir.join("ducklocal.exe.old-202"),
            dir.join("ducklocal.exe.new-303"),
        ];
        let keep = [
            exe.clone(),
            dir.join("README.md"),
            dir.join("other.old"),
            dir.join("notes.new-1"),
        ];

        for path in stale.iter().chain(keep.iter()) {
            std::fs::write(path, b"data").unwrap();
        }

        sweep_stale_binaries(&exe);

        for path in &stale {
            assert!(
                !path.exists(),
                "{} should have been removed",
                path.display()
            );
        }
        for path in &keep {
            assert!(path.exists(), "{} should have been kept", path.display());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_update_args_parse_and_validate() {
        let args: Vec<OsString> = [
            "--src",
            r"C:\cache\ducklocal.exe",
            "--dst",
            r"C:\Program Files\DuckLocal\ducklocal.exe",
            "--pid",
            "9876",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        let (src, dst, pid) = parse_apply_update_args(&args).unwrap();
        assert_eq!(src, PathBuf::from(r"C:\cache\ducklocal.exe"));
        assert_eq!(
            dst,
            PathBuf::from(r"C:\Program Files\DuckLocal\ducklocal.exe")
        );
        assert_eq!(pid, 9876);

        let missing: Vec<OsString> = ["--src", "a.exe"].into_iter().map(OsString::from).collect();
        assert!(parse_apply_update_args(&missing).is_err());

        let zero_pid: Vec<OsString> = ["--src", "a.exe", "--dst", "b.exe", "--pid", "0"]
            .into_iter()
            .map(OsString::from)
            .collect();
        assert!(parse_apply_update_args(&zero_pid).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_zip_unpack_extracts_exe_and_rejects_zip_slip() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let dir = unique_test_dir("zip");
        let pe_bytes = build_test_pe(0x8664, 0x020B, 2);

        // 1. Standard release layout: `ducklocal-windows-x86_64/ducklocal.exe`.
        let good_zip = dir.join("good.zip");
        {
            let file = std::fs::File::create(&good_zip).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let opts = SimpleFileOptions::default();
            writer
                .start_file("ducklocal-windows-x86_64/README.md", opts)
                .unwrap();
            writer.write_all(b"readme").unwrap();
            writer
                .start_file("ducklocal-windows-x86_64/ducklocal.exe", opts)
                .unwrap();
            writer.write_all(&pe_bytes).unwrap();
            writer.finish().unwrap();
        }
        let good_out = dir.join("out_good");
        let extracted = super::unpack_windows_zip(&good_zip, &good_out).unwrap();
        assert_eq!(std::fs::read(&extracted).unwrap(), pe_bytes);
        assert!(verify_windows_pe(&extracted).is_ok());

        // 2. Fallback root `ducklocal.exe`.
        let root_zip = dir.join("root.zip");
        {
            let file = std::fs::File::create(&root_zip).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file("ducklocal.exe", SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&pe_bytes).unwrap();
            writer.finish().unwrap();
        }
        let root_out = dir.join("out_root");
        let extracted_root = super::unpack_windows_zip(&root_zip, &root_out).unwrap();
        assert_eq!(std::fs::read(&extracted_root).unwrap(), pe_bytes);

        // 3. Zip-slip entry `../ducklocal.exe` must be rejected and never written outside.
        let evil_zip = dir.join("evil.zip");
        {
            let file = std::fs::File::create(&evil_zip).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file("../ducklocal.exe", SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&pe_bytes).unwrap();
            writer.finish().unwrap();
        }
        let evil_out = dir.join("out_evil");
        let err = super::unpack_windows_zip(&evil_zip, &evil_out).unwrap_err();
        assert!(err.to_string().contains("unsafe path"));
        assert!(!dir.join("ducklocal.exe").exists());
        assert!(!evil_out.join("ducklocal.exe").exists());

        // 4. Zip missing `ducklocal.exe` returns an error.
        let empty_zip = dir.join("empty.zip");
        {
            let file = std::fs::File::create(&empty_zip).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file(
                    "ducklocal-windows-x86_64/README.md",
                    SimpleFileOptions::default(),
                )
                .unwrap();
            writer.write_all(b"only docs").unwrap();
            writer.finish().unwrap();
        }
        assert!(super::unpack_windows_zip(&empty_zip, &dir.join("out_empty")).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
