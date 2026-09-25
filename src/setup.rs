//! Putting the `ducklocal` command on the PATH, and telling AI agents about it.
//!
//! Installing the app does neither: a `.app` bundle, a zip or a tarball puts
//! the executable somewhere no shell looks, and an agent that has never heard
//! of DuckLocal will not go looking. Both are one click from the window, in
//! the "Command line & AI" dialog.
//!
//! Blocking functions; call via `smol::unblock` from UI code. On macOS the
//! install may wait on the system's administrator prompt.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as _, Result};

/// The skill agents load to learn the CLI: `skills/ducklocal` in this
/// repository, compiled in so the app can install the copy that matches it.
const SKILL: &str = include_str!("../skills/ducklocal/SKILL.md");
const SKILL_CLI_REFERENCE: &str = include_str!("../skills/ducklocal/references/cli.md");

/// Where the command stands on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandStatus {
    /// `ducklocal` resolves to this copy of the app.
    Installed(PathBuf),
    /// The install location holds something else: another copy of DuckLocal
    /// (an older download, a source build), or a file that is not ours.
    /// macOS and Linux only; Windows adds a folder to Path instead.
    #[cfg_attr(windows, allow(dead_code))]
    Elsewhere(PathBuf),
    NotInstalled,
}

/// What `install_command` did, for the confirmation the window shows.
#[derive(Clone, Debug)]
pub struct Installed {
    pub path: PathBuf,
    /// Linux only: `~/.local/bin` is not on the PATH, so a new shell will not
    /// find the command until it is added.
    pub needs_path: bool,
}

/// This executable, resolved through symlinks. Not on Windows, where
/// canonicalizing yields a `\\?\C:\…` path that has no business in `Path`.
fn executable() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("Cannot locate the DuckLocal executable")?;
    if cfg!(windows) {
        return Ok(exe);
    }
    Ok(exe.canonicalize().unwrap_or(exe))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

// --- macOS and Linux: a symlink in a directory on the PATH -----------------

/// `/usr/local/bin` on macOS: on the default PATH of every shell, and where
/// `code`, `subl` and the rest put theirs. `~/.local/bin` on Linux: the XDG
/// place for a user's own commands, no root needed.
#[cfg(unix)]
fn link_path() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        Ok(PathBuf::from("/usr/local/bin/ducklocal"))
    } else {
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
        Ok(PathBuf::from(home).join(".local/bin/ducklocal"))
    }
}

#[cfg(unix)]
pub fn command_status() -> CommandStatus {
    let (Ok(link), Ok(exe)) = (link_path(), executable()) else {
        return CommandStatus::NotInstalled;
    };
    if std::fs::symlink_metadata(&link).is_err() {
        return CommandStatus::NotInstalled;
    }
    if same_file(&link, &exe) {
        CommandStatus::Installed(link)
    } else {
        CommandStatus::Elsewhere(link)
    }
}

#[cfg(unix)]
pub fn install_command() -> Result<Installed> {
    let exe = executable()?;
    let text = exe.to_string_lossy();
    // Opened straight from the disk image or a quarantined download, macOS
    // runs the app from a randomized read-only copy; a link to that breaks
    // the moment the image is ejected.
    if text.contains("/AppTranslocation/") || text.starts_with("/Volumes/") {
        bail!(crate::i18n::tr("setup.error.move_to_applications"));
    }
    let link = link_path()?;
    // Replace a link (an older copy's, most likely), never a real file: that
    // is somebody else's `ducklocal`, a package manager's perhaps.
    if let Ok(meta) = std::fs::symlink_metadata(&link) {
        if !meta.file_type().is_symlink() {
            bail!(crate::i18n::trf(
                "setup.error.not_ours",
                &[&link.display().to_string()]
            ));
        }
    }
    match link_directly(&exe, &link) {
        Ok(()) => {}
        #[cfg(target_os = "macos")]
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            link_as_administrator(&exe, &link)?
        }
        Err(e) => return Err(e).context(format!("Cannot create {}", link.display())),
    }
    let dir = link.parent().map(Path::to_path_buf).unwrap_or_default();
    let needs_path = !cfg!(target_os = "macos") && !on_path(&dir);
    Ok(Installed {
        path: link,
        needs_path,
    })
}

