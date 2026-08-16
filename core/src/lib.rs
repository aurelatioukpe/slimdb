//! # SlimDB
//!
//! The tiny embedded document database for local apps.
//!
//! No server. No configuration. No SQL.
//!
//! ```no_run
//! use slimdb_core::{Db, Value};
//!
//! fn main() -> slimdb_core::error::Result<()> {
//!     let mut db = Db::open("game.sdb")?;
//!     db.set("player.level", Value::Int(42))?;
//!     db.set("player.name", Value::Str("Aurel".to_string()))?;
//!     assert_eq!(db.get("player.level")?, Some(Value::Int(42)));
//!     db.close()?;
//!     Ok(())
//! }
//! ```

pub mod path;

pub mod crc32;
pub mod document;
pub mod error;
pub mod json;
pub mod record;
pub mod recovery;
pub mod wal;

pub use document::Value;
pub use error::{Result, SlimDbError};
pub use wal::DurabilityMode;

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::path::{
    delete_at_path, exists_at_path, get_at_path, set_at_path, split_path, validate_path_shape,
};
use crate::record::{
    parse_db_header, parse_wal_header, write_db_header, Record, DB_HEADER_LEN, TYPE_CLEAR,
    TYPE_DELETE, TYPE_SET, WAL_HEADER_LEN,
};
use crate::recovery::replay;
use crate::wal::Wal;

/// Snapshot of database statistics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    /// Number of top-level keys.
    pub keys: usize,
    /// Size of the snapshot file (`*.sdb`), 0 if it does not exist yet.
    pub data_file_size: u64,
    /// Size of the WAL file (`*.sdb-wal`).
    pub wal_file_size: u64,
    /// Next write sequence number.
    pub sequence: u64,
    /// Active durability mode.
    pub durability: DurabilityMode,
    /// Path of the database.
    pub path: String,
}

/// A SlimDB database handle.
///
/// One process, one file. See `docs/FORMAT.md` for the on-disk format and
/// `docs/SPEC.md` for the design scope.
pub struct Db {
    path: PathBuf,
    wal: Wal,
    state: BTreeMap<String, Value>,
    db_id: u64,
    seq: u64,
    mode: DurabilityMode,
}

impl Db {
    /// Open (or create) the database at `path`, with safe durability.
    ///
    /// ```
    /// # use slimdb_core::Db;
    /// # fn main() -> slimdb_core::error::Result<()> {
    /// let dir = std::env::temp_dir().join(format!("slimdb-open-doc-{}", std::process::id()));
    /// std::fs::create_dir_all(&dir).unwrap();
    /// let path = dir.join("doc.sdb");
    /// let mut db = Db::open(&path)?;
    /// db.close()?;
    /// # std::fs::remove_dir_all(&dir).ok();
    /// # Ok(())
    /// # }
    /// ```
    pub fn open(path: impl AsRef<Path>) -> Result<Db> {
        Db::open_with_mode(path, DurabilityMode::Safe)
    }

