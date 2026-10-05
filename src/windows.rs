//! Windows startup: use the caller's console for CLI commands without
//! creating a console for GUI launches or editor-spawned language servers.

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