#[cfg(unix)]
fn link_directly(exe: &Path, link: &Path) -> std::io::Result<()> {
    if let Some(dir) = link.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::remove_file(link) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    std::os::unix::fs::symlink(exe, link)
}

/// `/usr/local/bin` belongs to root on a Mac without Homebrew there: ask for
/// the administrator password through the system's own prompt, the way other
/// editors install their commands. The paths travel as arguments, never
/// spliced into the script, so no quoting can break it.
#[cfg(target_os = "macos")]
fn link_as_administrator(exe: &Path, link: &Path) -> Result<()> {
    let dir = link.parent().unwrap_or(Path::new("/usr/local/bin"));
    let output = std::process::Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "do shell script \"mkdir -p \" & quoted form of (item 3 of argv) & \
             \" && ln -sfn \" & quoted form of (item 1 of argv) & \" \" & \
             quoted form of (item 2 of argv) with prompt \"DuckLocal wants to add \
             the ducklocal command to your terminal.\" with administrator privileges",
            "-e",
            "end run",
        ])
        .arg(exe)
        .arg(link)
        .arg(dir)
        .output()
        .context("Cannot run osascript")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // -128 is the user pressing Cancel on the password prompt.
    if stderr.contains("-128") {
        bail!(crate::i18n::tr("setup.error.cancelled"));
    }
    bail!("{}", stderr.trim())
}

#[cfg(unix)]
fn on_path(dir: &Path) -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|entry| same_file(&entry, dir)))
        .unwrap_or(false)
}

// --- Windows: the app's folder on the user's Path --------------------------

#[cfg(windows)]
pub fn command_status() -> CommandStatus {
    let Ok(exe) = executable() else {
        return CommandStatus::NotInstalled;
    };
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    match user_path() {
        Ok(path)
            if path
                .split(';')
                .any(|entry| same_file(Path::new(entry), &dir)) =>
        {
            CommandStatus::Installed(exe)
        }
        _ => CommandStatus::NotInstalled,
    }
}

/// The user-scope `Path`, read from the registry rather than this process's
/// environment, which does not see a change made after it started.
#[cfg(windows)]
fn user_path() -> Result<String> {
    powershell(
        "[Environment]::GetEnvironmentVariable('Path', 'User')",
        None,
    )
}

#[cfg(windows)]
pub fn install_command() -> Result<Installed> {
    let exe = executable()?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("The executable has no folder"))?
        .to_string_lossy()
        .into_owned();
    // Appended to the user's Path, not the machine's: no administrator
    // needed, and new terminals see it. The folder travels in the
    // environment, never spliced into the script, so no quoting can break it.
    powershell(
        "$dir = $env:DUCKLOCAL_DIR; \
         $p = [Environment]::GetEnvironmentVariable('Path', 'User'); \
         if (-not $p) { $p = '' }; \
         if (-not (($p -split ';') -contains $dir)) { \
           [Environment]::SetEnvironmentVariable('Path', ($p.TrimEnd(';') + ';' + $dir).TrimStart(';'), 'User') \
         }",
        Some(&dir),
    )?;
    Ok(Installed {
        path: exe,
        needs_path: false,
    })
}

#[cfg(windows)]
fn powershell(script: &str, dir: Option<&str>) -> Result<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = std::process::Command::new("powershell.exe");
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW);
    if let Some(dir) = dir {
        command.env("DUCKLOCAL_DIR", dir);
    }
    let output = command.output().context("Cannot run PowerShell")?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Whether to point the user at the dialog on this launch: once ever, and