    /// Open (or create) the database at `path` with an explicit durability
    /// mode. See [`DurabilityMode`] for the trade-offs.
    pub fn open_with_mode(path: impl AsRef<Path>, mode: DurabilityMode) -> Result<Db> {
        let data_path = path.as_ref().to_path_buf();
        let wal_path = wal_path_for(&data_path);

        // Best-effort cleanup of a temp file left by an interrupted compaction.
        let _ = fs::remove_file(temp_path_for(&data_path));

        let snapshot_bytes = if data_path.exists() {
            let bytes = fs::read(&data_path)
                .map_err(|e| SlimDbError::Io(format!("read {}: {e}", data_path.display())))?;
            Some(bytes)
        } else {
            None
        };

        let mut state: BTreeMap<String, Value> = BTreeMap::new();
        let mut max_seq = 0u64;
        let mut db_id = 0u64;

        if let Some(bytes) = &snapshot_bytes {
            let header = parse_db_header(bytes)?;
            db_id = header.db_id;
            let outcome = replay(bytes, DB_HEADER_LEN, 0, false, true, |rec| {
                apply_record_to_state(&mut state, rec)
            })?;
            max_seq = max_seq.max(outcome.max_seq);
        }

        let (wal_db_id, truncate_to, rewrite_header) = match &snapshot_bytes {
            Some(bytes) => {
                let header = parse_db_header(bytes)?;
                if wal_path.exists() {
                    let wb = fs::read(&wal_path).map_err(|e| {
                        SlimDbError::Io(format!("read {}: {e}", wal_path.display()))
                    })?;
                    match parse_wal_header(&wb) {
                        Some(id) if id == header.db_id => {
                            // Records already covered by the snapshot (seq
                            // <= last_seq) are skipped: re-applying them on
                            // a state that has since advanced would fail.
                            let outcome =
                                replay(&wb, WAL_HEADER_LEN, header.last_seq, true, false, |rec| {
                                    apply_record_to_state(&mut state, rec)
                                })?;
                            max_seq = max_seq.max(outcome.max_seq);
                            (header.db_id, Some(outcome.truncate_to as u64), false)
                        }
                        // Stale or unreadable WAL: discard it.
                        _ => (header.db_id, Some(0), true),
                    }
                } else {
                    (header.db_id, Some(0), true)
                }
            }
            None => {
                if wal_path.exists() {
                    let wb = fs::read(&wal_path).map_err(|e| {
                        SlimDbError::Io(format!("read {}: {e}", wal_path.display()))
                    })?;
                    match parse_wal_header(&wb) {
                        // A database that was created but never compacted has
                        // only a WAL; replay it.
                        Some(id) => {
                            db_id = id;
                            let outcome = replay(&wb, WAL_HEADER_LEN, 0, true, false, |rec| {
                                apply_record_to_state(&mut state, rec)
                            })?;
                            max_seq = max_seq.max(outcome.max_seq);
                            (id, Some(outcome.truncate_to as u64), false)
                        }
                        None => {
                            db_id = new_db_id();
                            (db_id, Some(0), true)
                        }
                    }
                } else {
                    db_id = new_db_id();
                    (db_id, Some(0), true)
                }
            }
        };

        let wal = Wal::open(&wal_path, wal_db_id, mode, truncate_to, rewrite_header)?;
        Ok(Db {
            path: data_path,
            wal,
            state,
            db_id,
            seq: max_seq + 1,
            mode,
        })
    }

    /// Close the database. All buffered writes are flushed and the state is
    /// compacted into the snapshot file.
    pub fn close(mut self) -> Result<()> {
        self.compact()
    }

    /// Get the value at `path`, or `None` if it does not exist.
    pub fn get(&self, path: impl AsRef<str>) -> Result<Option<Value>> {
        let path = path.as_ref();
        let segs = split_path(path)?;
        Ok(get_at_path(&self.state, &segs))
    }

    /// Set the value at `path`, creating intermediate objects as needed.
    ///
    /// Errors with [`SlimDbError::TypeMismatch`] if the path traverses a
    /// non-object value.
    pub fn set(&mut self, path: impl AsRef<str>, value: Value) -> Result<()> {
        let path = path.as_ref();
        let segs = split_path(path)?;
        validate_path_shape(&self.state, &segs)?;
        let value = value.canonicalize();
        let rec = Record::set(path, &value, self.seq)?;
        self.seq += 1;
        self.wal.append_record(&rec)?;
        set_at_path(&mut self.state, &segs, value)?;
        Ok(())
    }

    /// Delete the value at `path`. Returns `true` if something was removed.
    pub fn delete(&mut self, path: impl AsRef<str>) -> Result<bool> {
        let path = path.as_ref();
        let segs = split_path(path)?;
        if !exists_at_path(&self.state, &segs) {
            return Ok(false);
        }
        let rec = Record::delete(path, self.seq)?;
        self.seq += 1;
        self.wal.append_record(&rec)?;
        delete_at_path(&mut self.state, &segs);
        Ok(true)
    }

    /// Whether a value exists at `path`.
    pub fn exists(&self, path: impl AsRef<str>) -> Result<bool> {
        self.has(path)
    }

