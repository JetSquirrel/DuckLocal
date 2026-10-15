//! Where this copy of DuckLocal is installed, and replacing it.
//!
//! An update happens in two steps. Staging, in the background: the verified
//! package is unpacked, checked once more — on macOS, the new app's signature
//! against the running one's team and bundle id — and copied beside the
//! installed copy, on the same volume, so the swap is a rename. Applying, when
//! the person clicks restart: the staged copy takes the installed one's place
//! and a small helper process, which waits for this one to exit, opens it.
//!
//! - macOS: the whole `.app` is replaced, never its executable alone, which
//!   would break its signature. Running from a translocated copy (an app
//!   opened from Downloads before it was moved) or a folder this user cannot
//!   write is not updated in place.
//! - Linux: the executable is renamed over; the running process keeps the
//!   old file open until it exits.
//! - Windows: a running executable cannot be replaced, so the helper waits for
//!   this process to exit, moves the new one into place and starts it.
//!
//! A swap that fails part-way puts the installed copy back.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context as _};

/// Why this copy cannot be updated in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Not a packaged release: a development build, run from `target/`.
    NotPackaged,
    /// macOS runs the app from a randomized read-only copy, because it was
    /// opened before being moved out of its download folder.
    Translocated,
    /// The folder it is installed in is not writable by this user.
    ReadOnly,
}

impl Reason {
    /// The i18n key that says it to the person.
    pub fn label_key(self) -> &'static str {
        match self {
            Reason::NotPackaged => "update.reason.not_packaged",
            Reason::Translocated => "update.reason.translocated",
            Reason::ReadOnly => "update.reason.read_only",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installation {
    /// macOS: the `.app` this process runs from.
    Bundle(PathBuf),
    /// Linux, Windows: the executable, in a folder this user can write.
    Portable(PathBuf),
    Manual(Reason),
}

/// Where this process is installed.
pub fn installation() -> Installation {
    match std::env::current_exe().and_then(|exe| exe.canonicalize()) {
        Ok(exe) => locate(&exe, std::env::consts::OS, writable),
        Err(_) => Installation::Manual(Reason::NotPackaged),
    }
}

fn locate(exe: &Path, os: &str, writable: impl Fn(&Path) -> bool) -> Installation {
    let text = exe.to_string_lossy().replace('\\', "/");
    if text.contains("/target/debug/") || text.contains("/target/release/") {
        return Installation::Manual(Reason::NotPackaged);
    }
    if os == "macos" {
        let bundle = exe
            .parent()
            .filter(|dir| dir.file_name().is_some_and(|name| name == "MacOS"))
            .and_then(Path::parent)
            .filter(|dir| dir.file_name().is_some_and(|name| name == "Contents"))
            .and_then(Path::parent)
            .filter(|dir| dir.extension().is_some_and(|ext| ext == "app"));
        let Some(bundle) = bundle else {
            return Installation::Manual(Reason::NotPackaged);
        };
        if text.contains("/AppTranslocation/") {
            return Installation::Manual(Reason::Translocated);
        }
        return match bundle.parent() {
            Some(folder) if writable(folder) => Installation::Bundle(bundle.to_path_buf()),
            _ => Installation::Manual(Reason::ReadOnly),
        };
    }
    match exe.parent() {
        Some(folder) if writable(folder) => Installation::Portable(exe.to_path_buf()),
        _ => Installation::Manual(Reason::ReadOnly),
    }
}

/// Whether this user can create files in `dir`: tried, not inferred from
/// permission bits, which say nothing of ACLs or read-only volumes.
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".ducklocal-write-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// A new version unpacked, verified and waiting beside the installed one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    /// What is installed now.
    installed: PathBuf,
    /// Its replacement, on the same volume.
    staged: PathBuf,
}

/// Unpack and verify `package`, the release `version` for this platform,
/// beside `installed`. Blocking — call it off the UI thread.
pub fn stage(package: &Path, installed: &Installation, version: &str) -> anyhow::Result<Staged> {
    match installed {
        Installation::Bundle(bundle) => stage_bundle(package, bundle, version),
        Installation::Portable(exe) => stage_portable(package, exe, version),
        Installation::Manual(_) => bail!("This copy of DuckLocal cannot be updated in place"),
    }
}

/// Put the staged version in place and start it once this process exits; the
/// caller quits right after. A swap that fails puts the installed one back.
pub fn apply(staged: &Staged) -> anyhow::Result<()> {
    if cfg!(target_os = "windows") {
        // The running executable cannot be replaced: the helper does it.
        return relaunch_windows(&staged.staged, &staged.installed);
    }
    let old = swap(staged)?;
    relaunch_unix(&staged.installed, &old)
}

/// Move the installed copy aside and the staged one into its place; where the
/// old copy went, for the helper to delete once this process has exited. A
/// second rename that fails puts the installed copy back.
fn swap(staged: &Staged) -> anyhow::Result<PathBuf> {
    let parent = staged
        .installed
        .parent()
        .ok_or_else(|| anyhow!("{} has no folder", staged.installed.display()))?;
    let old = parent.join(
        if staged.installed.extension().is_some_and(|ext| ext == "app") {
            ".DuckLocal-old.app"
        } else {
            ".ducklocal-old"
        },
    );
    remove(&old);
    std::fs::rename(&staged.installed, &old)
        .with_context(|| format!("Moving {} aside", staged.installed.display()))?;
    if let Err(error) = std::fs::rename(&staged.staged, &staged.installed) {
        let _ = std::fs::rename(&old, &staged.installed);
        return Err(error).context("Moving the new version into place");
    }
    Ok(old)
}

fn remove(path: &Path) {
    if path.is_dir() {
        let _ = std::fs::remove_dir_all(path);
    } else if path.exists() {
        let _ = std::fs::remove_file(path);
    }
}

/// Run `program` with `args`, failing with what it printed when it fails.
fn run(program: &str, args: &[&std::ffi::OsStr]) -> anyhow::Result<String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("Running {program}"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        bail!("{program} failed: {}", text.trim());
    }
    Ok(text)
}