/// only while the command is not installed. Marks the offer as made.
pub fn offer_once() -> bool {
    const SETTING: &str = "setup_offered";
    if crate::history::get_setting(SETTING)
        .ok()
        .flatten()
        .is_some()
    {
        return false;
    }
    if matches!(command_status(), CommandStatus::Installed(_)) {
        return false;
    }
    crate::history::set_setting(SETTING, "1").is_ok()
}

// --- AI agents -------------------------------------------------------------

/// `~/.claude/skills/ducklocal`: a user-level skill, so Claude Code finds it
/// in every project, not only the one it was copied into.
fn claude_skill_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| anyhow!("Cannot find the home folder"))?;
    Ok(PathBuf::from(home).join(".claude/skills/ducklocal"))
}

/// Whether the skill is installed, and whether it is this version's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkillStatus {
    Current(PathBuf),
    Outdated(PathBuf),
    NotInstalled,
}

pub fn claude_skill_status() -> SkillStatus {
    let Ok(dir) = claude_skill_dir() else {
        return SkillStatus::NotInstalled;
    };
    match std::fs::read_to_string(dir.join("SKILL.md")) {
        Ok(text) if text == SKILL => SkillStatus::Current(dir),
        Ok(_) => SkillStatus::Outdated(dir),
        Err(_) => SkillStatus::NotInstalled,
    }
}

/// Write the skill this build carries. Only its two files are written, so
/// anything else a user keeps in that folder survives an update.
pub fn install_claude_skill() -> Result<PathBuf> {
    install_skill_into(&claude_skill_dir()?)
}

fn install_skill_into(dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir.join("references"))
        .with_context(|| format!("Cannot create {}", dir.display()))?;
    std::fs::write(dir.join("SKILL.md"), SKILL)?;
    std::fs::write(dir.join("references/cli.md"), SKILL_CLI_REFERENCE)?;
    Ok(dir.to_path_buf())
}

/// A brief to paste into any other agent: what the command is, where it is,
/// and the habits that keep its answers honest. The full path goes in too,
/// so it works even before the command is on the PATH.
pub fn agent_prompt() -> String {
    let command = match command_status() {
        CommandStatus::Installed(_) => "ducklocal".to_string(),
        _ => executable()
            .map(|exe| format!("\"{}\"", exe.display()))
            .unwrap_or_else(|_| "ducklocal".to_string()),
    };
    format!(
        "DuckLocal is installed on this machine. Its command, {command}, runs DuckDB SQL \
         over local CSV, TSV, Parquet, JSON and Excel files and databases, and prints JSON.\n\
         \n\
         - Start with `{command} --help` for the options.\n\
         - Read a file's columns before querying it: \
         `{command} query --sql \"DESCRIBE SELECT * FROM 'data.csv'\"`.\n\
         - Profile what you will chart or summarize: `{command} profile data.csv`.\n\
         - Compute totals in SQL, not from sample rows; check `truncated` in the output.\n\
         - Use `--format md` for a table to show me, JSON when you parse it.\n\
         - It opens data read-only by default. Answer from the values it returns, \
         and say which query produced them.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skill_installs_its_files_and_keeps_others() {
        let dir = std::env::temp_dir().join(format!("ducklocal_skill_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.md"), "mine").unwrap();
        std::fs::write(dir.join("SKILL.md"), "an older skill").unwrap();

        install_skill_into(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("SKILL.md")).unwrap(),
            SKILL
        );
        assert!(dir.join("references/cli.md").exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("notes.md")).unwrap(),
            "mine"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_prompt_names_a_command_that_runs() {
        let prompt = agent_prompt();
        assert!(prompt.contains("--help"));
        assert!(prompt.contains("profile"));
        assert!(prompt.contains("DESCRIBE"));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_this_executable_counts_as_installed() {
        let dir = std::env::temp_dir().join(format!("ducklocal_link_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let link = dir.join("bin/ducklocal");
        link_directly(&executable().unwrap(), &link).unwrap();
        assert!(same_file(&link, &executable().unwrap()));
        // Re-linking replaces the old link rather than failing on it.
        link_directly(&executable().unwrap(), &link).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}
