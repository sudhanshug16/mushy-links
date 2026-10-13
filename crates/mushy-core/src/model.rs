use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type Clock = BTreeMap<String, u64>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Kind {
    File {
        hash: String,
        size: u64,
        executable: bool,
    },
    Directory,
    Symlink {
        target: String,
        directory: bool,
    },
    Deleted,
}
impl Kind {
    pub fn hash(&self) -> Option<&str> {
        match self {
            Self::File { hash, .. } => Some(hash),
            _ => None,
        }
    }
    pub fn size(&self) -> u64 {
        match self {
            Self::File { size, .. } => *size,
            Self::Symlink { target, .. } => target.len() as u64,
            _ => 0,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub modified_ns: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Change {
    pub id: String,
    pub group: String,
    pub device: String,
    pub sequence: u64,
    pub context: Clock,
    pub path: String,
    pub entry: Entry,
}
impl Change {
    pub fn validate(&self, group: &str) -> Result<()> {
        if self.group != group
            || self.sequence == 0
            || self.device.len() > 128
            || self.device.is_empty()
            || self.id != format!("{}:{}", self.device, self.sequence)
            || self.context.get(&self.device).copied().unwrap_or(0) >= self.sequence
        {
            bail!("invalid change identity or causal context");
        }
        validate_path(&self.path)?;
        if let Kind::File { hash, .. } = &self.entry.kind {
            validate_hash(hash)?;
        }
        if let Kind::Symlink { target, .. } = &self.entry.kind {
            validate_target(&self.path, target)?;
        }
        Ok(())
    }
    pub fn observes(&self, other: &Self) -> bool {
        self.context.get(&other.device).copied().unwrap_or(0) >= other.sequence
    }
}
/// Causality first; deterministic timestamp ordering only for concurrent edits.
pub fn winner(changes: &[Change]) -> Option<&Change> {
    let mut frontier: Vec<&Change> = Vec::new();
    for change in changes {
        if frontier.iter().any(|head| head.observes(change)) {
            continue;
        }
        frontier.retain(|head| !change.observes(head));
        frontier.push(change);
    }
    frontier.into_iter().max_by(|a, b| {
        (a.entry.modified_ns, &a.device, &a.id).cmp(&(b.entry.modified_ns, &b.device, &b.id))
    })
}

pub fn validate_hash(hash: &str) -> Result<()> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        bail!("invalid content hash");
    }
    Ok(())
}
pub fn canonical_unicode(path: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    path.nfc().collect()
}
pub fn validate_path(path: &str) -> Result<()> {
    if canonical_unicode(path) != path {
        bail!("filenames must use Unicode NFC normalization: {path:?}");
    }
    if path.is_empty()
        || path.len() > 4096
        || path.contains('\\')
        || path.contains('\0')
        || path.split('/').any(|s| {
            s.is_empty()
                || s == "."
                || s == ".."
                || s.contains(':')
                || s.starts_with(".mushy-stage-")
        })
    {
        bail!("unsafe or unsupported relative path: {path:?}");
    }
    Ok(())
}
pub fn validate_target(path: &str, target: &str) -> Result<()> {
    if canonical_unicode(target) != target {
        bail!("symlink targets must use Unicode NFC normalization");
    }
    if target.is_empty()
        || target.starts_with('/')
        || target.contains('\\')
        || target.contains(':')
        || target.contains('\0')
    {
        bail!("symlink {path:?} needs a relative, inside-root target");
    }
    let mut depth = path.split('/').count() - 1;
    for part in target.split('/') {
        match part {
            "" | "." => (),
            ".." => {
                if depth == 0 {
                    bail!("symlink {path:?} escapes the root");
                }
                depth -= 1;
            }
            _ => depth += 1,
        }
    }
    Ok(())
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub bytes: u64,
    pub files: u64,
    pub directories: u64,
    pub file_bytes: u64,
    pub device_bytes: u64,
    pub device_files: u64,
    pub device_directories: u64,
    pub metadata_bytes: u64,
    pub recovery_bytes: u64,
    pub recovery_days: u64,
    pub scan_seconds: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            bytes: 1 << 30,
            files: 10_000,
            directories: 1_000,
            file_bytes: 100 << 20,
            device_bytes: 4 << 30,
            device_files: 40_000,
            device_directories: 4_000,
            metadata_bytes: 256 << 20,
            recovery_bytes: 2 << 30,
            recovery_days: 7,
            scan_seconds: 60,
        }
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub bytes: u64,
    pub files: u64,
    pub directories: u64,
    #[serde(default)]
    pub largest_file: u64,
    #[serde(default)]
    pub symlinks: u64,
}
impl Usage {
    pub fn add(&mut self, kind: &Kind) {
        self.bytes = self.bytes.saturating_add(kind.size());
        self.largest_file = self.largest_file.max(kind.size());
        if matches!(kind, Kind::Symlink { .. }) {
            self.symlinks += 1;
        }
        match kind {
            Kind::File { .. } | Kind::Symlink { .. } => self.files += 1,
            Kind::Directory => self.directories += 1,
            Kind::Deleted => (),
        }
    }
    pub fn check(&self, limits: &Limits) -> Result<()> {
        if self.largest_file > limits.file_bytes {
            bail!(
                "limit exceeded: largest file is {} bytes > {}",
                self.largest_file,
                limits.file_bytes
            );
        }
        if self.bytes > limits.bytes {
            bail!("limit exceeded: {} bytes > {}", self.bytes, limits.bytes);
        }
        if self.files > limits.files {
            bail!("limit exceeded: {} files > {}", self.files, limits.files);
        }
        if self.directories > limits.directories {
            bail!(
                "limit exceeded: {} directories > {}",
                self.directories,
                limits.directories
            );
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Root {
    pub id: String,
    pub group: String,
    pub path: String,
    pub single_file: bool,
    pub excludes: Vec<String>,
    pub paused: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub id: String,
    pub root: String,
    pub host: String,
    pub remote_path: String,
}
pub fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(i64::MAX as u128) as i64
}