// --- macOS -------------------------------------------------------------------

fn stage_bundle(dmg: &Path, bundle: &Path, version: &str) -> anyhow::Result<Staged> {
    let parent = bundle
        .parent()
        .ok_or_else(|| anyhow!("{} has no folder", bundle.display()))?;
    let mount = std::env::temp_dir().join(format!("ducklocal-update-{}", std::process::id()));
    std::fs::create_dir_all(&mount)?;
    run(
        "hdiutil",
        &[
            "attach".as_ref(),
            "-nobrowse".as_ref(),
            "-readonly".as_ref(),
            "-noautoopen".as_ref(),
            "-mountpoint".as_ref(),
            mount.as_os_str(),
            dmg.as_os_str(),
        ],
    )?;
    let staged = parent.join(".DuckLocal-update.app");
    let result = (|| -> anyhow::Result<()> {
        let app = std::fs::read_dir(&mount)?
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|ext| ext == "app"))
            .ok_or_else(|| anyhow!("The disk image holds no app"))?;
        verify_bundle(&app, bundle, version)?;
        remove(&staged);
        run("ditto", &[app.as_os_str(), staged.as_os_str()])?;
        // The copy, once more: what is swapped in is what was verified.
        verify_bundle(&staged, bundle, version)
    })();
    let detached = run(
        "hdiutil",
        &["detach".as_ref(), mount.as_os_str(), "-quiet".as_ref()],
    );
    if detached.is_err() {
        let _ = run(
            "hdiutil",
            &["detach".as_ref(), mount.as_os_str(), "-force".as_ref()],
        );
    }
    let _ = std::fs::remove_dir(&mount);
    if let Err(error) = result {
        remove(&staged);
        return Err(error);
    }
    Ok(Staged {
        installed: bundle.to_path_buf(),
        staged,
    })
}

/// The new app is intact, signed by the team that signed the running one,
/// the same app (bundle id), and the version the release says it is.
fn verify_bundle(new: &Path, running: &Path, version: &str) -> anyhow::Result<()> {
    run(
        "codesign",
        &[
            "--verify".as_ref(),
            "--deep".as_ref(),
            "--strict".as_ref(),
            new.as_os_str(),
        ],
    )
    .context("The new version's signature does not verify")?;
    let (team, id) = signer(running)?;
    let (new_team, new_id) = signer(new)?;
    if team.is_empty() || new_team != team {
        bail!("The new version is signed by team {new_team:?}, not {team:?}");
    }
    if new_id != id {
        bail!("The new version is {new_id:?}, not {id:?}");
    }
    let plist = new.join("Contents/Info.plist");
    let found = run(
        "plutil",
        &[
            "-extract".as_ref(),
            "CFBundleShortVersionString".as_ref(),
            "raw".as_ref(),
            "-o".as_ref(),
            "-".as_ref(),
            plist.as_os_str(),
        ],
    )?;
    if found.trim().trim_start_matches('v') != version.trim_start_matches('v') {
        bail!("The new app is version {}, not {version}", found.trim());
    }
    Ok(())
}

