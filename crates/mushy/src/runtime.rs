use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use mushy_core::{model::Root, store::Store};
use notify::{RecursiveMode, Watcher};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub fn state_dir() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("MUSHY_STATE_DIR") {
        return Ok(PathBuf::from(p));
    }
    Ok(ProjectDirs::from("dev", "mushy", "mushy-links")
        .context("cannot determine user data directory")?
        .data_local_dir()
        .to_owned())
}
pub fn store() -> Result<Store> {
    Store::open(&state_dir()?)
}
pub struct Watch {
    pub dirty: Arc<AtomicBool>,
    _watcher: Option<notify::RecommendedWatcher>,
    last: Instant,
}
impl Watch {
    pub fn new(root: &Root) -> Self {
        let dirty = Arc::new(AtomicBool::new(true));
        let d = dirty.clone();
        let mut watcher = notify::recommended_watcher(move |_: notify::Result<notify::Event>| {
            d.store(true, Ordering::Release);
        })
        .ok();
        let path = Path::new(&root.path);
        let path = if root.single_file {
            path.parent().unwrap_or(path)
        } else {
            path
        };
        if let Some(w) = watcher.as_mut()
            && w.watch(path, RecursiveMode::Recursive).is_err()
        {
            watcher = None;
        }
        Self {
            dirty,
            _watcher: watcher,
            last: Instant::now(),
        }
    }
    pub fn needed(&mut self) -> bool {
        if self.dirty.swap(false, Ordering::AcqRel) || self.last.elapsed() > Duration::from_secs(60)
        {
            self.last = Instant::now();
            true
        } else {
            false
        }
    }
    pub fn retry(&self) {
        self.dirty.store(true, Ordering::Release);
    }
}
#[cfg(target_os = "linux")]
fn quote_systemd(p: &Path) -> String {
    format!(
        "\"{}\"",
        p.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    )
}
#[cfg(target_os = "linux")]
fn quote_desktop(p: &Path) -> String {
    let mut quoted = String::from("\"");
    for c in p.to_string_lossy().chars() {
        match c {
            '\\' => quoted.push_str("\\\\\\\\"),
            '"' | '`' | '$' => {
                quoted.push_str("\\\\");
                quoted.push(c);
            }
            '%' => quoted.push_str("%%"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}
/// Register only in the real user profile; test state directories never change login configuration.
pub fn install_startup() -> Result<()> {
    if std::env::var_os("MUSHY_STATE_DIR").is_some() {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    #[cfg(target_os = "linux")]
    {
        let home = std::env::var_os("HOME").context("HOME unavailable")?;
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or(PathBuf::from(home).join(".config"));
        if Command::new("systemctl")
            .args(["--user", "show-environment"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
        {
            let dir = config.join("systemd/user");
            fs::create_dir_all(&dir)?;
            fs::write(
                dir.join("mushy.service"),
                format!(
                    "[Unit]\nDescription=Mushy SSH file synchronization\n[Service]\nExecStart={} daemon run\nRestart=on-failure\nRestartSec=5\n[Install]\nWantedBy=default.target\n",
                    quote_systemd(&exe)
                ),
            )?;
            for args in [
                ["--user", "daemon-reload"].as_slice(),
                ["--user", "enable", "--now", "mushy.service"].as_slice(),
            ] {
                if !Command::new("systemctl").args(args).status()?.success() {
                    bail!("could not enable user startup; run mushy daemon run for diagnostics");
                }
            }
        } else {
            let dir = config.join("autostart");
            fs::create_dir_all(&dir)?;
            fs::write(
                dir.join("mushy.desktop"),
                format!(
                    "[Desktop Entry]\nType=Application\nName=Mushy Links\nExec={} daemon run\nTerminal=false\n",
                    quote_desktop(&exe)
                ),
            )?;
        }
    }
    #[cfg(target_os = "macos")]
    {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME unavailable")?);
        let dir = home.join("Library/LaunchAgents");
        fs::create_dir_all(&dir)?;
        let p = dir.join("dev.mushy.links.plist");
        let escaped = exe
            .to_string_lossy()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        fs::write(
            &p,
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>dev.mushy.links</string><key>ProgramArguments</key><array><string>{escaped}</string><string>daemon</string><string>run</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict></dict></plist>"
            ),
        )?;
        let uid = Command::new("id").arg("-u").output()?;
        if !uid.status.success() {
            bail!("cannot determine login user ID");
        }
        let uid = String::from_utf8(uid.stdout)?.trim().to_owned();
        let gui = format!("gui/{uid}");
        let domain = if Command::new("launchctl")
            .args(["print", &gui])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success()
        {
            gui
        } else {
            format!("user/{uid}")
        };
        if !Command::new("launchctl")
            .args(["print", &format!("{domain}/dev.mushy.links")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success()
        {
            let status = Command::new("launchctl")
                .args(["bootstrap", &domain])
                .arg(&p)
                .status()?;
            if !status.success() {
                bail!("could not load login agent; inspect {}", p.display());
            }
        }
    }
    #[cfg(windows)]
    {
        let background = exe.with_file_name("mushy-background.exe");
        if !background.exists() {
            bail!("mushy-background.exe must be installed beside mushy.exe");
        }
        let task = format!("\"{}\"", background.display());
        if !Command::new("reg")
            .args([
                "add",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "MushyLinks",
                "/t",
                "REG_SZ",
                "/d",
                &task,
                "/f",
            ])
            .status()?
            .success()
        {
            bail!("could not register per-user login startup");
        }
    }

    Ok(())
}
pub fn start() -> Result<()> {
    let s = store()?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(s.dir.join("daemon.lock"))?;
    if fs2::FileExt::try_lock_exclusive(&lock).is_err() {
        return Ok(());
    }
    fs2::FileExt::unlock(&lock)?;
    let _ = fs::remove_file(s.dir.join("daemon.stop"));
    let log = s.dir.join("daemon.log");
    if fs::metadata(&log).is_ok_and(|m| m.len() > 2 * 1024 * 1024) {
        let _ = fs::rename(&log, s.dir.join("daemon.previous.log"));
    }
    #[cfg(windows)]
    {
        // A detached daemon must not inherit any handles from an installer or
        // another caller capturing output, including non-standard pipe handles.
        crate::windows::spawn_background(&std::env::current_exe()?)?;
    }
    #[cfg(not(windows))]
    {
        let out = fs::OpenOptions::new().create(true).append(true).open(log)?;
        let mut c = Command::new(std::env::current_exe()?);
        c.args(["daemon", "run"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(out);
        c.spawn()?;
    }
    Ok(())
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("cannot remove {}", path.display())),
    }
}

/// Match installation's isolation rule: custom state never changes login startup.
pub fn uninstall_startup() -> Result<()> {
    if std::env::var_os("MUSHY_STATE_DIR").is_some() {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let home = std::env::var_os("HOME").context("HOME unavailable")?;
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or(PathBuf::from(home).join(".config"));
        let service = config.join("systemd/user/mushy.service");
        let manager_available = Command::new("systemctl")
            .args(["--user", "show-environment"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if service.exists()
            && manager_available
            && !Command::new("systemctl")
                .args(["--user", "disable", "--now", "mushy.service"])
                .status()?
                .success()
        {
            bail!("cannot disable mushy's user service; executables retained");
        }
        remove_if_present(&config.join("systemd/user/default.target.wants/mushy.service"))?;
        remove_if_present(&service)?;
        remove_if_present(&config.join("autostart/mushy.desktop"))?;
        if manager_available
            && !Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .status()?
                .success()
        {
            bail!("cannot reload user services");
        }
    }
    #[cfg(target_os = "macos")]
    {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME unavailable")?);
        let uid = Command::new("id").arg("-u").output()?;
        if !uid.status.success() {
            bail!("cannot determine login user ID");
        }
        let uid = String::from_utf8(uid.stdout)?.trim().to_owned();
        for domain in [
            format!("gui/{uid}/dev.mushy.links"),
            format!("user/{uid}/dev.mushy.links"),
        ] {
            if Command::new("launchctl")
                .args(["print", &domain])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?
                .success()
                && !Command::new("launchctl")
                    .args(["bootout", &domain])
                    .status()?
                    .success()
            {
                bail!("cannot unload mushy's login agent; executables retained");
            }
        }
        remove_if_present(&home.join("Library/LaunchAgents/dev.mushy.links.plist"))?;
    }
    #[cfg(windows)]
    {
        let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
        if Command::new("reg")
            .args(["query", key, "/v", "MushyLinks"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success()
            && !Command::new("reg")
                .args(["delete", key, "/v", "MushyLinks", "/f"])
                .status()?
                .success()
        {
            bail!("cannot remove mushy's login startup; executables retained");
        }
    }
    Ok(())
}

pub fn stop_and_wait() -> Result<()> {
    let dir = state_dir()?;
    if !dir.exists() {
        return Ok(());
    }
    fs::write(dir.join("daemon.stop"), b"stop")?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("daemon.lock"))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match fs2::FileExt::try_lock_exclusive(&lock) {
            Ok(()) => {
                fs2::FileExt::unlock(&lock)?;
                return Ok(());
            }
            Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => (),
            Err(e) => return Err(e).context("cannot check daemon shutdown"),
        }
        if Instant::now() >= deadline {
            bail!("daemon is still stopping; executables retained. Wait and rerun uninstall");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
pub fn daemon() -> Result<()> {
    let s = store()?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(s.dir.join("daemon.lock"))?;
    if fs2::FileExt::try_lock_exclusive(&lock).is_err() {
        return Ok(());
    }
    let _ = fs::remove_file(s.dir.join("daemon.stop"));
    let quit = Arc::new(AtomicBool::new(false));
    let q = quit.clone();
    ctrlc::set_handler(move || q.store(true, Ordering::Release))?;
    let mut workers = std::collections::BTreeMap::new();
    while !quit.load(Ordering::Acquire) && !s.dir.join("daemon.stop").exists() {
        for edge in s.edges()? {
            let Ok(root) = s.root(&edge.root) else {
                continue;
            };
            if root.paused {
                continue;
            }
            if workers
                .get(&edge.id)
                .is_some_and(|h: &std::thread::JoinHandle<()>| !h.is_finished())
            {
                continue;
            }
            let id = edge.id.clone();
            let stop = quit.clone();
            let dir = s.dir.clone();
            workers.insert(
                id,
                std::thread::spawn(move || {
                    if let Err(e) = crate::transport::worker(&dir, &edge, stop) {
                        eprintln!("sync worker {}: {e:#}", edge.id);
                    }
                }),
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    quit.store(true, Ordering::Release);
    for (_, worker) in workers {
        let _ = worker.join();
    }
    Ok(())
}
pub fn auth_login(host: &str) -> Result<()> {
    crate::transport::validate_host(host)?;
    let password = rpassword::prompt_password(format!(
        "SSH password for {host} (saved in OS secure store): "
    ))?;
    use std::io::Write;
    let mut child = credential_command(host)?
        .arg("--store")
        .stdin(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .context("no credential helper stdin")?
        .write_all(password.as_bytes())?;
    if !child.wait()?.success() {
        bail!("OS secure credential store is required and must be unlocked");
    }
    println!("Saved SSH password for {host} in the OS secure store.");
    Ok(())
}
pub fn auth_forget(host: &str) -> Result<()> {
    crate::transport::validate_host(host)?;
    if !credential_command(host)?
        .arg("--forget")
        .status()?
        .success()
    {
        bail!("could not remove credential from the OS secure store");
    }
    Ok(())
}
fn credential_command(host: &str) -> Result<Command> {
    // The same executable writes and reads the item, so macOS Keychain trusts
    // the askpass helper when SSH requests the saved password later.
    let helper = std::env::current_exe()?.with_file_name(if cfg!(windows) {
        "mushy-askpass.exe"
    } else {
        "mushy-askpass"
    });
    if !helper.is_file() {
        bail!("mushy-askpass must be installed beside mushy");
    }
    let mut command = Command::new(helper);
    command
        .env("MUSHY_AUTH_HOST", host)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    Ok(command)
}
