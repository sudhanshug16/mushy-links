use crate::runtime::Watch;
use anyhow::{Context, Result, bail, ensure};
use mushy_core::{
    engine,
    model::*,
    protocol::{self, Request, Response},
    scan,
    store::Store,
};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub fn validate_host(host: &str) -> Result<()> {
    ensure!(
        !host.is_empty()
            && !host.starts_with('-')
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"@._-:[]".contains(&b)),
        "invalid SSH host; use a host alias from SSH config"
    );
    Ok(())
}
pub struct Session {
    child: Arc<Mutex<Child>>,
    input: ChildStdout,
    output: ChildStdin,
    alive: Arc<AtomicBool>,
    activity: Arc<AtomicU64>,
    pub remote_device: String,
    pub clock_skew_seconds: i64,
    errors: Arc<Mutex<String>>,
}
impl Session {
    pub fn connect(host: &str, state: &Path) -> Result<Self> {
        Self::connect_cancel(host, state, Arc::new(AtomicBool::new(false)))
    }
    fn connect_cancel(host: &str, state: &Path, cancel: Arc<AtomicBool>) -> Result<Self> {
        validate_host(host)?;
        let mut command = Command::new("ssh");
        if let Some(c) = std::env::var_os("MUSHY_SSH_CONFIG") {
            command.arg("-F").arg(c);
        }
        command.args([
            "-T",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ServerAliveInterval=10",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "NumberOfPasswordPrompts=1",
        ]);
        let askpass = std::env::current_exe()?.with_file_name(if cfg!(windows) {
            "mushy-askpass.exe"
        } else {
            "mushy-askpass"
        });
        if askpass.exists() {
            command
                .env("SSH_ASKPASS", askpass)
                .env("SSH_ASKPASS_REQUIRE", "force")
                .env("MUSHY_AUTH_HOST", host);
        } else {
            command.args(["-o", "BatchMode=yes"]);
        }
        // Paths are exclusively carried in framed stdin, never interpreted by a remote shell.
        command
            .arg(host)
            .arg("mushy internal serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command
            .spawn()
            .context("OpenSSH client 'ssh' is required")?;
        let input = child.stdout.take().context("no SSH stdout")?;
        let output = child.stdin.take().context("no SSH stdin")?;
        let mut err = child.stderr.take().context("no SSH stderr")?;
        let log = state.join(format!(
            "ssh-{}.log",
            &blake3::hash(host.as_bytes()).to_hex()[..16]
        ));
        let errors = Arc::new(Mutex::new(String::new()));
        let captured = errors.clone();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 2048];
            while let Ok(n) = err.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                if let Ok(mut text) = captured.lock() {
                    text.push_str(&String::from_utf8_lossy(&buffer[..n]));
                    if text.len() > 16384 {
                        let mut start = text.len() - 16384;
                        while !text.is_char_boundary(start) {
                            start += 1;
                        }
                        text.drain(..start);
                    }
                    let _ = fs::write(&log, text.as_bytes());
                }
            }
        });
        let child = Arc::new(Mutex::new(child));
        let alive = Arc::new(AtomicBool::new(true));
        let activity = Arc::new(AtomicU64::new(epoch_seconds()));
        let c = child.clone();
        let a = alive.clone();
        let t = activity.clone();
        std::thread::spawn(move || {
            while a.load(Ordering::Acquire) {
                if epoch_seconds().saturating_sub(t.load(Ordering::Acquire)) > 90
                    || cancel.load(Ordering::Acquire)
                {
                    if let Ok(mut c) = c.lock() {
                        let _ = c.kill();
                    }
                    break;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
        let mut session = Self {
            child,
            input,
            output,
            alive,
            activity,
            remote_device: String::new(),
            clock_skew_seconds: 0,
            errors,
        };
        match session.response()? {
            Response::Hello {
                version,
                device,
                time_ns,
                ..
            } if version == protocol::VERSION => {
                session.remote_device = device;
                session.clock_skew_seconds = now_ns().saturating_sub(time_ns) / 1_000_000_000;
            }
            Response::Hello { .. } => {
                bail!("protocol mismatch; install the same mushy version on both devices")
            }
            _ => bail!("invalid helper handshake"),
        }
        Ok(session)
    }
    fn touch(&self) {
        self.activity.store(epoch_seconds(), Ordering::Release);
    }
    pub fn request(&mut self, request: &Request) -> Result<()> {
        self.touch();
        protocol::write(&mut self.output, request)
    }
    pub fn response(&mut self) -> Result<Response> {
        let r = protocol::read(&mut self.input).map_err(|error| {
            let text = self
                .errors
                .lock()
                .map(|s| s.trim().to_owned())
                .unwrap_or_default();
            if text.is_empty() {
                error
            } else {
                error.context(text)
            }
        })?;
        self.touch();
        match r {
            Response::Error { message } => bail!("remote: {message}"),
            r => Ok(r),
        }
    }
    pub fn ok(&mut self) -> Result<()> {
        ensure!(
            matches!(self.response()?, Response::Ok),
            "invalid helper response"
        );
        Ok(())
    }
    pub fn probe(
        &mut self,
        path: &str,
        group: Option<String>,
        excludes: Vec<String>,
        single_file: Option<bool>,
    ) -> Result<(Root, Usage, bool)> {
        self.request(&Request::Probe {
            path: path.to_owned(),
            group,
            excludes,
            single_file,
        })?;
        match self.response()? {
            Response::Probe {
                root,
                usage,
                existing,
            } => Ok((root, usage, existing)),
            _ => bail!("invalid probe response"),
        }
    }
    pub fn activate(&mut self, root: &Root) -> Result<()> {
        self.request(&Request::Activate { root: root.clone() })?;
        self.ok()
    }
    pub fn sync(&mut self, store: &Store, root: &Root, watch: &mut Watch) -> Result<()> {
        {
            let _guard = store.lock()?;
            let current = store.root(&root.id)?;
            ensure!(!current.paused, "link is paused");
            if watch.needed()
                && let Err(e) = engine::observe(store, root)
            {
                watch.retry();
                return Err(e);
            }
        }
        let known = store.clock(&root.group)?;
        self.request(&Request::Snapshot { known })?;
        let remote_clock = loop {
            match self.response()? {
                Response::Changes { changes, .. } => {
                    let _g = store.lock()?;
                    store.import(&root.group, &changes)?;
                }
                Response::Ready { clock } => break clock,
                _ => bail!("invalid snapshot response"),
            }
        };
        {
            let _g = store.lock()?;
            engine::validate_heads(store, root)?;
        }
        let mut outbound = store.changes(&root.group)?;
        outbound.sort_by(|a, b| (&a.device, a.sequence).cmp(&(&b.device, b.sequence)));
        outbound.retain(|c| c.sequence > remote_clock.get(&c.device).copied().unwrap_or(0));
        for chunk in outbound.chunks(128) {
            self.request(&Request::Import {
                changes: chunk.to_vec(),
            })?;
            self.ok()?;
        }
        // Both peers have identical metadata before requesting winning content.
        for (hash, size) in engine::missing_objects(store, &root.group)? {
            self.request(&Request::Get { hash: hash.clone() })?;
            match self.response()? {
                Response::Object { size: n } if n == size => (),
                _ => bail!("invalid object size"),
            }
            receive_object(&mut self.input, store, &hash, size, &self.activity)?;
        }
        // Receiver tells us which live objects are absent, using an explicit content-request response.
        self.request(&Request::Apply)?;
        loop {
            match self.response()? {
                Response::Object { .. } => bail!("unexpected object response"),
                Response::Missing { objects } => {
                    for (hash, size) in objects {
                        self.request(&Request::Put {
                            hash: hash.clone(),
                            size,
                        })?;
                        self.ok()?;
                        let object = store.object(&hash)?;
                        let mut file = fs::File::open(&object)
                            .context("winning content not yet available on this peer")?;
                        copy_exact(&mut file, &mut self.output, size, &self.activity)?;
                        self.output.flush()?;
                        self.ok()?;
                    }
                    self.request(&Request::Apply)?;
                }
                Response::Ok => break,
                _ => bail!("invalid apply response"),
            }
        }
        {
            let _g = store.lock()?;
            let current = store.root(&root.id)?;
            ensure!(!current.paused, "link is paused");
            // Capture edits that happened during network I/O before resolving them.
            if watch.needed()
                && let Err(e) = engine::observe(store, root)
            {
                watch.retry();
                return Err(e);
            }
            engine::apply(store, root)?;
            store.status(&root.id, "up_to_date", "")?;
        }
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
fn epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn copy_exact(
    input: &mut impl Read,
    output: &mut impl Write,
    size: u64,
    activity: &AtomicU64,
) -> Result<()> {
    let mut remaining = size;
    let mut buf = [0u8; 65536];
    while remaining > 0 {
        let count = (remaining as usize).min(buf.len());
        let n = input.read(&mut buf[..count])?;
        ensure!(n > 0, "interrupted content transfer");
        output.write_all(&buf[..n])?;
        remaining -= n as u64;
        activity.store(epoch_seconds(), Ordering::Release);
    }
    Ok(())
}
fn receive_object(
    input: &mut impl Read,
    store: &Store,
    hash: &str,
    size: u64,
    activity: &AtomicU64,
) -> Result<()> {
    validate_hash(hash)?;
    ensure!(
        size <= store.limits()?.file_bytes,
        "incoming file exceeds limit"
    );
    ensure!(
        fs2::available_space(&store.dir)? > size.saturating_add(16 << 20),
        "insufficient free disk space for incoming content"
    );
    let mut temp = tempfile::NamedTempFile::new_in(store.dir.join("objects"))?;
    let mut h = blake3::Hasher::new();
    let mut left = size;
    let mut buf = [0u8; 65536];
    while left > 0 {
        let count = (left as usize).min(buf.len());
        let n = input.read(&mut buf[..count])?;
        ensure!(n > 0, "interrupted content transfer");
        h.update(&buf[..n]);
        temp.write_all(&buf[..n])?;
        left -= n as u64;
        activity.store(epoch_seconds(), Ordering::Release);
    }
    ensure!(
        h.finalize().to_hex().as_str() == hash,
        "content hash mismatch"
    );
    temp.as_file().sync_all()?;
    let _g = store.lock()?;
    let dest = store.object(hash)?;
    if !dest.exists() {
        temp.persist(&dest)?;
    }
    store.record_blob(hash, size)?;
    Ok(())
}
pub fn serve() -> Result<()> {
    let store = crate::runtime::store()?;
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    protocol::write(
        &mut output,
        &Response::Hello {
            version: protocol::VERSION,
            device: store.device()?,
            platform: std::env::consts::OS.to_owned(),
            time_ns: now_ns(),
        },
    )?;
    let mut active: Option<Root> = None;
    let mut probe: Option<Root> = None;
    let mut watch: Option<Watch> = None;
    loop {
        let request = match protocol::read::<Request>(&mut input) {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };
        if matches!(request, Request::Close) {
            return Ok(());
        }
        let result: Result<()> = (|| {
            match request {
                Request::Probe {
                    path,
                    group,
                    excludes,
                    single_file,
                } => {
                    let _g = store.lock()?;
                    let path = if path == "~"
                        || path.starts_with("~/")
                        || (cfg!(windows) && path.starts_with("~\\"))
                    {
                        let home =
                            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                                .context("remote home directory unavailable")?;
                        std::path::PathBuf::from(home).join(path.get(2..).unwrap_or(""))
                    } else {
                        std::path::PathBuf::from(path)
                    };
                    let path = scan::absolute(&path)?;
                    let text = path.to_str().context("non UTF-8 root")?.to_owned();
                    let existing = store.root_at(&text)?;
                    let root = if let Some(root) = existing.clone() {
                        ensure!(
                            group.as_ref().is_none_or(|g| g == &root.group),
                            "cannot merge different sync groups"
                        );
                        ensure!(
                            excludes.is_empty() || excludes == root.excludes,
                            "exclusions must match the existing group"
                        );
                        ensure!(!root.paused, "remote root is paused");
                        root
                    } else {
                        if group.is_some() && path.exists() {
                            let nonempty = if path.is_dir() {
                                fs::read_dir(&path)?.next().is_some()
                            } else {
                                fs::metadata(&path)?.len() > 0
                            };
                            ensure!(!nonempty, "new remote member must be absent or empty");
                        }
                        ensure!(
                            path.exists() || group.is_some(),
                            "remote seed path does not exist"
                        );
                        let single_file = if path.exists() {
                            path.is_file()
                        } else {
                            single_file.unwrap_or(false)
                        };
                        Root {
                            id: uuid::Uuid::new_v4().to_string(),
                            group: group.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                            path: text,
                            single_file,
                            excludes,
                            paused: false,
                        }
                    };
                    let usage = if path.exists() {
                        scan::scan(&store, &root, true)?.usage
                    } else {
                        Usage::default()
                    };
                    probe = Some(root.clone());
                    protocol::write(
                        &mut output,
                        &Response::Probe {
                            root,
                            usage,
                            existing: existing.is_some(),
                        },
                    )?;
                }
                Request::Activate { root } => {
                    ensure!(
                        probe.as_ref().is_some_and(|r| r.id == root.id
                            && r.path == root.path
                            && r.group == root.group),
                        "activation must match preflight"
                    );
                    let root = probe.clone().unwrap();
                    let _g = store.lock()?;
                    if !root.single_file && !Path::new(&root.path).exists() {
                        fs::create_dir(&root.path)?;
                    }
                    store.save_root(&root)?;
                    engine::observe(&store, &root)?;
                    active = Some(root.clone());
                    watch = Some(Watch::new(&root));
                    protocol::write(&mut output, &Response::Ok)?;
                }
                Request::Snapshot { known } => {
                    let root = active.as_ref().context("activate a root first")?;
                    let _g = store.lock()?;
                    let current = store.root(&root.id)?;
                    ensure!(!current.paused, "remote root paused");
                    if watch.as_mut().context("watch missing")?.needed()
                        && let Err(e) = engine::observe(&store, root)
                    {
                        watch.as_ref().unwrap().retry();
                        return Err(e);
                    }
                    let mut changes = store.changes(&root.group)?;
                    changes.sort_by(|a, b| (&a.device, a.sequence).cmp(&(&b.device, b.sequence)));
                    changes.retain(|c| c.sequence > known.get(&c.device).copied().unwrap_or(0));
                    for chunk in changes.chunks(128) {
                        protocol::write(
                            &mut output,
                            &Response::Changes {
                                changes: chunk.to_vec(),
                                done: false,
                            },
                        )?;
                    }
                    protocol::write(
                        &mut output,
                        &Response::Ready {
                            clock: store.clock(&root.group)?,
                        },
                    )?;
                }
                Request::Import { changes } => {
                    let root = active.as_ref().context("activate a root first")?;
                    let _g = store.lock()?;
                    ensure!(!store.root(&root.id)?.paused, "remote root paused");
                    store.import(&root.group, &changes)?;
                    engine::validate_heads(&store, root)?;
                    protocol::write(&mut output, &Response::Ok)?;
                }
                Request::Get { hash } => {
                    let root = active.as_ref().context("activate a root first")?;
                    ensure!(
                        store
                            .changes(&root.group)?
                            .iter()
                            .any(|c| c.entry.kind.hash() == Some(&hash)),
                        "object is outside this sync group"
                    );
                    let mut file = fs::File::open(store.object(&hash)?)
                        .context("winning content is unavailable on this peer")?;
                    let size = file.metadata()?.len();
                    protocol::write(&mut output, &Response::Object { size })?;
                    copy_exact(
                        &mut file,
                        &mut output,
                        size,
                        &AtomicU64::new(epoch_seconds()),
                    )?;
                    output.flush()?;
                }
                Request::Put { hash, size } => {
                    let root = active.as_ref().context("activate a root first")?;
                    ensure!(
                        engine::missing_objects(&store, &root.group)?
                            .contains(&(hash.clone(), size)),
                        "unsolicited object"
                    );
                    protocol::write(&mut output, &Response::Ok)?;
                    receive_object(
                        &mut input,
                        &store,
                        &hash,
                        size,
                        &AtomicU64::new(epoch_seconds()),
                    )?;
                    protocol::write(&mut output, &Response::Ok)?;
                }
                Request::Apply => {
                    let root = active.as_ref().context("activate a root first")?;
                    let _g = store.lock()?;
                    ensure!(!store.root(&root.id)?.paused, "remote root paused");
                    engine::validate_heads(&store, root)?;
                    let missing = engine::missing_objects(&store, &root.group)?;
                    if missing.is_empty() {
                        if watch.as_mut().context("watch missing")?.needed()
                            && let Err(e) = engine::observe(&store, root)
                        {
                            watch.as_ref().unwrap().retry();
                            return Err(e);
                        }
                        engine::apply(&store, root)?;
                        store.status(&root.id, "up_to_date", "")?;
                        protocol::write(&mut output, &Response::Ok)?;
                    } else {
                        protocol::write(&mut output, &Response::Missing { objects: missing })?;
                    }
                }
                Request::Close => unreachable!(),
            }
            Ok(())
        })();
        if let Err(e) = result {
            if let Some(root) = &active {
                let _ = store.status(&root.id, classify(&format!("{e:#}")), &format!("{e:#}"));
            }
            protocol::write(
                &mut output,
                &Response::Error {
                    message: format!("{e:#}"),
                },
            )?;
            // Errors may occur within a binary payload; close rather than desynchronize framing.
            return Ok(());
        }
    }
}
pub fn classify(message: &str) -> &'static str {
    if message.contains("Permission denied")
        || message.contains("Host key verification failed")
        || message.contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
    {
        "authentication_required"
    } else if message.contains("limit exceeded") || message.contains("budget") {
        "limit_exceeded"
    } else if message.contains("SSH")
        || message.contains("connection")
        || message.contains("helper closed")
    {
        "offline"
    } else {
        "filesystem_error"
    }
}
pub fn worker(dir: &Path, edge: &Edge, stop: Arc<AtomicBool>) -> Result<()> {
    let store = Store::open(dir)?;
    let mut backoff = 1u64;
    loop {
        if stop.load(Ordering::Acquire) || dir.join("daemon.stop").exists() {
            return Ok(());
        }
        if !store.edges()?.iter().any(|e| e.id == edge.id) {
            return Ok(());
        }
        let root = store.root(&edge.root)?;
        if root.paused {
            return Ok(());
        }
        let result: Result<()> = (|| {
            store.status(&edge.id, "syncing", "")?;
            let mut session = Session::connect_cancel(&edge.host, dir, stop.clone())?;
            ensure!(
                session.remote_device != store.device()?,
                "cannot link a device to itself; cloned installations need distinct state directories"
            );
            let (remote, _, _) = session.probe(
                &edge.remote_path,
                Some(root.group.clone()),
                root.excludes.clone(),
                Some(root.single_file),
            )?;
            ensure!(root.single_file == remote.single_file, "root types differ");
            session.activate(&remote)?;
            let mut watch = Watch::new(&root);
            loop {
                if stop.load(Ordering::Acquire) || dir.join("daemon.stop").exists() {
                    return Ok(());
                }
                if !store.edges()?.iter().any(|e| e.id == edge.id) || store.root(&root.id)?.paused {
                    return Ok(());
                }
                session.sync(&store, &root, &mut watch)?;
                backoff = 1;
                store.status(&edge.id, "up_to_date", "")?;
                std::thread::sleep(Duration::from_millis(750));
            }
        })();
        if let Err(e) = result {
            let detail = format!("{e:#}");
            store.status(&edge.id, classify(&detail), &detail)?;
            store.status(&root.id, classify(&detail), &detail)?;
            let wait = Instant::now();
            let jitter = (uuid::Uuid::new_v4().as_u128() % 250) as u64;
            while wait.elapsed() < Duration::from_millis(backoff * 1000 + jitter) {
                if stop.load(Ordering::Acquire) || dir.join("daemon.stop").exists() {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            backoff = (backoff * 2).min(60);
        } else {
            return Ok(());
        }
    }
}
