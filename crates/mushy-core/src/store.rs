use crate::model::*;
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

pub struct Store {
    pub dir: PathBuf,
    pub db: Connection,
}
pub struct Guard(File);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
impl Store {
    pub fn open(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir.join("objects"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        let db = Connection::open(dir.join("state.sqlite3"))?;
        db.busy_timeout(std::time::Duration::from_secs(30))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS roots(id TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS archives(id TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS edges(id TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS changes(group_id TEXT,id TEXT,value TEXT NOT NULL,PRIMARY KEY(group_id,id));
            CREATE TABLE IF NOT EXISTS applied(root TEXT,path TEXT,value TEXT NOT NULL,PRIMARY KEY(root,path));
            CREATE TABLE IF NOT EXISTS pending(root TEXT,path TEXT,value TEXT NOT NULL,PRIMARY KEY(root,path));
            CREATE TABLE IF NOT EXISTS blobs(hash TEXT PRIMARY KEY,size INTEGER,expires INTEGER);
            CREATE TABLE IF NOT EXISTS statuses(id TEXT PRIMARY KEY,value TEXT NOT NULL);
            INSERT OR IGNORE INTO settings SELECT 'metadata_bytes_used', COALESCE(SUM(length(CAST(value AS BLOB))),0) FROM changes;
            CREATE TRIGGER IF NOT EXISTS meter_changes AFTER INSERT ON changes BEGIN
                UPDATE settings SET value=CAST(value AS INTEGER)+length(CAST(NEW.value AS BLOB)) WHERE key='metadata_bytes_used';
            END;
            PRAGMA journal_size_limit=8388608;
            PRAGMA wal_autocheckpoint=256;")?;
        db.execute(
            "INSERT OR IGNORE INTO settings VALUES('device',?1)",
            [uuid::Uuid::new_v4().to_string()],
        )?;
        Ok(Self {
            dir: dir.to_owned(),
            db,
        })
    }
    pub fn lock(&self) -> Result<Guard> {
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.dir.join("operation.lock"))?;
        f.lock_exclusive()?;
        Ok(Guard(f))
    }
    pub fn device(&self) -> Result<String> {
        Ok(self
            .db
            .query_row("SELECT value FROM settings WHERE key='device'", [], |r| {
                r.get(0)
            })?)
    }
    pub fn limits(&self) -> Result<Limits> {
        let value: Option<String> = self
            .db
            .query_row("SELECT value FROM settings WHERE key='limits'", [], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(value
            .map(|v| serde_json::from_str(&v))
            .transpose()?
            .unwrap_or_default())
    }
    pub fn set_limits(&self, limits: &Limits) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO settings VALUES('limits',?1)",
            [serde_json::to_string(limits)?],
        )?;
        Ok(())
    }
    pub fn roots(&self) -> Result<Vec<Root>> {
        self.json_rows("SELECT value FROM roots", [])
    }
    pub fn root(&self, id: &str) -> Result<Root> {
        let v: String = self
            .db
            .query_row("SELECT value FROM roots WHERE id=?1", [id], |r| r.get(0))
            .context("unknown root")?;
        Ok(serde_json::from_str(&v)?)
    }
    pub fn root_at(&self, path: &str) -> Result<Option<Root>> {
        Ok(self.roots()?.into_iter().find(|r| r.path == path))
    }
    pub fn save_root(&self, root: &Root) -> Result<()> {
        for old in self.roots()? {
            if old.id == root.id {
                continue;
            }
            if old.group == root.group {
                bail!(
                    "this device already has a root for this group at {}",
                    old.path
                );
            }
            let a = Path::new(&old.path);
            let b = Path::new(&root.path);
            if a.starts_with(b) || b.starts_with(a) {
                bail!("overlapping roots: {} and {}", old.path, root.path);
            }
        }
        self.db.execute(
            "INSERT OR REPLACE INTO roots VALUES(?1,?2)",
            params![root.id, serde_json::to_string(root)?],
        )?;
        Ok(())
    }
    pub fn edges(&self) -> Result<Vec<Edge>> {
        self.json_rows("SELECT value FROM edges", [])
    }
    pub fn save_edge(&self, edge: &Edge) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO edges VALUES(?1,?2)",
            params![edge.id, serde_json::to_string(edge)?],
        )?;
        Ok(())
    }
    pub fn archived_roots(&self) -> Result<Vec<Root>> {
        self.json_rows("SELECT value FROM archives", [])
    }
    pub fn unlink(&self, root: &Root) -> Result<()> {
        for c in self.applied(&root.id)?.values() {
            if let Some(hash) = c.entry.kind.hash() {
                self.record_blob(hash, c.entry.kind.size())?;
            }
        }
        let tx = self.db.unchecked_transaction()?;
        for e in self.edges()?.into_iter().filter(|e| e.root == root.id) {
            tx.execute("DELETE FROM edges WHERE id=?1", [e.id])?;
        }
        tx.execute("DELETE FROM applied WHERE root=?1", [&root.id])?;
        tx.execute("DELETE FROM pending WHERE root=?1", [&root.id])?;
        tx.execute(
            "INSERT OR REPLACE INTO archives VALUES(?1,?2)",
            params![root.id, serde_json::to_string(root)?],
        )?;
        tx.execute("DELETE FROM roots WHERE id=?1", [&root.id])?;
        tx.commit()?;
        Ok(())
    }
    pub fn changes(&self, group: &str) -> Result<Vec<Change>> {
        self.json_rows("SELECT value FROM changes WHERE group_id=?1", [group])
    }
    pub fn heads(&self, group: &str) -> Result<BTreeMap<String, Change>> {
        let mut grouped: BTreeMap<String, Vec<Change>> = BTreeMap::new();
        for change in self.changes(group)? {
            grouped.entry(change.path.clone()).or_default().push(change);
        }
        Ok(grouped
            .into_iter()
            .filter_map(|(p, events)| winner(&events).cloned().map(|c| (p, c)))
            .collect())
    }
    pub fn clock(&self, group: &str) -> Result<Clock> {
        let mut clock = Clock::new();
        for c in self.changes(group)? {
            clock
                .entry(c.device)
                .and_modify(|s| *s = (*s).max(c.sequence))
                .or_insert(c.sequence);
        }
        Ok(clock)
    }
    pub fn append(&self, change: &Change) -> Result<()> {
        change.validate(&change.group)?;
        let value = serde_json::to_string(change)?;
        if let Some(old) = self
            .db
            .query_row(
                "SELECT value FROM changes WHERE group_id=?1 AND id=?2",
                params![change.group, change.id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            if old != value {
                bail!("peer reused an immutable change ID");
            }
            return Ok(());
        }
        let size: u64 = self.db.query_row(
            "SELECT CAST(value AS INTEGER) FROM settings WHERE key='metadata_bytes_used'",
            [],
            |r| r.get(0),
        )?;
        if size.saturating_add(value.len() as u64) > self.limits()?.metadata_bytes {
            bail!("limit exceeded: synchronization metadata; raise metadata_bytes");
        }
        self.db.execute(
            "INSERT INTO changes VALUES(?1,?2,?3)",
            params![change.group, change.id, value],
        )?;
        Ok(())
    }
    pub fn import(&self, group: &str, changes: &[Change]) -> Result<()> {
        for c in changes {
            c.validate(group)?;
        }
        let tx = self.db.unchecked_transaction()?;
        for c in changes {
            self.append(c)?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn applied(&self, root: &str) -> Result<BTreeMap<String, Change>> {
        let rows: Vec<Change> =
            self.json_rows("SELECT value FROM applied WHERE root=?1", [root])?;
        Ok(rows.into_iter().map(|c| (c.path.clone(), c)).collect())
    }
    pub fn remember(&self, root: &str, change: &Change) -> Result<()> {
        let tx = if self.db.is_autocommit() {
            Some(self.db.unchecked_transaction()?)
        } else {
            None
        };
        self.db.execute(
            "INSERT OR REPLACE INTO applied VALUES(?1,?2,?3)",
            params![root, change.path, serde_json::to_string(change)?],
        )?;
        self.db.execute(
            "DELETE FROM pending WHERE root=?1 AND path=?2",
            params![root, change.path],
        )?;
        if let Some(tx) = tx {
            tx.commit()?;
        }
        Ok(())
    }
    pub fn pending(&self, root: &str) -> Result<Vec<Change>> {
        self.json_rows("SELECT value FROM pending WHERE root=?1", [root])
    }
    pub fn intent(&self, root: &str, change: &Change) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO pending VALUES(?1,?2,?3)",
            params![root, change.path, serde_json::to_string(change)?],
        )?;
        Ok(())
    }
    pub fn clear_pending(&self, root: &str, path: &str) -> Result<()> {
        self.db.execute(
            "DELETE FROM pending WHERE root=?1 AND path=?2",
            params![root, path],
        )?;
        Ok(())
    }
    pub fn object(&self, hash: &str) -> Result<PathBuf> {
        validate_hash(hash)?;
        Ok(self.dir.join("objects").join(hash))
    }
    pub fn record_blob(&self, hash: &str, size: u64) -> Result<()> {
        validate_hash(hash)?;
        let expiry = now_ns().saturating_add(
            (self
                .limits()?
                .recovery_days
                .saturating_mul(86_400)
                .min(i64::MAX as u64 / 1_000_000_000) as i64)
                * 1_000_000_000,
        );
        self.db.execute("INSERT INTO blobs VALUES(?1,?2,?3) ON CONFLICT(hash) DO UPDATE SET expires=MAX(expires,excluded.expires)", params![hash,size,expiry])?;
        Ok(())
    }
    pub fn live_hashes(&self) -> Result<BTreeSet<String>> {
        let mut live = BTreeSet::new();
        for r in self.roots()? {
            for c in self
                .heads(&r.group)?
                .values()
                .chain(self.applied(&r.id)?.values())
            {
                if let Some(hash) = c.entry.kind.hash() {
                    live.insert(hash.to_owned());
                }
            }
        }
        Ok(live)
    }
    pub fn gc(&self) -> Result<u64> {
        let live = self.live_hashes()?;
        let mut stmt = self.db.prepare("SELECT hash,size,expires FROM blobs")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, u64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut retained = 0u64;
        for (hash, size, expires) in rows {
            if live.contains(&hash) {
                continue;
            }
            if expires < now_ns() {
                match fs::remove_file(self.object(&hash)?) {
                    Ok(()) => (),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                    Err(e) => return Err(e.into()),
                }
                self.db.execute("DELETE FROM blobs WHERE hash=?1", [hash])?;
            } else {
                retained = retained.saturating_add(size);
            }
        }
        Ok(retained)
    }
    pub fn reserve_recovery(&self, bytes: u64) -> Result<()> {
        if self.gc()?.saturating_add(bytes) > self.limits()?.recovery_bytes {
            bail!("recovery budget full; raise recovery_bytes or wait for history to expire");
        }
        Ok(())
    }
    pub fn check_device(&self, root: &Root, usage: &Usage) -> Result<()> {
        let limits = self.limits()?;
        let mut total = usage.clone();
        for r in self.roots()?.into_iter().filter(|r| r.id != root.id) {
            for c in self.applied(&r.id)?.values() {
                total.add(&c.entry.kind);
            }
        }
        if total.bytes > limits.device_bytes
            || total.files > limits.device_files
            || total.directories > limits.device_directories
        {
            bail!("limit exceeded: total linked content on this device");
        }
        Ok(())
    }
    pub fn status(&self, id: &str, state: &str, detail: &str) -> Result<()> {
        let value = serde_json::json!({"state":state,"detail":detail,"updated_ns":now_ns()});
        self.db.execute(
            "INSERT OR REPLACE INTO statuses VALUES(?1,?2)",
            params![id, value.to_string()],
        )?;
        Ok(())
    }
    pub fn statuses(&self) -> Result<BTreeMap<String, serde_json::Value>> {
        let mut s = self.db.prepare("SELECT id,value FROM statuses")?;
        let rows = s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.map(|r| {
            let (k, v) = r?;
            Ok((k, serde_json::from_str(&v)?))
        })
        .collect()
    }
    fn json_rows<T: serde::de::DeserializeOwned, P: rusqlite::Params>(
        &self,
        sql: &str,
        p: P,
    ) -> Result<Vec<T>> {
        let mut statement = self.db.prepare(sql)?;
        let values = statement.query_map(p, |r| r.get::<_, String>(0))?;
        values.map(|v| Ok(serde_json::from_str(&v?)?)).collect()
    }
}
