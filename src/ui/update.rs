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
//! SHA-256 (and on macOS against the running app's signing team), and
//! unpacked beside the cache. It replaces the installed app when DuckLocal
//! quits: at once on "Restart to update", else on the next quit. Where the
//! app cannot replace itself — a development build, a folder it may not
//! write, a platform with no package — the status bar links to the package
//! on dl.ducklocal.app instead.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

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
    Downloading(String),
    /// A newer release unpacked and waiting for DuckLocal to quit.
    Ready(String),
    Failed(String),
}

#[derive(Default)]
struct Update {
    status: Status,
    /// `None` until read from the history store.
    at_launch: Option<bool>,
    /// Installs the downloaded release when DuckLocal quits.
    _on_quit: Option<Subscription>,
}

impl Global for Update {}

/// An unpacked release: the new app (macOS) or executable, and the installed
/// one it replaces.
#[derive(Clone)]
struct Staged {
    new: PathBuf,
    target: PathBuf,
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
        Status::Checking | Status::Downloading(_) | Status::Ready(_)
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
        cx.update(|cx| set(Status::Downloading(tag.clone()), cx));
        let staged = {
            let tag = tag.clone();
            smol::unblock(move || stage(&tag, name, &package, target)).await
        };
        cx.update(|cx| match staged {
            Ok(staged) => {
                *staged_release() = Some(staged);
                let on_quit = cx.on_app_quit(|_| {
                    install();
                    async {}
                });
                cx.default_global::<Update>()._on_quit = Some(on_quit);
                set(Status::Ready(tag), cx);
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
pub fn restart(cx: &mut App) {
    if staged_release().is_some() {
        // The quit hook installs it; only this way does it reopen.
        RELAUNCH.store(true, Ordering::Relaxed);
        cx.quit();
    }
}

static RELAUNCH: AtomicBool = AtomicBool::new(false);

/// The downloaded release, once unpacked; the quit hook takes it.
fn staged_release() -> std::sync::MutexGuard<'static, Option<Staged>> {
    static STAGED: Mutex<Option<Staged>> = Mutex::new(None);
    STAGED.lock().unwrap_or_else(|error| error.into_inner())
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

/// What a release replaces: the `.app` bundle the executable sits in on
/// macOS, the executable elsewhere — `None` when this copy cannot replace
/// itself. A development build never does: `cargo run` would overwrite
/// `target/` with a release.
fn install_target() -> Option<PathBuf> {
    if cfg!(debug_assertions) {
        return None;
    }
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let target = if cfg!(target_os = "macos") {
        exe.ancestors()
            .find(|path| path.extension().is_some_and(|ext| ext == "app"))?
            .to_path_buf()
    } else {
        exe
    };
    // A downloaded app run in place from a read-only, translocated copy, or
    // one installed where only an administrator may write.
    let dir = target.parent()?;
    let probe = dir.join(".ducklocal-update-probe");
    std::fs::File::create(&probe).ok()?;
    let _ = std::fs::remove_file(&probe);
    Some(target)
}

/// Where downloaded releases are unpacked.
fn updates_dir() -> anyhow::Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "DuckLocal")
        .ok_or_else(|| anyhow::anyhow!("no cache directory"))?;
    Ok(dirs.cache_dir().join("updates"))
}

/// Download package `name` of release `tag`, check it, and unpack the app
/// or executable that replaces `target`.
fn stage(tag: &str, name: &str, package: &Package, target: PathBuf) -> anyhow::Result<Staged> {
    let dir = updates_dir()?.join(tag);
    // A half-finished earlier attempt is started over.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(name);
    download(&package_url(tag, name), package, &file)?;
    let new = unpack(&file, &dir)?;
    verify(&new, &target)?;
    Ok(Staged { new, target })
}

/// Fetch `url` into `path`, failing unless it is `package`'s size and hash.
fn download(url: &str, package: &Package, path: &Path) -> anyhow::Result<()> {
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
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        out.write_all(&buffer[..read])?;
        size += read as u64;
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
fn unpack(file: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
    if cfg!(target_os = "macos") {
        let mount = dir.join("mount");
        std::fs::create_dir_all(&mount)?;
        run(std::process::Command::new("hdiutil")
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
        let copied = run(std::process::Command::new("ditto")
            .arg(mount.join("DuckLocal.app"))
            .arg(&app));
        let _ = run(std::process::Command::new("hdiutil")
            .args(["detach", "-force"])
            .arg(&mount));
        copied?;
        Ok(app)
    } else {
        // `tar` reads the zip too on Windows 10 and later.
        let out = dir.join("unpacked");
        std::fs::create_dir_all(&out)?;
        run(std::process::Command::new("tar")
            .arg("-xf")
            .arg(file)
            .arg("-C")
            .arg(&out))?;
        let (folder, exe) = if cfg!(windows) {
            ("ducklocal-windows-x86_64", "ducklocal.exe")
        } else {
            ("ducklocal-linux-x86_64", "ducklocal")
        };
        let exe = out.join(folder).join(exe);
        anyhow::ensure!(exe.is_file(), "the package holds no {}", exe.display());
        Ok(exe)
    }
}

/// On macOS, that the new app's signature holds and is the same team's as
/// the running app's: a checksum from the same mirror proves no more than
/// the mirror. Windows and Linux builds are not signed.
fn verify(new: &Path, target: &Path) -> anyhow::Result<()> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    run(std::process::Command::new("codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(new))?;
    let team = |app: &Path| -> anyhow::Result<String> {
        let output = std::process::Command::new("codesign")
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

/// The `TeamIdentifier` in `codesign -dv` output; `None` for an ad-hoc or
/// missing signature.
fn team_identifier(description: &str) -> Option<String> {
    description
        .lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|team| !team.is_empty() && *team != "not set")
        .map(str::to_string)
}

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

/// Put the staged release in place, and open it if `restart` asked to. Runs
/// as DuckLocal quits.
fn install() {
    let Some(staged) = staged_release().take() else {
        return;
    };
    if let Err(error) = install_staged(&staged, RELAUNCH.load(Ordering::Relaxed)) {
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
if [ "$relaunch" = 1 ]; then open "$app"; fi
"#;
    std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(SCRIPT)
        .arg("ducklocal-update")
        .arg(std::process::id().to_string())
        .arg(&staged.new)
        .arg(&staged.target)
        .arg(if relaunch { "1" } else { "0" })
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn install_staged(staged: &Staged, relaunch: bool) -> anyhow::Result<()> {
    // A running executable can be renamed, on Windows too: the new one is
    // copied beside it, and the old moved out of its way.
    let target = &staged.target;
    let incoming = target.with_extension("update");
    std::fs::copy(&staged.new, &incoming)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&incoming, std::fs::Permissions::from_mode(0o755))?;
    }
    if cfg!(windows) {
        // Windows will not replace a running executable, only rename it;
        // the next launch deletes it.
        let old = old_executable(target);
        let _ = std::fs::remove_file(&old);
        std::fs::rename(target, &old)?;
    }
    std::fs::rename(&incoming, target)?;
    if relaunch {
        std::process::Command::new(target).spawn()?;
    }
    Ok(())
}

/// Where Windows keeps the replaced executable until the next launch.
fn old_executable(target: &Path) -> PathBuf {
    target.with_extension("old")
}

/// Remove what the last update left: its download, and on Windows the
/// executable it replaced. Run at launch, before a check can start another.
pub fn clean_up() {
    if let Ok(dir) = updates_dir() {
        let _ = std::fs::remove_dir_all(dir);
    }
    if cfg!(windows) {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::fs::remove_file(old_executable(&exe));
        }
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
    use super::{is_newer, package_url, team_identifier, Manifest};

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
}
