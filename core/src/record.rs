//! The SlimDB on-disk format: database header and framed records.
//!
//! Both the snapshot file (`*.sdb`) and the WAL (`*.sdb-wal`) share the same
//! record framing. See `docs/FORMAT.md` for the full specification.
//!
//! Layout of a database file:
//!
//! ```text
//! [ header: 32 bytes ]
//! [ record ][ record ][ record ] ...
//! ```
//!
//! Layout of a record (little-endian):
//!
//! ```text
//! MAGIC(2) VERSION(1) TYPE(1) LENGTH(4) SEQUENCE(8) PAYLOAD(LENGTH) CRC32(4)
//! ```

use std::io::Write;

use crate::crc32::crc32;
use crate::document::Value;
use crate::error::{Result, SlimDbError};
use crate::path::split_path;

/// Magic of the database snapshot header (8 bytes).
pub const DB_MAGIC: [u8; 8] = *b"SLIMDB\x00\x00";
/// Size of the database snapshot header in bytes.
pub const DB_HEADER_LEN: usize = 32;
/// Current on-disk format version.
pub const FORMAT_VERSION: u32 = 1;

/// Magic of the WAL header (4 bytes).
pub const WAL_MAGIC: [u8; 4] = *b"SWAL";
/// Size of the WAL header in bytes.
pub const WAL_HEADER_LEN: usize = 20;

/// Record magic (2 bytes).
const RECORD_MAGIC: [u8; 2] = *b"SD";
/// Record version byte.
const RECORD_VERSION: u8 = 1;
/// Size of the record frame excluding payload and checksum.
pub const RECORD_FRAME_LEN: usize = 16;
/// Size of the trailing checksum.
pub const RECORD_CRC_LEN: usize = 4;

/// Record type: write a value at a path.
pub const TYPE_SET: u8 = 0;
/// Record type: delete a path.
pub const TYPE_DELETE: u8 = 1;
/// Record type: clear the whole database.
pub const TYPE_CLEAR: u8 = 2;
/// Record type: begin a transaction.
pub const TYPE_TXN_BEGIN: u8 = 3;
/// Record type: commit a transaction.
pub const TYPE_TXN_COMMIT: u8 = 4;
/// Record type: roll back a transaction.
pub const TYPE_TXN_ROLLBACK: u8 = 5;

/// A framed WAL/snapshot record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// One of the `TYPE_*` constants.
    pub rtype: u8,
    /// Monotonically increasing write sequence number.
    pub seq: u64,
    /// Type-specific payload.
    pub payload: Vec<u8>,
}

/// The parsed database snapshot header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DbHeader {
    /// Format version found on disk.
    pub format_version: u32,
    /// Database identifier, unique per database file.
    pub db_id: u64,
    /// Highest WAL sequence number included in this snapshot. On replay,
    /// WAL records with a sequence <= this value are already contained in
    /// the snapshot and are skipped (they may not be valid to re-apply on a
    /// state that has since advanced). See docs/FORMAT.md §"Recovery".
    pub last_seq: u64,
}

/// Result of attempting to parse a record at a position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    /// A fully valid record starting at `start`, ending at `end` (exclusive).
    Record { rec: Record, end: usize },
    /// The frame itself is invalid (bad magic/version, truncated, length
    /// beyond EOF). The record spans from `start` to the end of the file.
    BadFrame { start: usize },
    /// The frame parsed but the payload checksum did not match. The record
    /// claims to span `[start, end)`.
    BadChecksum { start: usize, end: usize },
    /// The frame and checksum are fine but the payload cannot be decoded.
    BadPayload { start: usize, end: usize },
}

impl Record {
    /// Build a SET record (payload: `u32 path_len || path || value`).
    pub fn set(path: &str, value: &Value, seq: u64) -> Result<Record> {
        split_path(path)?;
        let mut payload = Vec::new();
        push_u32(&mut payload, path.len())?;
        payload.extend_from_slice(path.as_bytes());
        value.encode(&mut payload)?;
        Ok(Record {
            rtype: TYPE_SET,
            seq,
            payload,
        })
    }