/// An app's signing team and bundle id, as `codesign` reads them.
fn signer(app: &Path) -> anyhow::Result<(String, String)> {
    let text = run(
        "codesign",
        &["-dv".as_ref(), "--verbose=2".as_ref(), app.as_os_str()],
    )?;
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name))
            .map(str::trim)
            .filter(|value| *value != "not set")
            .unwrap_or_default()
            .to_string()
    };
    Ok((field("TeamIdentifier="), field("Identifier=")))
}

// --- Linux and Windows -------------------------------------------------------

fn stage_portable(package: &Path, exe: &Path, version: &str) -> anyhow::Result<Staged> {
    let folder = exe
        .parent()
        .ok_or_else(|| anyhow!("{} has no folder", exe.display()))?;
    let unpack = folder.join(".ducklocal-update");
    remove(&unpack);
    std::fs::create_dir_all(&unpack)?;
    let name = exe
        .file_name()
        .ok_or_else(|| anyhow!("{} has no name", exe.display()))?;
    let staged = folder.join(format!("{}.new", name.to_string_lossy()));
    let result = (|| -> anyhow::Result<()> {
        // `tar` reads both: GNU tar the Linux .tar.gz, and the bsdtar every
        // Windows 10 and later ships the Windows .zip.
        let flags = if cfg!(target_os = "windows") {
            "-xf"
        } else {
            "-xzf"
        };
        run(
            "tar",
            &[
                flags.as_ref(),
                package.as_os_str(),
                "-C".as_ref(),
                unpack.as_os_str(),
            ],
        )?;
        let new = find(&unpack, name, 2)
            .ok_or_else(|| anyhow!("The package holds no {}", name.to_string_lossy()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))?;
            // Windows builds are GUI programs whose console output a pipe may
            // not get; there the digest alone vouches for the file.
            let said = run(&new.to_string_lossy(), &["--version".as_ref()])?;
            if !said.contains(version.trim_start_matches('v')) {
                bail!("The new program says it is {}, not {version}", said.trim());
            }
        }
        #[cfg(not(unix))]
        let _ = version;
        remove(&staged);
        std::fs::rename(&new, &staged)?;
        Ok(())
    })();
    remove(&unpack);
    result?;
    Ok(Staged {
        installed: exe.to_path_buf(),
        staged,
    })
}

/// A file called `name` at most `depth` folders under `dir`.
fn find(dir: &Path, name: &std::ffi::OsStr, depth: usize) -> Option<PathBuf> {
    let entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    entries
        .iter()
        .find(|path| path.is_file() && path.file_name() == Some(name))
        .cloned()
        .or_else(|| {
            (depth > 0)
                .then(|| {
                    entries
                        .iter()
                        .filter(|path| path.is_dir())
                        .find_map(|path| find(path, name, depth - 1))
                })
                .flatten()
        })
}

// --- Relaunching -------------------------------------------------------------

