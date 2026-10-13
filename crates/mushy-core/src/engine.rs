use crate::{
    model::*,
    scan::{self, Snapshot},
    store::Store,
};
use anyhow::{Context, Result, bail};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::Path,
};

pub fn observe(store: &Store, root: &Root) -> Result<Snapshot> {
    if root.paused {
        bail!("link is paused");
    }
    let snapshot = scan::scan(store, root, true)?;
    let tx = store.db.unchecked_transaction()?;
    let device = store.device()?;
    let mut clock = store.clock(&root.group)?;
    // A filesystem mutation may have committed immediately before a crash.
    for pending in store.pending(&root.id)? {
        let current = snapshot
            .entries
            .get(&pending.path)
            .map(|e| &e.kind)
            .unwrap_or(&Kind::Deleted);
        if current == &pending.entry.kind {
            store.remember(&root.id, &pending)?;
        } else {
            store.clear_pending(&root.id, &pending.path)?;
        }
    }
    let applied = store.applied(&root.id)?;
    let mut paths: BTreeSet<String> = snapshot.entries.keys().cloned().collect();
    paths.extend(applied.keys().cloned());
    for path in paths {
        let current = snapshot.entries.get(&path).cloned().unwrap_or(Entry {
            kind: Kind::Deleted,
            modified_ns: now_ns(),
        });
        let old = applied.get(&path);
        // Directory mtimes are incidental to child mutations; content hashes identify files.
        if old.is_some_and(|c| c.entry.kind == current.kind) {
            continue;
        }
        if old.is_none() && current.kind == Kind::Deleted {
            continue;
        }
        if let Some(hash) = old.and_then(|c| c.entry.kind.hash())
            && current.kind.hash() != Some(hash)
        {
            let old_size = old.unwrap().entry.kind.size();
            store.reserve_recovery(old_size)?;
            if store.object(hash)?.exists() {
                store.record_blob(hash, old_size)?;
            }
        }
        let sequence = clock
            .get(&device)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .context("device sequence exhausted")?;
        let change = Change {
            id: format!("{device}:{sequence}"),
            group: root.group.clone(),
            device: device.clone(),
            sequence,
            context: clock.clone(),
            path,
            entry: current,
        };
        store.append(&change)?;
        clock.insert(device.clone(), sequence);
        store.remember(&root.id, &change)?;
    }
    tx.commit()?;
    Ok(snapshot)
}
pub fn new_change(store: &Store, group: &str, path: &str, entry: Entry) -> Result<Change> {
    let context = store.clock(group)?;
    let device = store.device()?;
    let sequence = context
        .get(&device)
        .copied()
        .unwrap_or(0)
        .checked_add(1)
        .context("device sequence exhausted")?;
    Ok(Change {
        id: format!("{device}:{sequence}"),
        group: group.to_owned(),
        device,
        sequence,
        context,
        path: path.to_owned(),
        entry,
    })
}
pub fn missing_objects(store: &Store, group: &str) -> Result<Vec<(String, u64)>> {
    let limits = store.limits()?;
    let mut missing = BTreeMap::new();
    for c in store.heads(group)?.values() {
        if let Kind::File { hash, size, .. } = &c.entry.kind {
            if *size > limits.file_bytes {
                bail!("limit exceeded: incoming file {}", c.path);
            }
            if !store.object(hash)?.exists() {
                missing.insert(hash.clone(), *size);
            }
        }
    }
    Ok(missing.into_iter().collect())
}
pub fn validate_heads(store: &Store, root: &Root) -> Result<()> {
    let mut usage = Usage::default();
    let limits = store.limits()?;
    let heads = store.heads(&root.group)?;
    let excluded = scan::exclusions(&root.excludes)?;
    let mut folded = BTreeSet::new();
    for (p, c) in &heads {
        scan::disk_path(root, p)?;
        if scan::is_excluded(&excluded, p) {
            bail!("peer sent an excluded path: {p}");
        }
        if c.entry.kind == Kind::Deleted {
            continue;
        }
        scan::portable_name(p)?;
        if !folded.insert(p.to_lowercase()) {
            bail!("case-colliding path: {p}");
        }
        if c.entry.kind.size() > limits.file_bytes {
            bail!("limit exceeded: incoming file {p}");
        }
        usage.add(&c.entry.kind);
        usage.check(&limits)?;
        let mut parent = p.as_str();
        while let Some((ancestor, _)) = parent.rsplit_once('/') {
            if let Some(a) = heads.get(ancestor)
                && !matches!(a.entry.kind, Kind::Directory | Kind::Deleted)
            {
                bail!("structural conflict at {ancestor}; rename the obstructing entry");
            }
            parent = ancestor;
        }
    }
    store.check_device(root, &usage)
}
pub fn apply(store: &Store, root: &Root) -> Result<()> {
    if root.paused {
        bail!("link is paused");
    }
    validate_heads(store, root)?;
    if !missing_objects(store, &root.group)?.is_empty() {
        bail!("content unavailable; waiting for a peer holding the winning version");
    }
    let applied = store.applied(&root.id)?;
    let mut heads = store.heads(&root.group)?;
    // Surviving concurrent descendants require their directory ancestors. Emit one
    // causal resurrection instead of repeatedly failing a directory deletion.
    let live_paths: Vec<_> = heads
        .iter()
        .filter(|(_, c)| c.entry.kind != Kind::Deleted)
        .map(|(p, _)| p.clone())
        .collect();
    let mut parents = BTreeSet::new();
    for p in live_paths {
        let mut child = p.as_str();
        while let Some((parent, _)) = child.rsplit_once('/') {
            if heads
                .get(parent)
                .is_none_or(|c| c.entry.kind == Kind::Deleted)
            {
                parents.insert(parent.to_owned());
            }
            child = parent;
        }
    }
    for parent in parents {
        let change = new_change(
            store,
            &root.group,
            &parent,
            Entry {
                kind: Kind::Directory,
                modified_ns: now_ns(),
            },
        )?;
        store.append(&change)?;
        heads.insert(parent, change);
    }

    let mut needed: Vec<_> = heads
        .values()
        .filter(|c| applied.get(&c.path).is_none_or(|a| a.id != c.id))
        .cloned()
        .collect();
    #[cfg(windows)]
    if needed
        .iter()
        .any(|c| matches!(c.entry.kind, Kind::Symlink { .. }))
    {
        let (dir, _) = scan::root_dir(root)?;
        let target = format!(".mushy-stage-{}", uuid::Uuid::new_v4());
        let link = format!(".mushy-stage-{}", uuid::Uuid::new_v4());
        dir.create(&target)?;
        let result = dir.symlink_file(&target, &link);
        let _ = dir.remove_file(&link);
        let _ = dir.remove_file(&target);
        result.context("Windows symlink creation unavailable; enable Developer Mode or grant symlink privilege")?;
    }
    // Deletes deepest first, directory creation shallowest first, then content.
    needed.sort_by_key(|c| match c.entry.kind {
        Kind::Deleted => (0, usize::MAX - c.path.matches('/').count()),
        Kind::Directory => (1, c.path.matches('/').count()),
        _ => (2, c.path.matches('/').count()),
    });
    let new_live: BTreeSet<_> = heads.values().filter_map(|c| c.entry.kind.hash()).collect();
    let archived: BTreeMap<_, _> = needed
        .iter()
        .filter_map(|c| applied.get(&c.path))
        .filter_map(|a| {
            a.entry
                .kind
                .hash()
                .filter(|h| !new_live.contains(h))
                .map(|h| (h, a.entry.kind.size()))
        })
        .collect();
    let old_bytes = archived.values().copied().sum();
    store.reserve_recovery(old_bytes)?;
    let (dir, _) = scan::root_dir(root)?;
    scan::verify_identity(store, root, &dir)?;
    let volume = if root.single_file {
        Path::new(&root.path)
            .parent()
            .context("file root needs a parent")?
    } else {
        Path::new(&root.path)
    };
    let largest = needed
        .iter()
        .map(|c| c.entry.kind.size())
        .max()
        .unwrap_or(0);
    if fs2::available_space(volume)? < largest.saturating_add(16 << 20) {
        bail!("insufficient free disk space for staged replacement");
    }
    for change in needed {
        let path = scan::disk_path(root, &change.path)?;
        if change.entry.kind != Kind::Deleted {
            ensure_parents(&dir, &path)?;
        }
        let current = dir.symlink_metadata(&path).ok();
        // Protect edits made after observation. Hash immediately before replacement.
        if let Some(old) = applied.get(&change.path) {
            if !matches_current(&dir, &path, &old.entry.kind)? {
                bail!(
                    "destination changed while syncing {}; rescan required",
                    change.path
                );
            }
            if let Some(hash) = old.entry.kind.hash() {
                store.record_blob(hash, old.entry.kind.size())?;
            }
        } else if current.is_some()
            && !(change.entry.kind == Kind::Directory
                && current
                    .as_ref()
                    .is_some_and(|m| m.is_dir() && !m.file_type().is_symlink()))
        {
            bail!(
                "untracked destination appeared: {}; rescan required",
                change.path
            );
        }
        // Leave directories containing excluded/untracked children intact. They
        // become a new directory edit on the next scan; user files are untouched.
        if change.entry.kind == Kind::Deleted
            && current
                .as_ref()
                .is_some_and(|m| m.is_dir() && !m.file_type().is_symlink())
            && dir.read_dir(&path)?.next().is_some()
        {
            let resurrected = new_change(
                store,
                &root.group,
                &change.path,
                Entry {
                    kind: Kind::Directory,
                    modified_ns: now_ns(),
                },
            )?;
            store.append(&resurrected)?;
            store.remember(&root.id, &resurrected)?;
            continue;
        }
        store.intent(&root.id, &change)?;
        apply_one(store, &dir, &path, &change.entry)?;
        #[cfg(unix)]
        {
            let parent = Path::new(&path)
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            dir.open(parent)?.into_std().sync_all()?;
        }
        store.remember(&root.id, &change)?;
    }
    store.gc()?;
    Ok(())
}
fn ensure_parents(dir: &Dir, path: &str) -> Result<()> {
    let mut current = String::new();
    let parts: Vec<_> = path.split('/').collect();
    for part in &parts[..parts.len() - 1] {
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(part);
        match dir.symlink_metadata(&current) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => (),
            Ok(_) => bail!("parent is not a real directory: {current}"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => dir.create_dir(&current)?,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
pub fn matches_current(dir: &Dir, path: &str, kind: &Kind) -> Result<bool> {
    let m = match dir.symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(*kind == Kind::Deleted),
        Err(e) => return Err(e.into()),
    };
    Ok(match kind {
        Kind::Deleted => false,
        Kind::Directory => m.is_dir() && !m.file_type().is_symlink(),
        Kind::Symlink { target, .. } => {
            m.file_type().is_symlink()
                && dir
                    .read_link_contents(path)?
                    .to_str()
                    .map(|t| canonical_unicode(&t.replace('\\', "/")))
                    == Some(target.clone())
        }
        Kind::File { hash, size, .. } => {
            if !m.is_file() || m.file_type().is_symlink() || m.len() != *size {
                false
            } else {
                let mut opts = OpenOptions::new();
                opts.read(true).follow(FollowSymlinks::No);
                let mut f = dir.open_with(path, &opts)?;
                let mut h = blake3::Hasher::new();
                let mut buf = [0u8; 65536];
                let mut count = 0;
                loop {
                    let n = f.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    count += n as u64;
                    if count > *size {
                        return Ok(false);
                    }
                    h.update(&buf[..n]);
                }
                count == *size && h.finalize().to_hex().as_str() == hash
            }
        }
    })
}
fn apply_one(store: &Store, dir: &Dir, path: &str, entry: &Entry) -> Result<()> {
    let existing = dir.symlink_metadata(path).ok();
    match &entry.kind {
        Kind::Deleted => {
            if let Some(m) = existing {
                if m.is_dir() && !m.file_type().is_symlink() {
                    dir.remove_dir(path)?;
                } else {
                    dir.remove_file_or_symlink(path)?;
                }
            }
        }
        Kind::Directory => {
            if let Some(m) = existing {
                if !m.is_dir() || m.file_type().is_symlink() {
                    bail!("structural conflict at {path}");
                }
            } else {
                dir.create_dir(path)?;
            }
        }
        Kind::File {
            hash,
            size,
            executable,
        } => {
            if existing.is_some_and(|m| m.is_dir() && !m.file_type().is_symlink()) {
                bail!("structural conflict at {path}");
            }
            let parent = Path::new(path).parent().unwrap_or(Path::new(""));
            let temp = parent.join(format!(".mushy-stage-{}", uuid::Uuid::new_v4()));
            let mut options = OpenOptions::new();
            options
                .create_new(true)
                .write(true)
                .follow(FollowSymlinks::No);
            let mut out = dir.open_with(&temp, &options)?;
            let result: Result<()> = (|| {
                let mut input = fs::File::open(store.object(hash)?)?;
                let mut h = blake3::Hasher::new();
                let mut count = 0u64;
                let mut buf = [0u8; 65536];
                loop {
                    let n = input.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    count += n as u64;
                    if count > *size {
                        bail!("invalid object length");
                    }
                    h.update(&buf[..n]);
                    out.write_all(&buf[..n])?;
                }
                if count != *size || h.finalize().to_hex().as_str() != hash {
                    bail!("content hash mismatch");
                }
                #[cfg(unix)]
                {
                    use cap_std::fs::PermissionsExt;
                    out.set_permissions(cap_std::fs::Permissions::from_mode(if *executable {
                        0o700
                    } else {
                        0o600
                    }))?;
                }
                #[cfg(not(unix))]
                let _ = executable;
                filetime::set_file_handle_times(
                    &out.try_clone()?.into_std(),
                    None,
                    Some(filetime::FileTime::from_unix_time(
                        entry.modified_ns.div_euclid(1_000_000_000),
                        entry.modified_ns.rem_euclid(1_000_000_000) as u32,
                    )),
                )?;
                out.sync_all()?;
                Ok(())
            })();
            drop(out);
            if let Err(e) = result {
                let _ = dir.remove_file(&temp);
                return Err(e);
            }
            if let Err(e) = dir.rename(&temp, dir, path) {
                let _ = dir.remove_file_or_symlink(&temp);
                return Err(e.into());
            }
        }
        Kind::Symlink { target, directory } => {
            validate_target(path, target)?;
            let parent = Path::new(path).parent().unwrap_or(Path::new(""));
            match dir.metadata(parent.join(target)) {
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => {
                    return Err(e).context("symlink target is not safely contained in this root");
                }
            }
            if existing.is_some_and(|m| m.is_dir() && !m.file_type().is_symlink()) {
                bail!("structural conflict at {path}");
            }
            let parent = Path::new(path).parent().unwrap_or(Path::new(""));
            let temp = parent.join(format!(".mushy-stage-{}", uuid::Uuid::new_v4()));
            #[cfg(unix)]
            {
                let _ = directory;
                dir.symlink(target, &temp)?;
            }
            #[cfg(windows)]
            {
                if *directory {
                    dir.symlink_dir(target, &temp)?;
                } else {
                    dir.symlink_file(target, &temp)?;
                }
            }
            if let Err(e) = dir.rename(&temp, dir, path) {
                let _ = dir.remove_file_or_symlink(&temp);
                return Err(e.into());
            }
        }
    }
    Ok(())
}
pub fn restore(store: &Store, root: &Root, path: &str, id: &str) -> Result<()> {
    observe(store, root)?;
    let old = store
        .changes(&root.group)?
        .into_iter()
        .find(|c| c.path == path && c.id == id)
        .context("version not found for this path")?;
    if let Some(h) = old.entry.kind.hash()
        && !store.object(h)?.exists()
    {
        bail!("recovery content has expired");
    }
    let mut entry = old.entry;
    entry.modified_ns = now_ns();
    let change = new_change(store, &root.group, path, entry)?;
    store.append(&change)?;
    apply(store, root)
}
