use std::os::windows::{
    ffi::OsStrExt,
    io::{AsRawHandle, IntoRawHandle},
};
use std::{fs, io, path::Path};
use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::{
        Console::{STD_ERROR_HANDLE, SetStdHandle},
        Threading::{CREATE_NO_WINDOW, CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW},
    },
};

pub fn spawn_background(exe: &Path) -> io::Result<()> {
    let program: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut command = std::ffi::OsString::from("\"");
    command.push(exe.as_os_str());
    command.push("\" daemon run --background");
    let mut command: Vec<u16> = command.encode_wide().chain(Some(0)).collect();
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();
    // SAFETY: all strings are NUL-terminated UTF-16 and remain alive during the
    // call. The mutable command buffer and output struct have the correct size.
    // No handles are inherited; the daemon opens its own log after startup.
    let created = unsafe {
        CreateProcessW(
            program.as_ptr(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut process,
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful creation returned owned process/thread handles. The
    // daemon runs independently, so this parent closes them without waiting.
    unsafe {
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
    }
    Ok(())
}

pub fn background_log(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("daemon.log"))?;
    // SAFETY: the file handle is valid. After success, its ownership is retained
    // for the entire process lifetime as the standard error handle. Windows
    // closes it at process termination, including when an error is reported.
    if unsafe { SetStdHandle(STD_ERROR_HANDLE, log.as_raw_handle()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _ = log.into_raw_handle();
    Ok(())
}