    /// Alias for [`Db::exists`].
    pub fn has(&self, path: impl AsRef<str>) -> Result<bool> {
        let path = path.as_ref();
        let segs = split_path(path)?;
        Ok(exists_at_path(&self.state, &segs))
    }

    /// All top-level keys, sorted.
    pub fn keys(&self) -> Vec<String> {
        self.state.keys().cloned().collect()
    }

    /// Read-modify-write: pass the current value (or `None`) to `f` and set
    /// or delete according to the returned value.
    pub fn update<F>(&mut self, path: impl AsRef<str>, f: F) -> Result<()>
    where
        F: FnOnce(Option<Value>) -> Option<Value>,
    {
        let current = self.get(path.as_ref())?;
        match f(current) {
            Some(v) => {
                self.set(path, v)?;
            }
            None => {
                self.delete(path)?;
            }
        }
        Ok(())
    }

    /// Remove every key.
    pub fn clear(&mut self) -> Result<()> {
        if self.state.is_empty() {
            return Ok(());
        }
        let rec = Record::clear(self.seq);
        self.seq += 1;
        self.wal.append_record(&rec)?;
        self.state.clear();
        Ok(())
    }

    /// Flush buffered WAL bytes to disk and fsync.
    pub fn flush(&mut self) -> Result<()> {
        self.wal.flush(true)
    }

    /// Database statistics.
    pub fn stats(&self) -> Result<Stats> {
        let data_size = fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        let wal_size = fs::metadata(self.wal.path()).map(|m| m.len()).unwrap_or(0);
        Ok(Stats {
            keys: self.state.len(),
            data_file_size: data_size,
            wal_file_size: wal_size,
            sequence: self.seq,
            durability: self.mode,
            path: self.path.display().to_string(),
        })
    }

    /// The active durability mode.
    pub fn durability(&self) -> DurabilityMode {
        self.mode
    }

    /// Compact the WAL into the snapshot file, atomically
    /// (temp file + fsync + rename), then reset the WAL.
    pub fn compact(&mut self) -> Result<()> {
        // Every buffered WAL byte must be durable before the snapshot is
        // written, otherwise truncating the WAL could lose writes.
        self.wal.flush(true)?;
        // The snapshot covers the whole WAL history up to the last issued
        // sequence. It is recorded in the header so that, if a crash lands
        // between the snapshot rename and the WAL reset, recovery skips the
        // WAL records already contained in the snapshot instead of
        // re-applying them to a state that has since advanced.
        let last_seq = self.seq.saturating_sub(1);
        let tmp = temp_path_for(&self.path);
        {
            let mut out = File::create(&tmp)
                .map_err(|e| SlimDbError::Io(format!("create {}: {e}", tmp.display())))?;
            write_db_header(&mut out, self.db_id, last_seq)?;
            for (key, value) in &self.state {
                let rec = Record::set(key, value, self.seq)?;
                self.seq += 1;
                let mut buf = Vec::new();
                rec.encode(&mut buf)?;
                out.write_all(&buf)
                    .map_err(|e| SlimDbError::Io(format!("write snapshot: {e}")))?;
            }
            out.sync_all()
                .map_err(|e| SlimDbError::Io(format!("fsync snapshot: {e}")))?;
        }
        fs::rename(&tmp, &self.path)
            .map_err(|e| SlimDbError::Io(format!("rename snapshot: {e}")))?;
        sync_parent_dir(&self.path)?;
        self.wal.reset(self.db_id)?;
        Ok(())
    }

