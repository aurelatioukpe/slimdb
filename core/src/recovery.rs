//! Record scanning and replay.
//!
//! Both the snapshot file and the WAL are replayed through [`replay`]. The
//! function walks records sequentially, applies them through a callback, and
//! classifies the first unparseable record:
//!
//! * If a valid record exists *after* the invalid one, the file is genuinely
//!   corrupted → hard error.
//! * Otherwise the invalid region is the tail of a torn write (crash
//!   mid-append) → truncate at the end of the last applied record.
//!
//! Transactions are reconstructed by grouping records between `BEGIN` and
//! `COMMIT` markers; an uncommitted transaction at the tail is discarded.
//!
//! Records with `seq <= skip_before_seq` are parsed (their sequence still
//! counts toward `max_seq`) but not applied: they are already contained in
//! the snapshot, whose header records the highest WAL sequence it covers
//! (`DbHeader::last_seq`). Re-applying them would be incorrect — the state
//! may have advanced past the point where an older record was valid.

use crate::error::{Result, SlimDbError};
use crate::record::{
    parse_record, ParseOutcome, Record, TYPE_CLEAR, TYPE_DELETE, TYPE_SET, TYPE_TXN_BEGIN,
    TYPE_TXN_COMMIT, TYPE_TXN_ROLLBACK,
};

/// How far ahead to scan for a valid record after a broken frame, in bytes.
const FORWARD_SCAN_LIMIT: usize = 4096;

/// Outcome of a replay: where the WAL should be truncated to, and the highest
/// sequence number seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayOutcome {
    /// Byte offset to truncate the file to (exclusive). For snapshots this is
    /// ignored (snapshots are never truncated).
    pub truncate_to: usize,
    /// Highest record sequence number observed.
    pub max_seq: u64,
    /// Number of records applied to the state.
    pub applied_records: usize,
}

/// Replay records from `bytes[start..]`, invoking `apply` for every record
/// that must be reflected in state.
///
/// # Arguments
///
/// * `skip_before_seq` — records with `seq <= skip_before_seq` are parsed
///   but not applied: they are already contained in the snapshot (see
///   `DbHeader::last_seq`). Pass `0` to apply everything.
/// * `allow_tail_truncation` — if `false`, any invalid record is a hard
///   corruption error (used for the snapshot file, which is only ever written
///   atomically). If `true`, an invalid region with no valid record after it
///   is treated as a torn tail and truncated (used for the WAL).
/// * `forbid_txn` — if `true`, transaction markers are rejected (used for
///   the snapshot, which must contain only plain records).
pub fn replay<F>(
    bytes: &[u8],
    start: usize,
    skip_before_seq: u64,
    allow_tail_truncation: bool,
    forbid_txn: bool,
    mut apply: F,
) -> Result<ReplayOutcome>
where
    F: FnMut(&Record) -> Result<()>,
{
    let mut pos = start;
    let mut max_seq = 0u64;
    let mut applied = 0usize;
    // End offset of the last record that was (or will be) applied. This is
    // the truncation point on torn tails and on uncommitted tail txns.
    let mut applied_end = start;
    let mut pending: Option<Vec<Record>> = None;

    while pos < bytes.len() {
        match parse_record(bytes, pos) {
            ParseOutcome::Record { rec, end } => {
                max_seq = max_seq.max(rec.seq);
                match rec.rtype {
                    TYPE_TXN_BEGIN => {
                        if forbid_txn {
                            return Err(SlimDbError::Corrupt(
                                "transaction marker inside snapshot".to_string(),
                            ));
                        }
                        if pending.is_some() {
                            return Err(SlimDbError::Corrupt(
                                "nested transaction BEGIN".to_string(),
                            ));
                        }
                        pending = Some(Vec::new());
                    }
                    TYPE_TXN_COMMIT => {
                        if forbid_txn {
                            return Err(SlimDbError::Corrupt(
                                "transaction marker inside snapshot".to_string(),
                            ));
                        }
                        let batch = pending.take().ok_or_else(|| {
                            SlimDbError::Corrupt("COMMIT without BEGIN".to_string())
                        })?;
                        for r in &batch {
                            if r.seq > skip_before_seq {
                                apply(r)?;
                                applied += 1;
                            }
                        }
                        applied_end = end;
                    }
                    TYPE_TXN_ROLLBACK => {
                        if forbid_txn {
                            return Err(SlimDbError::Corrupt(
                                "transaction marker inside snapshot".to_string(),
                            ));
                        }
                        pending.take().ok_or_else(|| {
                            SlimDbError::Corrupt("ROLLBACK without BEGIN".to_string())
                        })?;
                        // The buffered records are discarded.
                    }
                    TYPE_SET | TYPE_DELETE | TYPE_CLEAR => {
                        if let Some(batch) = pending.as_mut() {
                            batch.push(rec);
                        } else if rec.seq > skip_before_seq {
                            apply(&rec)?;
                            applied += 1;
                            applied_end = end;
                        }
                    }
                    other => {
                        return Err(SlimDbError::Corrupt(format!("unknown record type {other}")))
                    }
                }
                pos = end;
            }
            ParseOutcome::BadFrame { start: p } => {
                if !allow_tail_truncation || valid_record_after(bytes, p, None) {
                    return Err(SlimDbError::Corrupt(format!(
                        "invalid record at byte {p} (corruption)"
                    )));
                }
                break; // torn tail
            }
            ParseOutcome::BadChecksum { start: p, end: q }
            | ParseOutcome::BadPayload { start: p, end: q } => {
                if !allow_tail_truncation || valid_record_after(bytes, p, Some(q)) {
                    return Err(SlimDbError::Corrupt(format!(
                        "invalid record at byte {p} (corruption)"
                    )));
                }
                break; // torn tail
            }
        }
    }

    // An open transaction at the end of the log is uncommitted: it is simply
    // never applied (truncation happens at `applied_end`).

    Ok(ReplayOutcome {
        truncate_to: applied_end,
        max_seq,
        applied_records: applied,
    })
}

