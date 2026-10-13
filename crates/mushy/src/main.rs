mod runtime;
mod shell;
mod transport;
#[cfg(windows)]
mod windows;
use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use mushy_core::{engine, model::*, scan, store::Store};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(
    name = "mushy",
    version,
    about = "Soft links across devices, backed by automatic SSH sync"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// The ln -m shortcut for linking files across devices.
    Ln {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
        args: Vec<OsString>,
    },
    /// Set up the ln -m shortcut in your shell.
    Shell {
        #[command(subcommand)]
        command: shell::ShellCommand,
    },
    /// Link a remote file or directory. A new local destination must be absent or empty.
    Link {
        /// user@host:/path, or local destination when using --host and --remote-path.
        source: PathBuf,
        destination: Option<PathBuf>,
        #[arg(long, requires = "remote_path")]
        host: Option<String>,
        #[arg(long, requires = "host")]
        remote_path: Option<String>,
        #[arg(long)]
        exclude: Vec<String>,
        /// Keep the link registered without starting the background daemon.
        #[arg(long)]
        no_start: bool,
    },
    List,
    Status {
        path: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    Pause {
        path: PathBuf,
    },
    Resume {
        path: PathBuf,
    },
    /// Detach this device, keeping files and recovery history.
    Unlink {
        path: PathBuf,
    },
    History {
        path: PathBuf,
    },
    Restore {
        path: PathBuf,
        #[arg(long)]
        version: String,
        /// Export recovered content to an absent path without changing a linked root.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Auth {
        #[command(subcommand)]
        command: Auth,
    },
    Config {
        #[command(subcommand)]
        command: Config,
    },
    Doctor {
        host: Option<String>,
    },
    Daemon {
        #[command(subcommand)]
        command: Daemon,
    },
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        command: Internal,
    },
}
#[derive(Subcommand)]
enum Auth {
    Login { host: String },
    Forget { host: String },
}
#[derive(Subcommand)]
enum Config {
    Show,
    Set { key: String, value: u64 },
}
#[derive(Subcommand)]
enum Daemon {
    Run {
        #[arg(long, hide = true)]
        background: bool,
    },
    Start,
    Stop,
    Install,
}
#[derive(Subcommand)]
enum Internal {
    Serve,
}
fn main() {
    if let Err(e) = run() {
        eprintln!("mushy: {e:#}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    mushy_links::prepare_process()?;
    let cli = match Cli::parse() {
        Cli {
            command: Commands::Ln { args },
        } => Cli::parse_from(shell::link_args(args)?),
        cli => cli,
    };
    if let Commands::Shell { command } = &cli.command {
        return shell::run(command);
    }
    #[cfg(windows)]
    if matches!(
        cli.command,
        Commands::Daemon {
            command: Daemon::Run { background: true }
        }
    ) {
        windows::background_log(&runtime::state_dir()?)?;
    }
    if matches!(
        cli.command,
        Commands::Internal {
            command: Internal::Serve
        }
    ) {
        return transport::serve();
    }
    let pause_requested = matches!(cli.command, Commands::Pause { .. });
    let store = runtime::store()?;
    match cli.command {
        Commands::Ln { .. } | Commands::Shell { .. } => unreachable!(),
        Commands::Link {
            source,
            destination,
            host,
            remote_path,
            exclude,
            no_start,
        } => {
            let (host, remote, path) = if let (Some(host), Some(remote)) = (host, remote_path) {
                ensure!(
                    destination.is_none(),
                    "with --host/--remote-path, provide one positional local destination"
                );
                (host, remote, source)
            } else {
                let source = source.to_str().context("source must be UTF-8")?;
                let (h, p) = parse_source(source)?;
                (h, p, destination.context("provide a local destination")?)
            };
            link(&store, &host, &remote, &path, exclude, no_start)?;
        }
        Commands::List => {
            for r in store.roots()? {
                println!(
                    "{}  {}  {}{}",
                    r.id,
                    r.group,
                    r.path,
                    if r.paused { " [paused]" } else { "" }
                );
            }
        }
        Commands::Status { path, json } => {
            let filter = path.map(|p| scan::absolute(&p)).transpose()?;
            let statuses = store.statuses()?;
            let edges = store.edges()?;
            let mut rows = Vec::new();
            for r in store.roots()? {
                if filter.as_ref().is_some_and(|p| p.to_str() != Some(&r.path)) {
                    continue;
                }
                let peers:Vec<_>=edges.iter().filter(|e|e.root==r.id).map(|e|serde_json::json!({"host":e.host,"remote_path":e.remote_path,"status":statuses.get(&e.id)})).collect();
                let row = serde_json::json!({"id":r.id,"group":r.group,"path":r.path,"paused":r.paused,"status":statuses.get(&r.id),"peers":peers});
                if !json {
                    println!(
                        "{}: {}",
                        r.path,
                        if r.paused {
                            "paused"
                        } else {
                            statuses
                                .get(&r.id)
                                .and_then(|s| s["state"].as_str())
                                .unwrap_or("waiting")
                        }
                    );
                    if let Some(d) = statuses
                        .get(&r.id)
                        .and_then(|s| s["detail"].as_str())
                        .filter(|d| !d.is_empty())
                    {
                        println!("  {d}");
                    }
                    for e in edges.iter().filter(|e| e.root == r.id) {
                        println!(
                            "  {}:{} — {}",
                            e.host,
                            e.remote_path,
                            statuses
                                .get(&e.id)
                                .and_then(|s| s["state"].as_str())
                                .unwrap_or("waiting")
                        );
                    }
                }
                rows.push(row);
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            }
        }
        Commands::Pause { path } | Commands::Resume { path } => {
            let paused = pause_requested;
            let _g = store.lock()?;
            let mut root = find_root(&store, &path)?.0;
            root.paused = paused;
            store.save_root(&root)?;
            store.status(&root.id, if paused { "paused" } else { "waiting" }, "")?;
            drop(_g);
            if !paused {
                runtime::start()?;
            }
            println!(
                "{} {}",
                if paused { "Paused" } else { "Resumed" },
                root.path
            );
        }
        Commands::Unlink { path } => {
            let _g = store.lock()?;
            let (root, _) = find_root(&store, &path)?;
            store.unlink(&root)?;
            println!(
                "Unlinked {}. Files and recovery history retained.",
                root.path
            );
        }
        Commands::History { path } => {
            let (root, relative) = find_history_root(&store, &path)?;
            for c in store
                .changes(&root.group)?
                .into_iter()
                .filter(|c| relative.as_ref().is_none_or(|p| p == &c.path))
            {
                let available = c
                    .entry
                    .kind
                    .hash()
                    .map(|h| store.object(h).is_ok_and(|p| p.exists()))
                    .unwrap_or(true);
                println!(
                    "{}  {}  {}  {}{}",
                    c.id,
                    c.entry.modified_ns,
                    c.path,
                    match c.entry.kind {
                        Kind::Deleted => "deleted",
                        Kind::File { .. } => "file",
                        Kind::Directory => "directory",
                        Kind::Symlink { .. } => "symlink",
                    },
                    if available {
                        ""
                    } else {
                        " [content unavailable]"
                    }
                );
            }
        }
        Commands::Restore {
            path,
            version,
            output,
        } => {
            let _g = store.lock()?;
            let (root, relative) = find_history_root(&store, &path)?;
            let relative = relative
                .or_else(|| {
                    store
                        .changes(&root.group)
                        .ok()?
                        .into_iter()
                        .find(|c| c.id == version)
                        .map(|c| c.path)
                })
                .context("specify a file inside the linked root")?;
            if let Some(output) = output {
                let change = store
                    .changes(&root.group)?
                    .into_iter()
                    .find(|c| c.id == version && c.path == relative)
                    .context("version not found")?;
                let hash = change
                    .entry
                    .kind
                    .hash()
                    .context("--output exports regular file versions only")?;
                let mut input = fs::File::open(store.object(hash)?)
                    .context("recovery content has expired or is unavailable on this device")?;
                let mut out = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&output)?;
                std::io::copy(&mut input, &mut out)?;
                out.sync_all()?;
                println!("Exported recovered content to {}.", output.display());
            } else {
                ensure!(
                    store.root_at(&root.path)?.is_some(),
                    "root was unlinked; use --output NEW_PATH to export recovery content"
                );
                engine::restore(&store, &root, &relative, &version)?;
                println!("Restored {relative}; the change will sync automatically.");
            }
        }
        Commands::Auth {
            command: Auth::Login { host },
        } => runtime::auth_login(&host)?,
        Commands::Auth {
            command: Auth::Forget { host },
        } => {
            runtime::auth_forget(&host)?;
            println!("Forgot password for {host}.");
        }
        Commands::Config {
            command: Config::Show,
        } => println!("{}", serde_json::to_string_pretty(&store.limits()?)?),
        Commands::Config {
            command: Config::Set { key, value },
        } => {
            ensure!(value > 0, "limits must be positive");
            let _g = store.lock()?;
            let mut values = serde_json::to_value(store.limits()?)?;
            ensure!(
                values.get(&key).is_some(),
                "unknown configuration key; run mushy config show"
            );
            values[&key] = value.into();
            store.set_limits(&serde_json::from_value(values)?)?;
            println!("Set {key}={value}");
        }
        Commands::Doctor { host } => {
            println!(
                "mushy {}\nState: {}\nDevice: {}\nPlatform: {}",
                env!("CARGO_PKG_VERSION"),
                store.dir.display(),
                store.device()?,
                std::env::consts::OS
            );
            ensure!(
                std::process::Command::new("ssh")
                    .arg("-V")
                    .status()?
                    .success(),
                "OpenSSH is required"
            );
            if let Some(host) = host {
                let remote = transport::Session::connect(&host, &store.dir)?;
                println!(
                    "Remote helper protocol compatible; device {}",
                    remote.remote_device
                );
                println!(
                    "Clock difference: {} seconds (concurrent edits use device timestamps).",
                    remote.clock_skew_seconds
                );
            }
            println!("Diagnostics passed.");
        }
        Commands::Daemon {
            command: Daemon::Run { .. },
        } => runtime::daemon()?,
        Commands::Daemon {
            command: Daemon::Start,
        } => runtime::start()?,
        Commands::Daemon {
            command: Daemon::Stop,
        } => {
            fs::write(store.dir.join("daemon.stop"), b"stop")?;
            println!("Requested daemon stop.");
        }
        Commands::Daemon {
            command: Daemon::Install,
        } => {
            runtime::install_startup()?;
            runtime::start()?;
        }
        Commands::Internal { .. } => unreachable!(),
    }
    Ok(())
}
fn parse_source(source: &str) -> Result<(String, String)> {
    let index = if let Some(end) = source.find("]:") {
        end + 1
    } else {
        source.find(':').context("source must be HOST:PATH")?
    };
    let (host, path) = source.split_at(index);
    transport::validate_host(host)?;
    ensure!(path.len() > 1, "remote path cannot be empty");
    Ok((host.to_owned(), path[1..].to_owned()))
}
fn find_root(store: &Store, path: &Path) -> Result<(Root, Option<String>)> {
    find_in_roots(store.roots()?, path)
}
fn find_history_root(store: &Store, path: &Path) -> Result<(Root, Option<String>)> {
    if let Ok(found) = find_root(store, path) {
        return Ok(found);
    }
    find_in_roots(store.archived_roots()?, path)
}
fn find_in_roots(mut roots: Vec<Root>, path: &Path) -> Result<(Root, Option<String>)> {
    let abs = scan::reference_path(path)?;
    roots.sort_by_key(|r| std::cmp::Reverse(r.path.len()));
    for r in roots {
        let base = Path::new(&r.path);
        if abs == base {
            return Ok((
                r.clone(),
                if r.single_file {
                    Some("file".to_owned())
                } else {
                    None
                },
            ));
        }
        if !r.single_file && abs.starts_with(base) {
            let rel = abs
                .strip_prefix(base)?
                .to_str()
                .context("non UTF-8 path")?
                .replace('\\', "/");
            return Ok((r, Some(canonical_unicode(&rel))));
        }
    }
    bail!("path does not belong to a linked root")
}
fn link(
    store: &Store,
    host: &str,
    remote_path: &str,
    local: &Path,
    excludes: Vec<String>,
    no_start: bool,
) -> Result<()> {
    transport::validate_host(host)?;
    scan::exclusions(&excludes)?;
    let path = scan::absolute(local)?;
    let text = path.to_str().context("non UTF-8 root")?.to_owned();
    let existing = store.root_at(&text)?;
    ensure!(
        path.parent().is_some_and(|p| p.is_dir()),
        "local destination parent directory must exist"
    );
    if existing.is_none() && path.exists() {
        ensure!(
            if path.is_dir() {
                fs::read_dir(&path)?.next().is_none()
            } else {
                fs::metadata(&path)?.len() == 0
            },
            "new local destination must be absent or empty"
        );
    }
    let mut session=transport::Session::connect(host,&store.dir).context("SSH setup failed; trust the host with ssh HOST and use keys/agent or mushy auth login HOST")?;
    ensure!(
        session.remote_device != store.device()?,
        "cannot link to this device's own state"
    );
    eprintln!("Scanning remote content and checking limits…");
    let effective_excludes = existing
        .as_ref()
        .map(|r| r.excludes.clone())
        .unwrap_or(excludes);
    let (remote, usage, _) = session.probe(
        remote_path,
        existing.as_ref().map(|r| r.group.clone()),
        effective_excludes,
        existing.as_ref().map(|r| r.single_file),
    )?;
    usage.check(&store.limits()?)?;
    #[cfg(windows)]
    if usage.symlinks > 0 {
        preflight_windows_symlink(path.parent().context("local path needs a parent")?)?;
    }
    let root = existing.clone().unwrap_or_else(|| Root {
        id: uuid::Uuid::new_v4().to_string(),
        group: remote.group.clone(),
        path: text,
        single_file: remote.single_file,
        excludes: remote.excludes.clone(),
        paused: false,
    });
    ensure!(
        root.single_file == remote.single_file,
        "local and remote root types differ"
    );
    {
        let _g = store.lock()?;
        store.check_device(&root, &usage)?;
        // save_root validates overlapping roots and multiple roots per group before either side transfers data.
        store.save_root(&root)?;
    }
    if !no_start && let Err(e) = runtime::install_startup() {
        if existing.is_none() {
            let _g = store.lock()?;
            store.unlink(&root)?;
        }
        return Err(e);
    }
    if !path.exists() && !root.single_file {
        fs::create_dir(&path)?;
    }
    if existing.is_none() && root.single_file && path.exists() {
        fs::remove_file(&path)?;
    }
    session.activate(&remote)?;
    let edge = Edge {
        id: uuid::Uuid::new_v4().to_string(),
        root: root.id.clone(),
        host: host.to_owned(),
        remote_path: remote.path.clone(),
    };
    {
        let _g = store.lock()?;
        if !store
            .edges()?
            .iter()
            .any(|e| e.root == root.id && e.host == host && e.remote_path == remote.path)
        {
            store.save_edge(&edge)?;
        }
        store.status(&root.id, "syncing", "initial transfer")?;
    }
    eprintln!("Synchronizing initial content…");
    let result = session.sync(store, &root, &mut runtime::Watch::new(&root));
    if !no_start {
        runtime::start()?;
    }
    if let Err(e) = result {
        store.status_with_lock(
            &root.id,
            transport::classify(&format!("{e:#}")),
            &format!("{e:#}"),
        )?;
        return Err(e.context("link registered; synchronization will retry in the background"));
    }
    println!("Linked {} ↔ {}:{}", path.display(), host, remote.path);
    Ok(())
}
#[cfg(windows)]
fn preflight_windows_symlink(parent: &Path) -> Result<()> {
    let dir = tempfile::tempdir_in(parent)?;
    fs::write(dir.path().join("target"), b"")?;
    std::os::windows::fs::symlink_file("target", dir.path().join("link")).context(
        "Windows cannot create symlinks here; enable Developer Mode or grant symlink privilege",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_hosts_without_shell_interpretation() {
        assert_eq!(
            parse_source("user@box:/a b").unwrap(),
            ("user@box".into(), "/a b".into())
        );
        assert!(parse_source("-oProxyCommand=x:/a").is_err());
        assert!(parse_source("host;touch:/a").is_err());
        assert_eq!(parse_source("[::1]:/a").unwrap().0, "[::1]");
    }
}
