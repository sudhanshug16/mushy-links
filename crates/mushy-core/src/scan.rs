use crate::{model::*, store::Store};
use anyhow::{Context, Result, bail};
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};

pub fn absolute(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    if let Ok(meta) = fs::symlink_metadata(&path) {
        if meta.file_type().is_symlink() {
            bail!("a linked root cannot itself be a symlink");
        }
        return Ok(path.canonicalize()?);
    }
    let mut ancestor = path.as_path();
    let mut suffix = Vec::new();
    while fs::symlink_metadata(ancestor).is_err() {
        suffix.push(
            ancestor
                .file_name()
                .context("missing path must have ordinary filename components")?
                .to_owned(),
        );
        ancestor = ancestor
            .parent()
            .context("cannot find an existing parent")?;
    }
    let mut resolved = ancestor.canonicalize()?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}
pub fn reference_path(path: &Path) -> Result<PathBuf> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        let p = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()?.join(path)
        };
        Ok(p.parent()
            .context("path needs a parent")?
            .canonicalize()?
            .join(p.file_name().context("path needs a name")?))
    } else {
        absolute(path)
    }
}
pub fn root_dir(root: &Root) -> Result<(Dir, String)> {
    let p = Path::new(&root.path);
    if root.single_file {
        let name = p
            .file_name()
            .context("invalid file root")?
            .to_str()
            .context("non-UTF-8 filename")?
            .to_owned();
        Ok((
            Dir::open_ambient_dir(p.parent().context("no parent")?, ambient_authority())?,
            name,
        ))
    } else {
        let meta = fs::symlink_metadata(p)
            .context("root missing or inaccessible; synchronization paused")?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            bail!("directory root replaced or missing; synchronization paused");
        }
        Ok((
            Dir::open_ambient_dir(p, ambient_authority())?,
            String::new(),
        ))
    }
}
/// Pin the directory containing the root so a removed mount or replaced root
/// cannot be mistaken for a tree full of user-requested deletions.
pub fn verify_identity(store: &Store, root: &Root, dir: &Dir) -> Result<()> {
    let identity = directory_identity(dir)?;
    let key = format!("root_identity:{}", root.id);
    store.db.execute(
        "INSERT OR IGNORE INTO settings VALUES(?1,?2)",
        rusqlite::params![key, identity],
    )?;
    let saved: String =
        store
            .db
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
                r.get(0)
            })?;
    if saved != identity {
        bail!(
            "root directory identity changed (replacement or mount change); restore the original directory before resuming"
        );
    }
    Ok(())
}
fn directory_identity(dir: &Dir) -> Result<String> {
    let file = dir.try_clone()?.into_std_file();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = file.metadata()?;
        Ok(format!("{}:{}", meta.dev(), meta.ino()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
        };
        let mut info = std::mem::MaybeUninit::<FILE_ID_INFO>::uninit();
        // SAFETY: the handle belongs to `file`, which stays alive throughout this
        // call. The buffer is correctly sized and only read after API success.
        let success = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                info.as_mut_ptr().cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        };
        if success == 0 {
            return Err(std::io::Error::last_os_error())
                .context("cannot verify root directory identity on this filesystem");
        }
        // SAFETY: the API initialized the complete FILE_ID_INFO on success.
        let info = unsafe { info.assume_init() };
        let id = u128::from_le_bytes(info.FileId.Identifier);
        Ok(format!("{}:{id:032x}", info.VolumeSerialNumber))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        bail!("root directory identity is unsupported on this platform")
    }
}
pub fn disk_path(root: &Root, path: &str) -> Result<String> {
    validate_path(path)?;
    if root.single_file {
        if path != "file" {
            bail!("file-root protocol path must be 'file'");
        }
        Ok(Path::new(&root.path)
            .file_name()
            .context("no name")?
            .to_str()
            .context("non UTF-8 filename")?
            .to_owned())
    } else {
        #[cfg(target_os = "macos")]
        {
            use unicode_normalization::UnicodeNormalization;
            Ok(path.nfd().collect())
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(path.to_owned())
        }
    }
}
pub fn exclusions(patterns: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        b.add(Glob::new(p)?);
    }
    Ok(b.build()?)
}
pub fn is_excluded(set: &GlobSet, path: &str) -> bool {
    if path.split('/').any(|p| p.starts_with(".mushy-stage-")) {
        return true;
    }
    let mut current = path;
    loop {
        if set.is_match(current) || set.is_match(format!("{current}/")) {
            return true;
        }
        if let Some((parent, _)) = current.rsplit_once('/') {
            current = parent;
        } else {
            return false;
        }
    }
}
#[derive(Debug)]
pub struct Snapshot {
    pub entries: BTreeMap<String, Entry>,
    pub usage: Usage,
}
pub fn scan(store: &Store, root: &Root, portable: bool) -> Result<Snapshot> {
    let limits = store.limits()?;
    let (dir, name) = root_dir(root)?;
    verify_identity(store, root, &dir)?;
    let mut scanner = Scanner {
        store,
        root,
        dir: &dir,
        limits,
        excludes: exclusions(&root.excludes)?,
        entries: BTreeMap::new(),
        usage: Usage::default(),
        names: BTreeMap::new(),
        start: Instant::now(),
        portable,
    };
    if root.single_file {
        match dir.symlink_metadata(&name) {
            Ok(_) => scanner.visit("file", &name)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    } else {
        scanner.walk("")?;
    }
    store.check_device(root, &scanner.usage)?;
    Ok(Snapshot {
        entries: scanner.entries,
        usage: scanner.usage,
    })
}
struct Scanner<'a> {
    store: &'a Store,
    root: &'a Root,
    dir: &'a Dir,
    limits: Limits,
    excludes: GlobSet,
    entries: BTreeMap<String, Entry>,
    usage: Usage,
    names: BTreeMap<String, String>,
    start: Instant,
    portable: bool,
}
impl Scanner<'_> {
    fn check_time(&self) -> Result<()> {
        if self.start.elapsed().as_secs() >= self.limits.scan_seconds {
            bail!("scan budget exceeded; raise scan_seconds or choose a smaller root");
        }
        Ok(())
    }
    fn walk(&mut self, relative: &str) -> Result<()> {
        self.check_time()?;
        let dir = if relative.is_empty() {
            self.dir.try_clone()?
        } else {
            self.dir.open_dir(relative)?
        };
        for child in dir.entries()? {
            let name = child?
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("non-UTF-8 filenames are unsupported"))?;
            let p = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            #[cfg(target_os = "macos")]
            let key = canonical_unicode(&p);
            #[cfg(not(target_os = "macos"))]
            let key = p.clone();
            if is_excluded(&self.excludes, &key) {
                continue;
            }
            self.visit(&key, &p)?;
        }
        Ok(())
    }
    fn visit(&mut self, key: &str, relative: &str) -> Result<()> {
        self.check_time()?;
        validate_path(key)?;
        if self.portable {
            portable_name(key)?;
        }
        let folded = key.to_lowercase();
        if self.portable && self.names.insert(folded, key.to_owned()).is_some() {
            bail!("case-colliding path: {key}");
        }
        let meta = self.dir.symlink_metadata(relative)?;
        let modified_ns = meta
            .modified()?
            .into_std()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .min(i64::MAX as u128) as i64;
        let kind = if meta.file_type().is_symlink() {
            if self.root.single_file {
                bail!("symlinks need a directory root so targets can stay inside it");
            }
            let target = self
                .dir
                .read_link_contents(relative)?
                .into_os_string()
                .into_string()
                .map_err(|_| anyhow::anyhow!("non UTF-8 symlink"))?;
            #[cfg(windows)]
            let target = target.replace('\\', "/");
            #[cfg(target_os = "macos")]
            let target = canonical_unicode(&target);
            validate_target(key, &target)?;
            let resolved_directory = match self.dir.metadata(relative) {
                Ok(m) => Some(m.is_dir()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => {
                    return Err(e).context(format!(
                        "symlink {key} cannot be resolved safely inside this root"
                    ));
                }
            };
            #[cfg(not(windows))]
            let directory = resolved_directory.unwrap_or(false);
            #[cfg(windows)]
            let directory = {
                use cap_std::fs::FileTypeExt;
                let _ = resolved_directory;
                // Windows records the link's type even after its target is
                // deleted. Preserve it so dangling links do not block deletes.
                meta.file_type().is_symlink_dir()
            };
            Kind::Symlink { target, directory }
        } else if meta.is_dir() {
            Kind::Directory
        } else if meta.is_file() {
            if meta.len() > self.limits.file_bytes {
                bail!(
                    "limit exceeded: {key} is {} bytes, maximum {}",
                    meta.len(),
                    self.limits.file_bytes
                );
            }
            // Check entry limits before reading potentially expensive content.
            let mut next = self.usage.clone();
            next.add(&Kind::File {
                hash: String::new(),
                size: meta.len(),
                executable: false,
            });
            next.check(&self.limits)?;
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            let mut file = self.dir.open_with(relative, &options)?;
            if fs2::available_space(&self.store.dir)? < meta.len().saturating_add(16 << 20) {
                bail!("insufficient free disk space for content capture");
            }
            let stage = self
                .store
                .dir
                .join("objects")
                .join(format!(".stage-{}", uuid::Uuid::new_v4()));
            let mut tmp = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&stage)?;
            let mut hasher = blake3::Hasher::new();
            let mut size = 0u64;
            let mut buf = [0u8; 65536];
            let result: Result<()> = (|| {
                loop {
                    self.check_time()?;
                    let n = file.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    size += n as u64;
                    if size > self.limits.file_bytes {
                        bail!("file grew above size limit: {key}");
                    }
                    hasher.update(&buf[..n]);
                    tmp.write_all(&buf[..n])?;
                }
                let after = self.dir.symlink_metadata(relative)?;
                if after.len() != meta.len()
                    || after.modified()? != meta.modified()?
                    || size != meta.len()
                {
                    bail!("file changed during scan; retrying: {key}");
                }
                tmp.sync_all()?;
                Ok(())
            })();
            drop(tmp);
            if let Err(e) = result {
                let _ = fs::remove_file(&stage);
                return Err(e);
            }
            let hash = hasher.finalize().to_hex().to_string();
            let object = self.store.object(&hash)?;
            if object.exists() {
                fs::remove_file(&stage)?;
            } else {
                fs::rename(&stage, &object)?;
            }
            self.store.record_blob(&hash, size)?;
            #[cfg(unix)]
            let executable = {
                use cap_std::fs::PermissionsExt;
                meta.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            Kind::File {
                hash,
                size,
                executable,
            }
        } else {
            bail!("unsupported special file: {key}");
        };
        self.usage.add(&kind);
        self.usage.check(&self.limits)?;
        let is_dir = kind == Kind::Directory;
        self.entries
            .insert(key.to_owned(), Entry { kind, modified_ns });
        if is_dir {
            self.walk(relative)?;
        }
        Ok(())
    }
}
pub fn portable_name(path: &str) -> Result<()> {
    if path.len() > 240 {
        bail!("path exceeds portable 240-byte limit: {path}");
    }
    for part in path.split('/') {
        let base = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if part.ends_with(['.', ' '])
            || part.chars().any(|c| c < ' ' || "<>:\"|?*".contains(c))
            || matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (base.len() == 4
                && (base.starts_with("COM") || base.starts_with("LPT"))
                && matches!(base.as_bytes()[3], b'1'..=b'9'))
        {
            bail!("filename is incompatible with Windows: {path}");
        }
    }
    Ok(())
}