    /// Build a DELETE record (payload: `u32 path_len || path`).
    pub fn delete(path: &str, seq: u64) -> Result<Record> {
        split_path(path)?;
        let mut payload = Vec::new();
        push_u32(&mut payload, path.len())?;
        payload.extend_from_slice(path.as_bytes());
        Ok(Record {
            rtype: TYPE_DELETE,
            seq,
            payload,
        })
    }

    /// Build a CLEAR record.
    pub fn clear(seq: u64) -> Record {
        Record {
            rtype: TYPE_CLEAR,
            seq,
            payload: Vec::new(),
        }
    }

    /// Build a transaction begin marker.
    pub fn txn_begin(seq: u64) -> Record {
        Record {
            rtype: TYPE_TXN_BEGIN,
            seq,
            payload: Vec::new(),
        }
    }

    /// Build a transaction commit marker.
    pub fn txn_commit(seq: u64) -> Record {
        Record {
            rtype: TYPE_TXN_COMMIT,
            seq,
            payload: Vec::new(),
        }
    }

    /// Build a transaction rollback marker.
    pub fn txn_rollback(seq: u64) -> Record {
        Record {
            rtype: TYPE_TXN_ROLLBACK,
            seq,
            payload: Vec::new(),
        }
    }

    /// Append the encoded bytes of this record to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let start = out.len();
        out.extend_from_slice(&RECORD_MAGIC);
        out.push(RECORD_VERSION);
        out.push(self.rtype);
        push_u32(out, self.payload.len())?;
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.payload);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_le_bytes());
        Ok(())
    }

    /// Parse the SET payload into `(path, value)`.
    pub fn set_payload(&self) -> Result<(String, Value)> {
        if self.rtype != TYPE_SET {
            return Err(SlimDbError::Decode(
                "set_payload called on non-SET record".to_string(),
            ));
        }
        let mut cur = 0;
        let path_len = take_u32(&self.payload, &mut cur)? as usize;
        let path_bytes = take(&self.payload, &mut cur, path_len)?;
        let path = std::str::from_utf8(path_bytes)
            .map_err(|_| SlimDbError::Decode("path is not valid UTF-8".to_string()))?
            .to_string();
        split_path(&path)?;
        let value = Value::decode_all(&self.payload[cur..])?;
        Ok((path, value))
    }

    /// Parse the DELETE payload into `path`.
    pub fn delete_payload(&self) -> Result<String> {
        if self.rtype != TYPE_DELETE {
            return Err(SlimDbError::Decode(
                "delete_payload called on non-DELETE record".to_string(),
            ));
        }
        let mut cur = 0;
        let path_len = take_u32(&self.payload, &mut cur)? as usize;
        let path_bytes = take(&self.payload, &mut cur, path_len)?;
        if cur != self.payload.len() {
            return Err(SlimDbError::Decode(
                "trailing bytes in DELETE payload".to_string(),
            ));
        }
        let path = std::str::from_utf8(path_bytes)
            .map_err(|_| SlimDbError::Decode("path is not valid UTF-8".to_string()))?
            .to_string();
        split_path(&path)?;
        Ok(path)
    }
}

/// Try to parse a record starting at `start` in `bytes`.
///
/// Returns [`ParseOutcome::Record`] on success and one of the failure
/// variants otherwise. See [`ParseOutcome`] for how failures are classified
/// (the recovery layer uses this to distinguish a truncatable tail from real
/// corruption).
pub fn parse_record(bytes: &[u8], start: usize) -> ParseOutcome {
    if start + RECORD_FRAME_LEN + RECORD_CRC_LEN > bytes.len() {
        return ParseOutcome::BadFrame { start };
    }
    if bytes[start..start + 2] != RECORD_MAGIC {
        return ParseOutcome::BadFrame { start };
    }
    if bytes[start + 2] != RECORD_VERSION {
        return ParseOutcome::BadFrame { start };
    }
    let rtype = bytes[start + 3];
    let len = u32::from_le_bytes(bytes[start + 4..start + 8].try_into().unwrap());
    let seq = u64::from_le_bytes(bytes[start + 8..start + 16].try_into().unwrap());
    let payload_start = start + RECORD_FRAME_LEN;
    let end = payload_start + len as usize + RECORD_CRC_LEN;
    if end > bytes.len() {
        return ParseOutcome::BadFrame { start };
    }
    let expected = crc32(&bytes[start..payload_start + len as usize]);
    let stored = u32::from_le_bytes(bytes[payload_start + len as usize..end].try_into().unwrap());
    if expected != stored {
        return ParseOutcome::BadChecksum { start, end };
    }
    let payload = &bytes[payload_start..payload_start + len as usize];
    let rec = match decode_payload(rtype, seq, payload) {
        Ok(rec) => rec,
        Err(_) => return ParseOutcome::BadPayload { start, end },
    };
    ParseOutcome::Record { rec, end }
}