    /// Write a standalone, consistent snapshot of the database to `dest`.
    ///
    /// The backup is a plain `*.sdb` file that can be opened directly or
    /// restored later with [`Db::restore`].
    pub fn backup(&mut self, dest: impl AsRef<Path>) -> Result<()> {
        self.wal.flush(true)?;
        let dest = dest.as_ref();
        let tmp = temp_path_for(dest);
        {
            let mut out = File::create(&tmp)
                .map_err(|e| SlimDbError::Io(format!("create {}: {e}", tmp.display())))?;
            let last_seq = self.seq.saturating_sub(1);
            write_db_header(&mut out, self.db_id, last_seq)?;
            for (key, value) in &self.state {
                let rec = Record::set(key, value, self.seq)?;
                self.seq += 1;
                let mut buf = Vec::new();
                rec.encode(&mut buf)?;
                out.write_all(&buf)
                    .map_err(|e| SlimDbError::Io(format!("write backup: {e}")))?;
            }
            out.sync_all()
                .map_err(|e| SlimDbError::Io(format!("fsync backup: {e}")))?;
        }
        fs::rename(&tmp, dest).map_err(|e| SlimDbError::Io(format!("rename backup: {e}")))?;
        sync_parent_dir(dest)?;
        Ok(())
    }

    /// Replace this database's contents with the backup at `src`.
    ///
    /// The replacement is atomic on the snapshot and the WAL is reset.
    pub fn restore(&mut self, src: impl AsRef<Path>) -> Result<()> {
        let src = src.as_ref();
        let bytes =
            fs::read(src).map_err(|e| SlimDbError::Io(format!("read {}: {e}", src.display())))?;
        let header = parse_db_header(&bytes)?;
        let mut state = BTreeMap::new();
        let mut max_seq = 0u64;
        let outcome = replay(&bytes, DB_HEADER_LEN, 0, false, true, |rec| {
            apply_record_to_state(&mut state, rec)
        })?;
        max_seq = max_seq.max(outcome.max_seq);

        // Durable, atomic replacement of the snapshot.
        let tmp = temp_path_for(&self.path);
        fs::copy(src, &tmp).map_err(|e| SlimDbError::Io(format!("copy backup: {e}")))?;
        {
            let f = File::open(&tmp).map_err(|e| SlimDbError::Io(format!("open {tmp:?}: {e}")))?;
            f.sync_all()
                .map_err(|e| SlimDbError::Io(format!("fsync backup copy: {e}")))?;
        }
        fs::rename(&tmp, &self.path)
            .map_err(|e| SlimDbError::Io(format!("rename snapshot: {e}")))?;
        sync_parent_dir(&self.path)?;

        self.wal.reset(header.db_id)?;
        self.state = state;
        self.db_id = header.db_id;
        self.seq = self.seq.max(max_seq + 1);
        Ok(())
    }

    /// Start a transaction. The transaction is applied atomically: either all
    /// of its writes are committed or none are, including across crashes.
    pub fn transaction(&mut self) -> Transaction<'_> {
        Transaction::new(self)
    }
}

/// An atomic batch of writes.
///
/// Dropping a transaction without committing rolls it back. A transaction
/// committed to a crash-safe database is never partially visible, even after
/// a `kill -9` in the middle of the commit.
pub struct Transaction<'a> {
    db: &'a mut Db,
    undo: Vec<UndoOp>,
    batch: Vec<u8>,
    finished: bool,
    has_records: bool,
}

enum UndoOp {
    Set { path: String, prev: Option<Value> },
    Clear { prev: BTreeMap<String, Value> },
}

impl<'a> Transaction<'a> {
    fn new(db: &'a mut Db) -> Self {
        Transaction {
            db,
            undo: Vec::new(),
            batch: Vec::new(),
            finished: false,
            has_records: false,
        }
    }

    /// Read a value within the transaction's view.
    pub fn get(&self, path: impl AsRef<str>) -> Result<Option<Value>> {
        self.db.get(path)
    }

    /// Queue a write at `path`.
    pub fn set(&mut self, path: impl AsRef<str>, value: Value) -> Result<()> {
        let path = path.as_ref();
        let prev = self.db.get(path)?;
        self.undo.push(UndoOp::Set {
            path: path.to_string(),
            prev,
        });
        let segs = split_path(path)?;
        validate_path_shape(&self.db.state, &segs)?;
        let value = value.canonicalize();
        let rec = Record::set(path, &value, self.db.seq)?;
        self.db.seq += 1;
        rec.encode(&mut self.batch)?;
        set_at_path(&mut self.db.state, &segs, value)?;
        self.has_records = true;
        Ok(())
    }