/// Whether a valid record exists somewhere after an invalid one.
///
/// For a checksum failure we know the claimed span of the bad record, so the
/// next record would start exactly at its end. For a broken frame we scan a
/// bounded window ahead.
fn valid_record_after(bytes: &[u8], p: usize, claimed_end: Option<usize>) -> bool {
    if let Some(q) = claimed_end {
        return q < bytes.len() && matches!(parse_record(bytes, q), ParseOutcome::Record { .. });
    }
    let limit = (p + 1 + FORWARD_SCAN_LIMIT).min(bytes.len());
    (p + 1..limit).any(|q| matches!(parse_record(bytes, q), ParseOutcome::Record { .. }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Record;

    fn apply_all(state: &mut Vec<u64>, rec: &Record) -> Result<()> {
        state.push(rec.seq);
        Ok(())
    }

    fn make_chain(records: &[Record]) -> Vec<u8> {
        let mut buf = Vec::new();
        for r in records {
            r.encode(&mut buf).unwrap();
        }
        buf
    }

    #[test]
    fn plain_records_apply_in_order() {
        let recs = vec![
            Record::set("a", &crate::document::Value::Int(1), 1).unwrap(),
            Record::set("b", &crate::document::Value::Int(2), 2).unwrap(),
        ];
        let bytes = make_chain(&recs);
        let mut state = Vec::new();
        let out = replay(&bytes, 0, 0, true, false, |r| apply_all(&mut state, r)).unwrap();
        assert_eq!(state, vec![1, 2]);
        assert_eq!(out.truncate_to, bytes.len());
        assert_eq!(out.max_seq, 2);
    }

    #[test]
    fn committed_txn_applies() {
        let recs = vec![
            Record::txn_begin(1),
            Record::set("a", &crate::document::Value::Int(1), 2).unwrap(),
            Record::txn_commit(3),
            Record::set("b", &crate::document::Value::Int(2), 4).unwrap(),
        ];
        let bytes = make_chain(&recs);
        let mut state = Vec::new();
        let out = replay(&bytes, 0, 0, true, false, |r| apply_all(&mut state, r)).unwrap();
        assert_eq!(state, vec![2, 4]);
        assert_eq!(out.truncate_to, bytes.len());
    }

    fn encoded_len(rec: &Record) -> usize {
        let mut buf = Vec::new();
        rec.encode(&mut buf).unwrap();
        buf.len()
    }

    #[test]
    fn uncommitted_txn_is_discarded() {
        let recs = vec![
            Record::set("a", &crate::document::Value::Int(1), 1).unwrap(),
            Record::txn_begin(2),
            Record::set("b", &crate::document::Value::Int(2), 3).unwrap(),
            Record::set("c", &crate::document::Value::Int(3), 4).unwrap(),
        ];
        let bytes = make_chain(&recs);
        let mut state = Vec::new();
        let out = replay(&bytes, 0, 0, true, false, |r| apply_all(&mut state, r)).unwrap();
        assert_eq!(state, vec![1]);
        // Truncate at the end of the last applied record (before BEGIN).
        assert_eq!(out.truncate_to, encoded_len(&recs[0]));
    }

    #[test]
    fn rolled_back_txn_is_discarded() {
        let recs = vec![
            Record::txn_begin(1),
            Record::set("a", &crate::document::Value::Int(1), 2).unwrap(),
            Record::txn_rollback(3),
            Record::set("b", &crate::document::Value::Int(2), 4).unwrap(),
        ];
        let bytes = make_chain(&recs);
        let mut state = Vec::new();
        let out = replay(&bytes, 0, 0, true, false, |r| apply_all(&mut state, r)).unwrap();
        assert_eq!(state, vec![4]);
        assert_eq!(out.truncate_to, bytes.len());
    }

    #[test]
    fn torn_tail_truncates() {
        let recs = vec![
            Record::set("a", &crate::document::Value::Int(1), 1).unwrap(),
            Record::set("b", &crate::document::Value::Int(2), 2).unwrap(),
        ];
        let bytes = make_chain(&recs);
        // Chop off part of the last record.
        let torn = &bytes[..bytes.len() - 5];
        let mut state = Vec::new();
        let out = replay(torn, 0, 0, true, false, |r| apply_all(&mut state, r)).unwrap();
        assert_eq!(state, vec![1]);
        // Truncate at the end of the first (fully applied) record.
        assert_eq!(out.truncate_to, encoded_len(&recs[0]));
    }

    #[test]
    fn mid_file_garbage_is_corruption() {
        let recs = vec![
            Record::set("a", &crate::document::Value::Int(1), 1).unwrap(),
            Record::set("b", &crate::document::Value::Int(2), 2).unwrap(),
            Record::set("c", &crate::document::Value::Int(3), 3).unwrap(),
        ];
        let bytes = make_chain(&recs);
        let first_len = encoded_len(&recs[0]);
        let second_len = encoded_len(&recs[1]);
        // Replace the second record entirely with garbage, keeping the third.
        let mut spliced = Vec::new();
        spliced.extend_from_slice(&bytes[..first_len]);
        spliced.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD, 0xEE]);
        spliced.extend_from_slice(&bytes[first_len + second_len..]);
        let mut state = Vec::new();
        let result = replay(&spliced, 0, 0, true, false, |r| apply_all(&mut state, r));
        assert!(result.is_err());
    }

    #[test]
    fn snapshot_forbids_txn() {
        let recs = vec![Record::txn_begin(1), Record::txn_commit(2)];
        let bytes = make_chain(&recs);
        let result = replay(&bytes, 0, 0, false, true, |_| Ok(()));
        assert!(result.is_err());
    }

    #[test]
    fn snapshot_rejects_corruption() {
        let recs = vec![
            Record::set("a", &crate::document::Value::Int(1), 1).unwrap(),
            Record::set("b", &crate::document::Value::Int(2), 2).unwrap(),
        ];
        let bytes = make_chain(&recs);
        let torn = &bytes[..bytes.len() - 1];
        let result = replay(torn, 0, 0, false, false, |_| Ok(()));
        assert!(result.is_err());
    }

    #[test]
    fn records_at_or_below_skip_seq_are_not_applied() {
        // Simulates the compaction crash window: the snapshot already
        // contains every record up to `last_seq`, and the WAL still holds
        // them. They must be parsed (max_seq stays correct) but not
        // re-applied, and the WAL truncates to the start.
        let recs = vec![
            Record::set("a", &crate::document::Value::Int(1), 1).unwrap(),
            Record::set("b", &crate::document::Value::Int(2), 2).unwrap(),
        ];
        let bytes = make_chain(&recs);
        let mut state = Vec::new();
        let out = replay(&bytes, 0, 2, true, false, |r| apply_all(&mut state, r)).unwrap();
        assert!(state.is_empty());
        assert_eq!(out.truncate_to, 0);
        assert_eq!(out.max_seq, 2);
        assert_eq!(out.applied_records, 0);
    }

    #[test]
    fn records_above_skip_seq_are_applied() {
        // Records written after the snapshot boundary are not covered by it
        // and must apply normally.
        let recs = vec![
            Record::set("a", &crate::document::Value::Int(1), 1).unwrap(),
            Record::set("b", &crate::document::Value::Int(2), 2).unwrap(),
            Record::set("c", &crate::document::Value::Int(3), 3).unwrap(),
        ];
        let bytes = make_chain(&recs);
        let mut state = Vec::new();
        let out = replay(&bytes, 0, 2, true, false, |r| apply_all(&mut state, r)).unwrap();
        assert_eq!(state, vec![3]);
        assert_eq!(out.truncate_to, bytes.len());
    }
}