fn decode_payload(rtype: u8, seq: u64, payload: &[u8]) -> Result<Record> {
    match rtype {
        TYPE_SET => {
            let rec = Record {
                rtype,
                seq,
                payload: payload.to_vec(),
            };
            // Validate fully so replay never fails later.
            rec.set_payload()?;
            Ok(rec)
        }
        TYPE_DELETE => {
            let rec = Record {
                rtype,
                seq,
                payload: payload.to_vec(),
            };
            rec.delete_payload()?;
            Ok(rec)
        }
        TYPE_CLEAR | TYPE_TXN_BEGIN | TYPE_TXN_COMMIT | TYPE_TXN_ROLLBACK => {
            if !payload.is_empty() {
                return Err(SlimDbError::Decode(format!(
                    "record type {rtype} must have an empty payload"
                )));
            }
            Ok(Record {
                rtype,
                seq,
                payload: Vec::new(),
            })
        }
        other => Err(SlimDbError::Decode(format!("unknown record type {other}"))),
    }
}

/// Write the database snapshot header to `out`.
pub fn write_db_header(out: &mut impl Write, db_id: u64, last_seq: u64) -> Result<()> {
    let mut buf = Vec::with_capacity(DB_HEADER_LEN);
    buf.extend_from_slice(&DB_MAGIC);
    buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    buf.extend_from_slice(&db_id.to_le_bytes());
    buf.extend_from_slice(&last_seq.to_le_bytes());
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    debug_assert_eq!(buf.len(), DB_HEADER_LEN);
    out.write_all(&buf)
        .map_err(|e| SlimDbError::Io(format!("write header: {e}")))
}

/// Parse and validate the database snapshot header.
pub fn parse_db_header(bytes: &[u8]) -> Result<DbHeader> {
    if bytes.len() < DB_HEADER_LEN {
        return Err(SlimDbError::Corrupt(
            "database file shorter than header".to_string(),
        ));
    }
    if bytes[0..8] != DB_MAGIC {
        return Err(SlimDbError::Corrupt("bad database magic".to_string()));
    }
    let format_version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    // Check the version before the checksum: a file written by a newer major
    // version must fail with a clear message, not a vague corruption error.
    if format_version != FORMAT_VERSION {
        return Err(SlimDbError::UnsupportedVersion(format_version));
    }
    let db_id = u64::from_le_bytes(bytes[12..20].try_into().unwrap());
    let last_seq = u64::from_le_bytes(bytes[20..28].try_into().unwrap());
    let stored = u32::from_le_bytes(bytes[28..32].try_into().unwrap());
    let expected = crc32(&bytes[0..28]);
    if stored != expected {
        return Err(SlimDbError::Corrupt(
            "database header checksum mismatch".to_string(),
        ));
    }
    Ok(DbHeader {
        format_version,
        db_id,
        last_seq,
    })
}

/// Write the WAL header to `out`.
pub fn write_wal_header(out: &mut impl Write, db_id: u64) -> Result<()> {
    let mut buf = Vec::with_capacity(WAL_HEADER_LEN);
    buf.extend_from_slice(&WAL_MAGIC);
    buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    buf.extend_from_slice(&db_id.to_le_bytes());
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    debug_assert_eq!(buf.len(), WAL_HEADER_LEN);
    out.write_all(&buf)
        .map_err(|e| SlimDbError::Io(format!("write WAL header: {e}")))
}

