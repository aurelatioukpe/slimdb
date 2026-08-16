//! The write-ahead log.
//!
//! Every mutation is appended to the WAL before it is reflected in the
//! in-memory state, and the WAL is fsynced according to the durability mode:
//!
//! * `Safe` (default) — fsync after every write.
//! * `Balanced` — fsync periodically (100 ms or 1 MiB of buffered writes).
//! * `Fast` — no fsync; the OS decides when data hits disk.
//!
//! The WAL shares the record framing of the snapshot file. It carries a small
//! header binding it to a specific database id, so a stale WAL left next to a
//! different snapshot is detected and discarded instead of being replayed.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::error::{Result, SlimDbError};
use crate::record::{write_wal_header, Record};

/// WAL flush threshold for `Balanced` mode (bytes).
const BALANCED_BYTES: u64 = 1 << 20;
/// WAL flush threshold for `Balanced` mode (time since last fsync).
const BALANCED_MILLIS: u128 = 100;

/// Durability guarantees offered by the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurabilityMode {
    /// fsync after every write. Default; no configuration required.
    Safe,
    /// Periodic flush (documented compromise for write-heavy workloads).
    Balanced,
    /// Buffered writes only; for non-critical use.
    Fast,
}

impl DurabilityMode {
    /// Human-readable name.
    pub fn name(&self) -> &'static str {
        match self {
            DurabilityMode::Safe => "safe",
            DurabilityMode::Balanced => "balanced",
            DurabilityMode::Fast => "fast",
        }
    }

    /// Parse a mode from its name.
    pub fn parse(s: &str) -> Option<DurabilityMode> {
        match s {
            "safe" => Some(DurabilityMode::Safe),
            "balanced" => Some(DurabilityMode::Balanced),
            "fast" => Some(DurabilityMode::Fast),
            _ => None,
        }
    }
}

/// The write-ahead log file.
pub struct Wal {
    file: File,
    path: PathBuf,
    /// Bytes encoded but not yet written to the file.
    pending: Vec<u8>,
    mode: DurabilityMode,
    /// Bytes buffered since the last fsync.
    buffered_since_fsync: u64,
    /// When the last fsync happened.
    last_fsync: Instant,
}

impl Wal {
    /// Open (creating if needed) the WAL at `path`, truncating it to
    /// `truncate_to` bytes and optionally rewriting its header with `db_id`.
    ///
    /// The truncation/header write is fsynced before returning so that no
    /// stale tail can resurface after a later crash.
    pub fn open(
        path: &Path,
        db_id: u64,
        mode: DurabilityMode,
        truncate_to: Option<u64>,
        rewrite_header: bool,
    ) -> Result<Wal> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| SlimDbError::Io(format!("open WAL {}: {e}", path.display())))?;
        if let Some(n) = truncate_to {
            file.set_len(n)
                .map_err(|e| SlimDbError::Io(format!("truncate WAL: {e}")))?;
        }
        if rewrite_header {
            file.seek(SeekFrom::Start(0))
                .map_err(|e| SlimDbError::Io(format!("seek WAL: {e}")))?;
            write_wal_header(&mut file, db_id)?;
        }
        // Appends must go at the end: `set_len` does not move the file
        // position, and a fresh handle starts at 0 (the header).
        file.seek(SeekFrom::End(0))
            .map_err(|e| SlimDbError::Io(format!("seek WAL: {e}")))?;
        file.sync_all()
            .map_err(|e| SlimDbError::Io(format!("sync WAL: {e}")))?;
        Ok(Wal {
            file,
            path: path.to_path_buf(),
            pending: Vec::new(),
            mode,
            buffered_since_fsync: 0,
            last_fsync: Instant::now(),
        })
    }

    /// Path of the WAL file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append an encoded record and flush per the durability mode.
    pub fn append_record(&mut self, rec: &Record) -> Result<()> {
        let start = self.pending.len();
        rec.encode(&mut self.pending)?;
        self.buffered_since_fsync += (self.pending.len() - start) as u64;
        self.maybe_flush()
    }

    /// Append raw encoded bytes (e.g. a transaction batch) and flush per the
    /// durability mode.
    pub fn append_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.pending.extend_from_slice(bytes);
        self.buffered_since_fsync += bytes.len() as u64;
        self.maybe_flush()
    }

    fn maybe_flush(&mut self) -> Result<()> {
        match self.mode {
            DurabilityMode::Safe => self.flush(true),
            DurabilityMode::Balanced => {
                let elapsed = self.last_fsync.elapsed().as_millis() >= BALANCED_MILLIS;
                if self.buffered_since_fsync >= BALANCED_BYTES || elapsed {
                    self.flush(true)
                } else {
                    Ok(())
                }
            }
            DurabilityMode::Fast => Ok(()),
        }
    }

    /// Write pending bytes to the file and fsync if `force_fsync` or the mode
    /// is `Safe`.
    pub fn flush(&mut self, force_fsync: bool) -> Result<()> {
        if !self.pending.is_empty() {
            self.file
                .write_all(&self.pending)
                .map_err(|e| SlimDbError::Io(format!("write WAL: {e}")))?;
            self.pending.clear();
        }
        self.buffered_since_fsync = 0;
        if force_fsync || self.mode == DurabilityMode::Safe {
            self.file
                .sync_all()
                .map_err(|e| SlimDbError::Io(format!("fsync WAL: {e}")))?;
        }
        self.last_fsync = Instant::now();
        Ok(())
    }

    /// Reset the WAL to a fresh empty log carrying `db_id`.
    ///
    /// Used after a snapshot has been durably written (compaction) or after
    /// a restore. The reset is fsynced before returning.
    pub fn reset(&mut self, db_id: u64) -> Result<()> {
        self.file
            .set_len(0)
            .map_err(|e| SlimDbError::Io(format!("truncate WAL: {e}")))?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| SlimDbError::Io(format!("seek WAL: {e}")))?;
        write_wal_header(&mut self.file, db_id)?;
        self.pending.clear();
        self.buffered_since_fsync = 0;
        self.file
            .sync_all()
            .map_err(|e| SlimDbError::Io(format!("fsync WAL: {e}")))?;
        self.last_fsync = Instant::now();
        Ok(())
    }
}
