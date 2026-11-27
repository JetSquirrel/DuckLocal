//! Windows startup and auto-update helpers: attach the caller's console for
//! CLI commands, wait for an exiting parent process during in-place restarts,
//! schedule reboot cleanup for locked binaries, and run the elevated UAC
//! swap helper when installed in a protected folder.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

pub(crate) const UPDATE_WAIT_PID_ENV: &str = "DUCKLOCAL_UPDATE_WAIT_PID";

pub(crate) fn attach_cli_console() {
    use windows_sys::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_UNKNOWN};
    use windows_sys::Win32::System::Console::{
        AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE,
        STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    // AttachConsole replaces standard handles. Keep inherited pipes/files
    // intact: SQL from stdin, JSON redirection and the LSP transport depend
    // on these, and they may be valid even when no parent console exists.
    // SAFETY: these Win32 calls borrow process-owned handles; none are closed.
    unsafe {
        let handles = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].map(|id| {
            let handle = GetStdHandle(id);
            (id, handle, GetFileType(handle) != FILE_TYPE_UNKNOWN)
        });
        if AttachConsole(ATTACH_PARENT_PROCESS) != 0 {
            for (id, handle, valid) in handles {
                if valid {
                    SetStdHandle(id, handle);
                }
            }
        }
    }
}

/// If launched by a previous DuckLocal process restarting after an update,
/// wait up to 15 seconds for that parent process to exit so file locks on
/// `history.duckdb`, user databases, `ducklocal.log`, and `.old-*` binaries
/// are released before startup proceeds.
pub(crate) fn wait_for_update_parent() {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    const MAX_WAIT_MS: u32 = 15_000;

    let Some(raw_pid) = std::env::var_os(UPDATE_WAIT_PID_ENV) else {
        return;
    };
    // Remove immediately so child processes do not inherit the marker.
    std::env::remove_var(UPDATE_WAIT_PID_ENV);

    let Some(pid) = raw_pid.to_str().and_then(|s| s.trim().parse::<u32>().ok()) else {
        return;
    };
    if pid == 0 || pid == std::process::id() {
        return;
    }

    // SAFETY: `OpenProcess` with `PROCESS_SYNCHRONIZE` returns either null
    // (already exited or inaccessible) or a valid handle that we wait on and
    // close immediately afterwards.
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if !handle.is_null() {
            let _ = WaitForSingleObject(handle, MAX_WAIT_MS);
            CloseHandle(handle);
        }
    }
}

/// Ask Windows to remove `path` on the next reboot (used by the elevated
/// helper for locked `.old-*` executables inside protected folders like
/// `Program Files`).
pub(crate) fn schedule_delete_on_reboot(path: &Path) -> std::io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_DELAY_UNTIL_REBOOT};

    let wide = encode_wide(path.as_os_str());
    // SAFETY: `wide` is a valid NUL-terminated UTF-16 string and `null()` tells
    // `MoveFileExW` to delete the existing file on reboot.
    let ok = unsafe { MoveFileExW(wide.as_ptr(), std::ptr::null(), MOVEFILE_DELAY_UNTIL_REBOOT) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Outcome of running the elevated `__apply-update` helper via UAC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ElevatedOutcome {
    Succeeded,
    Cancelled,
}

/// Launch `exe __apply-update --src <src> --dst <dst> --pid <pid>` elevated
/// via `ShellExecuteExW("runas")` with a hidden window, wait for it to finish,
/// and return whether it succeeded or the user cancelled the UAC prompt.
pub(crate) fn run_elevated_apply_helper(
    exe: &Path,
    src: &Path,
    dst: &Path,
    pid: u32,
) -> anyhow::Result<ElevatedOutcome> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_CANCELLED, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    const HELPER_TIMEOUT_MS: u32 = 60_000;

    let verb = encode_wide(OsStr::new("runas"));
    let file = encode_wide(exe.as_os_str());
    let params = build_apply_update_params(src, dst, pid);

    // SAFETY: `info` is zero-initialized with `cbSize` set; all wide string
    // pointers remain alive across the `ShellExecuteExW` call, and `hProcess`
    // is closed before returning.
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = SW_HIDE;

        if ShellExecuteExW(&mut info) == 0 {
            let err = GetLastError();
            if err == ERROR_CANCELLED {
                return Ok(ElevatedOutcome::Cancelled);
            }
            anyhow::bail!(
                "failed to launch elevated updater: {}",
                std::io::Error::from_raw_os_error(err as i32)
            );
        }

        if info.hProcess.is_null() {
            anyhow::bail!("elevated updater did not return a process handle");
        }

        let wait = WaitForSingleObject(info.hProcess, HELPER_TIMEOUT_MS);
        let mut exit_code: u32 = 1;
        let (got_code, code_err) = if wait == WAIT_OBJECT_0 {
            let ok = GetExitCodeProcess(info.hProcess, &mut exit_code);
            let err = if ok == 0 {
                Some(std::io::Error::last_os_error())
            } else {
                None
            };
            (ok, err)
        } else {
            (0, None)
        };
        CloseHandle(info.hProcess);

        if wait != WAIT_OBJECT_0 {
            anyhow::bail!("elevated updater timed out or failed to wait (status {wait:#x})");
        }
        if got_code == 0 {
            let err = code_err.unwrap_or_else(std::io::Error::last_os_error);
            anyhow::bail!("cannot read elevated updater exit code: {err}");
        }
        if exit_code != 0 {
            anyhow::bail!("elevated updater failed with exit code {exit_code}");
        }
        Ok(ElevatedOutcome::Succeeded)
    }
}