/// Parse the WAL header, returning the database id it belongs to, or `None`
/// if the header is missing/invalid (treated as an empty/stale WAL).
pub fn parse_wal_header(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < WAL_HEADER_LEN {
        return None;
    }
    if bytes[0..4] != WAL_MAGIC {
        return None;
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if version != FORMAT_VERSION {
        return None;
    }
    let db_id = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    let stored = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
    if crc32(&bytes[0..16]) != stored {
        return None;
    }
    Some(db_id)
}

fn push_u32(out: &mut Vec<u8>, len: usize) -> Result<()> {
    let len = u32::try_from(len)
        .map_err(|_| SlimDbError::InvalidArgument("payload too large".to_string()))?;
    out.extend_from_slice(&len.to_le_bytes());
    Ok(())
}

fn take_u32(bytes: &[u8], pos: &mut usize) -> Result<u32> {
    let b = take(bytes, pos, 4)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn take<'a>(bytes: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8]> {
    if *pos + n > bytes.len() {
        return Err(SlimDbError::Decode("unexpected end of payload".to_string()));
    }
    let slice = &bytes[*pos..*pos + n];
    *pos += n;
    Ok(slice)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_roundtrip() {
        let value = Value::Object(
            [
                ("a".to_string(), Value::Int(1)),
                ("b".to_string(), Value::Str("x".to_string())),
            ]
            .into_iter()
            .collect(),
        );
        let rec = Record::set("player.level", &value, 7).unwrap();
        let mut buf = Vec::new();
        rec.encode(&mut buf).unwrap();

        match parse_record(&buf, 0) {
            ParseOutcome::Record { rec: parsed, end } => {
                assert_eq!(end, buf.len());
                assert_eq!(parsed.rtype, TYPE_SET);
                assert_eq!(parsed.seq, 7);
                let (path, v) = parsed.set_payload().unwrap();
                assert_eq!(path, "player.level");
                assert_eq!(v, value);
            }
            other => panic!("expected record, got {other:?}"),
        }
    }

    #[test]
    fn parse_detects_corruption() {
        let rec = Record::set("a", &Value::Int(1), 1).unwrap();
        let mut buf = Vec::new();
        rec.encode(&mut buf).unwrap();

        // Flip a byte in the payload.
        let mut corrupted = buf.clone();
        let idx = RECORD_FRAME_LEN + 2;
        corrupted[idx] ^= 0xFF;
        assert!(matches!(
            parse_record(&corrupted, 0),
            ParseOutcome::BadChecksum { .. }
        ));

        // Truncate mid-record.
        let truncated = &buf[..buf.len() - 3];
        assert!(matches!(
            parse_record(truncated, 0),
            ParseOutcome::BadFrame { .. }
        ));

        // Unknown record type.
        let mut bad_type = buf.clone();
        bad_type[3] = 99;
        let last = bad_type.len() - 1;
        bad_type[last] ^= 0xFF; // invalidate crc
                                // CRC check fails first, which is fine.
        assert!(matches!(
            parse_record(&bad_type, 0),
            ParseOutcome::BadChecksum { .. }
        ));
    }

    #[test]
    fn header_roundtrip() {
        let mut buf = Vec::new();
        write_db_header(&mut buf, 0xDEAD_BEEF, 42).unwrap();
        let header = parse_db_header(&buf).unwrap();
        assert_eq!(header.db_id, 0xDEAD_BEEF);
        assert_eq!(header.format_version, FORMAT_VERSION);
        assert_eq!(header.last_seq, 42);
    }

    #[test]
    fn header_rejects_tampering() {
        let mut buf = Vec::new();
        write_db_header(&mut buf, 1, 0).unwrap();
        buf[0] ^= 0x01;
        assert!(matches!(
            parse_db_header(&buf),
            Err(SlimDbError::Corrupt(_))
        ));

        let mut buf = Vec::new();
        write_db_header(&mut buf, 1, 0).unwrap();
        // Change the version without fixing the checksum.
        buf[8..12].copy_from_slice(&2u32.to_le_bytes());
        assert!(matches!(
            parse_db_header(&buf),
            Err(SlimDbError::UnsupportedVersion(2))
        ));
    }

    #[test]
    fn wal_header_roundtrip() {
        let mut buf = Vec::new();
        write_wal_header(&mut buf, 42).unwrap();
        assert_eq!(parse_wal_header(&buf), Some(42));
        buf[0] ^= 0xFF;
        assert_eq!(parse_wal_header(&buf), None);
    }
}
