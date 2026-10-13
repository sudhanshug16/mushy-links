#![cfg_attr(windows, windows_subsystem = "windows")]
fn main() {
    let result = (|| -> std::io::Result<()> {
        mushy_links::prepare_process()?;
        let exe = std::env::current_exe()?.with_file_name(if cfg!(windows) {
            "mushy.exe"
        } else {
            "mushy"
        });
        let mut command = std::process::Command::new(exe);
        command
            .args(["daemon", "start"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let status = command.status()?;
        if !status.success() {
            return Err(std::io::Error::other("mushy background startup failed"));
        }
        Ok(())
    })();
    if result.is_err() {
        std::process::exit(1);
    }
}