fn encode_wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn build_apply_update_params(src: &Path, dst: &Path, pid: u32) -> Vec<u16> {
    let pid_text = pid.to_string();
    let args: [&OsStr; 7] = [
        OsStr::new("__apply-update"),
        OsStr::new("--src"),
        src.as_os_str(),
        OsStr::new("--dst"),
        dst.as_os_str(),
        OsStr::new("--pid"),
        OsStr::new(&pid_text),
    ];
    let mut out = Vec::new();
    for (idx, arg) in args.iter().enumerate() {
        if idx > 0 {
            out.push(b' ' as u16);
        }
        append_quoted_arg_wide(&mut out, arg);
    }
    out.push(0);
    out
}

/// Quote a single Windows command-line argument according to
/// `CommandLineToArgvW` rules (handling spaces, quotes, and backslashes).
fn append_quoted_arg_wide(out: &mut Vec<u16>, arg: &OsStr) {
    out.push(b'"' as u16);
    let mut backslashes = 0usize;
    for unit in arg.encode_wide() {
        if unit == b'\\' as u16 {
            backslashes += 1;
        } else if unit == b'"' as u16 {
            out.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2 + 1));
            out.push(b'"' as u16);
            backslashes = 0;
        } else {
            out.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
            backslashes = 0;
            out.push(unit);
        }
    }
    out.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2));
    out.push(b'"' as u16);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elevated_params_quote_spaces_and_trailing_backslashes() {
        let wide = build_apply_update_params(
            Path::new(r"C:\Users\Alice Smith\AppData\Local\DuckLocal\cache\updates\ducklocal.exe"),
            Path::new(r"C:\Program Files\DuckLocal\ducklocal.exe"),
            4242,
        );
        assert_eq!(wide.last(), Some(&0));
        let text = String::from_utf16(&wide[..wide.len() - 1]).unwrap();
        assert_eq!(
            text,
            r#""__apply-update" "--src" "C:\Users\Alice Smith\AppData\Local\DuckLocal\cache\updates\ducklocal.exe" "--dst" "C:\Program Files\DuckLocal\ducklocal.exe" "--pid" "4242""#
        );

        let mut quoted = Vec::new();
        append_quoted_arg_wide(&mut quoted, OsStr::new(r#"C:\dir with "quotes"\"#));
        let quoted_text = String::from_utf16(&quoted).unwrap();
        assert_eq!(quoted_text, r#""C:\dir with \"quotes\"\\""#);
    }
}
