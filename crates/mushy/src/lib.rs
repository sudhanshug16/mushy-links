/// Keep redirected caller streams out of unrelated child processes. Rust
/// explicitly duplicates the selected standard streams when spawning a child.
/// On Windows, original inheritable pipes can otherwise leak into a detached
/// daemon and keep callers waiting for EOF after the CLI has exited.
pub fn prepare_process() -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::{HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation},
            Storage::FileSystem::{FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType},
            System::Console::{
                GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
            },
        };
        for stream in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: these APIs inspect the process's existing standard
            // handles and change only their inheritance flags. No ownership is
            // taken, handles closed, or standard-stream bindings changed.
            unsafe {
                let handle = GetStdHandle(stream);
                if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                    continue;
                }
                if matches!(GetFileType(handle), FILE_TYPE_PIPE | FILE_TYPE_DISK)
                    && SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) == 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
        }
    }
    Ok(())
}