/// A detached shell that waits for this process to exit, removes the old
/// copy and opens the new one.
fn relaunch_unix(installed: &Path, old: &Path) -> anyhow::Result<()> {
    let open = if cfg!(target_os = "macos") {
        r#"open "$2""#
    } else {
        r#"exec "$2""#
    };
    let script =
        format!(r#"while kill -0 "$0" 2>/dev/null; do sleep 0.2; done; rm -rf "$1"; {open}"#);
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(script)
        .arg(std::process::id().to_string())
        .arg(old)
        .arg(installed)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command.spawn().context("Starting the relaunch helper")?;
    Ok(())
}

/// A hidden PowerShell that waits for this process to exit, moves the new
/// executable over the old one and starts it.
fn relaunch_windows(staged: &Path, installed: &Path) -> anyhow::Result<()> {
    let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "''"));
    let script = format!(
        "Wait-Process -Id {pid} -ErrorAction SilentlyContinue; \
         Move-Item -LiteralPath {new} -Destination {exe} -Force; \
         Start-Process -FilePath {exe}",
        pid = std::process::id(),
        new = quote(staged),
        exe = quote(installed),
    );
    let mut command = Command::new("powershell");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &script,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    command.spawn().context("Starting the update helper")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mac_app_is_updated_as_its_whole_bundle() {
        let exe = Path::new("/Applications/DuckLocal.app/Contents/MacOS/ducklocal");
        assert_eq!(
            locate(exe, "macos", |_| true),
            Installation::Bundle(PathBuf::from("/Applications/DuckLocal.app"))
        );
        assert_eq!(
            locate(exe, "macos", |_| false),
            Installation::Manual(Reason::ReadOnly)
        );
        let translocated = Path::new(
            "/private/var/folders/x/T/AppTranslocation/ABC/d/DuckLocal.app/Contents/MacOS/ducklocal",
        );
        assert_eq!(
            locate(translocated, "macos", |_| true),
            Installation::Manual(Reason::Translocated)
        );
        assert_eq!(
            locate(Path::new("/usr/local/bin/ducklocal"), "macos", |_| true),
            Installation::Manual(Reason::NotPackaged)
        );
    }

    #[test]
    fn a_portable_copy_is_updated_where_it_can_be_written() {
        let exe = Path::new("/home/me/apps/ducklocal-linux-x86_64/ducklocal");
        assert_eq!(
            locate(exe, "linux", |_| true),
            Installation::Portable(exe.to_path_buf())
        );
        assert_eq!(
            locate(exe, "linux", |_| false),
            Installation::Manual(Reason::ReadOnly)
        );
        let windows = Path::new(r"C:\Users\me\ducklocal-windows-x86_64\ducklocal.exe");
        assert_eq!(
            locate(windows, "windows", |_| true),
            Installation::Portable(windows.to_path_buf())
        );
    }

    #[test]
    fn a_development_build_is_never_replaced() {
        for exe in [
            "/src/ducklocal/target/debug/ducklocal",
            "/src/ducklocal/target/release/DuckLocal.app/Contents/MacOS/ducklocal",
        ] {
            assert_eq!(
                locate(Path::new(exe), "macos", |_| true),
                Installation::Manual(Reason::NotPackaged),
                "{exe}"
            );
        }
    }

    /// Stages a real signed release against a copy of an installed one:
    /// `DUCKLOCAL_TEST_DMG` (the release's dmg), `DUCKLOCAL_TEST_APP` (an
    /// installed DuckLocal.app, which is copied, never touched) and
    /// `DUCKLOCAL_TEST_VERSION` (the dmg's version).
    #[test]
    #[ignore = "needs a signed release dmg; see the doc comment"]
    fn a_signed_release_stages_beside_its_installed_copy() {
        let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
        let (dmg, app, version) = (
            PathBuf::from(var("DUCKLOCAL_TEST_DMG")),
            PathBuf::from(var("DUCKLOCAL_TEST_APP")),
            var("DUCKLOCAL_TEST_VERSION"),
        );
        let dir = std::env::temp_dir().join(format!("ducklocal-stage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let installed = dir.join("DuckLocal.app");
        run("ditto", &[app.as_os_str(), installed.as_os_str()]).unwrap();

        let staged = stage(&dmg, &Installation::Bundle(installed.clone()), &version).unwrap();
        assert_eq!(staged.staged, dir.join(".DuckLocal-update.app"));
        assert!(staged.staged.join("Contents/MacOS/ducklocal").is_file());
        // Not the version the dmg holds: refused, and nothing left behind.
        let error = stage(&dmg, &Installation::Bundle(installed.clone()), "99.0.0").unwrap_err();
        assert!(error.to_string().contains("not 99.0.0"), "{error:#}");
        assert!(!staged.staged.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_swap_puts_the_new_copy_in_place_or_the_old_one_back() {
        let dir = std::env::temp_dir().join(format!("ducklocal-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("DuckLocal.app")).unwrap();
        std::fs::create_dir_all(dir.join(".DuckLocal-update.app")).unwrap();
        std::fs::write(dir.join("DuckLocal.app/old"), b"").unwrap();
        std::fs::write(dir.join(".DuckLocal-update.app/new"), b"").unwrap();
        let staged = Staged {
            installed: dir.join("DuckLocal.app"),
            staged: dir.join(".DuckLocal-update.app"),
        };
        let old = swap(&staged).unwrap();
        assert!(dir.join("DuckLocal.app/new").exists());
        assert!(old.join("old").exists());
        assert!(!dir.join(".DuckLocal-update.app").exists());

        // Nothing staged any more: the second rename fails, and the
        // installed copy is back where it was.
        assert!(swap(&staged).is_err());
        assert!(dir.join("DuckLocal.app/new").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_package_is_unpacked_to_its_one_executable() {
        let dir = std::env::temp_dir().join(format!("ducklocal-find-{}", std::process::id()));
        let inner = dir.join("ducklocal-linux-x86_64");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("ducklocal"), b"").unwrap();
        std::fs::write(inner.join("README.md"), b"").unwrap();
        assert_eq!(
            find(&dir, "ducklocal".as_ref(), 2),
            Some(inner.join("ducklocal"))
        );
        assert_eq!(find(&dir, "ducklocal.exe".as_ref(), 2), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