    /// Queue a delete at `path`. Returns `true` if the path existed.
    pub fn delete(&mut self, path: impl AsRef<str>) -> Result<bool> {
        let path = path.as_ref();
        let segs = split_path(path)?;
        if !exists_at_path(&self.db.state, &segs) {
            return Ok(false);
        }
        let prev = self.db.get(path)?;
        self.undo.push(UndoOp::Set {
            path: path.to_string(),
            prev,
        });
        let rec = Record::delete(path, self.db.seq)?;
        self.db.seq += 1;
        rec.encode(&mut self.batch)?;
        delete_at_path(&mut self.db.state, &segs);
        self.has_records = true;
        Ok(true)
    }

    /// Queue a full clear.
    pub fn clear(&mut self) -> Result<()> {
        if self.db.state.is_empty() {
            return Ok(());
        }
        self.undo.push(UndoOp::Clear {
            prev: self.db.state.clone(),
        });
        let rec = Record::clear(self.db.seq);
        self.db.seq += 1;
        rec.encode(&mut self.batch)?;
        self.db.state.clear();
        self.has_records = true;
        Ok(())
    }

    /// Commit all queued writes atomically: one WAL append (BEGIN + batch +
    /// COMMIT), fsynced in safe mode.
    ///
    /// On failure the in-memory state is rolled back.
    pub fn commit(mut self) -> Result<()> {
        self.finished = true;
        if !self.has_records {
            return Ok(());
        }
        let mut buf = Vec::new();
        Record::txn_begin(self.db.seq).encode(&mut buf)?;
        self.db.seq += 1;
        buf.extend_from_slice(&self.batch);
        Record::txn_commit(self.db.seq).encode(&mut buf)?;
        self.db.seq += 1;
        if let Err(e) = self.db.wal.append_bytes(&buf) {
            self.rollback_undo();
            return Err(e);
        }
        Ok(())
    }

    /// Discard all queued writes and restore the previous state.
    pub fn rollback(mut self) {
        self.finished = true;
        self.rollback_undo();
    }

    fn rollback_undo(&mut self) {
        for op in self.undo.iter().rev() {
            match op {
                UndoOp::Set { path, prev } => {
                    if let Ok(segs) = split_path(path) {
                        match prev {
                            Some(v) => {
                                let _ = set_at_path(&mut self.db.state, &segs, v.clone());
                            }
                            None => {
                                delete_at_path(&mut self.db.state, &segs);
                            }
                        }
                    }
                }
                UndoOp::Clear { prev } => {
                    self.db.state = prev.clone();
                }
            }
        }
        self.undo.clear();
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.rollback_undo();
        }
    }
}

fn apply_record_to_state(state: &mut BTreeMap<String, Value>, rec: &Record) -> Result<()> {
    match rec.rtype {
        TYPE_SET => {
            let (path_str, value) = rec.set_payload()?;
            let segs = split_path(&path_str)?;
            validate_path_shape(state, &segs)?;
            set_at_path(state, &segs, value.canonicalize())?;
            Ok(())
        }
        TYPE_DELETE => {
            let path_str = rec.delete_payload()?;
            let segs = split_path(&path_str)?;
            delete_at_path(state, &segs);
            Ok(())
        }
        TYPE_CLEAR => {
            state.clear();
            Ok(())
        }
        other => Err(SlimDbError::Corrupt(format!(
            "unexpected record type {other}"
        ))),
    }
}

fn wal_path_for(data: &Path) -> PathBuf {
    let mut s = data.as_os_str().to_os_string();
    s.push("-wal");
    PathBuf::from(s)
}

fn temp_path_for(data: &Path) -> PathBuf {
    let mut s = data.as_os_str().to_os_string();
    s.push(".tmp");
    PathBuf::from(s)
}

fn sync_parent_dir(path: &Path) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Generate a fresh database id (not security-critical; used to detect stale
/// WAL files).
fn new_db_id() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut x = nanos ^ ((std::process::id() as u64) << 32) ^ 0x9E37_79B9_7F4A_7C15;
    // SplitMix64 finalizer.
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}
