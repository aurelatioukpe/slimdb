//! In-process crash fuzzing.
//!
//! Each iteration:
//! 1. Applies a deterministic stream of random ops (sets, deletes, clears,
//!    transactions, occasional compaction) to a real database and a shadow.
//! 2. Simulates a crash by dropping the handle without closing, then
//!    truncating the WAL at a random byte offset (torn write).
//! 3. Reopens and asserts the recovered state equals the shadow state after
//!    *some* prefix of the ops — never more, never corrupted.
//!
//! The real `kill -9` variant lives in `tests/crash-tester` (a separate
//! crate); this loop runs thousands of iterations per second in CI.

mod common;

use common::{apply_deterministic_ops, Shadow, TestDir};
use slimdb_core::{Db, DurabilityMode, SlimDbError, Value};

/// Number of fuzz iterations (override with SLIMDB_FUZZ_ITERS).
fn iterations() -> usize {
    std::env::var("SLIMDB_FUZZ_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(250)
}

fn recovered_matches_prefix(recovered: &Shadow, history: &[Shadow]) -> bool {
    history
        .iter()
        .any(|h| h.map == recovered.map && h.map.len() == recovered.map.len())
}

#[test]
fn torn_wal_never_loses_committed_prefix() {
    let iters = iterations();
    for it in 0..iters {
        let dir = TestDir::new("fuzz-torn");
        let path = dir.db("f.sdb");
        let wal_path = dir.path.join("f.sdb-wal");
        let seed = 0x5EED_0000u64 ^ (it as u64).wrapping_mul(0x9E37_79B9);

        // 1. Build the op stream and the shadow history.
        let mut shadow = Shadow::new();
        let ops = apply_deterministic_ops(&mut shadow, seed, 30 + (seed % 40) as usize);
        let mut history: Vec<Shadow> = vec![Shadow::new()];
        {
            let mut s = Shadow::new();
            for op in &ops {
                s.apply(op);
                history.push(s.clone());
            }
        }

        // 2. Apply ops to the real database.
        let mode = match it % 7 {
            5 => DurabilityMode::Balanced,
            6 => DurabilityMode::Fast,
            _ => DurabilityMode::Safe,
        };
        {
            let mut db = Db::open_with_mode(&path, mode).unwrap();
            for (i, op) in ops.iter().enumerate() {
                match op {
                    common::Op::Set { path, value } => db.set(path, value.clone()).unwrap(),
                    common::Op::Delete { path } => {
                        db.delete(path).unwrap();
                    }
                    common::Op::Clear => {
                        db.clear().unwrap();
                    }
                    common::Op::Txn(inner) => {
                        let mut txn = db.transaction();
                        for op in inner {
                            match op {
                                common::Op::Set { path, value } => {
                                    txn.set(path, value.clone()).unwrap()
                                }
                                common::Op::Delete { path } => {
                                    txn.delete(path).unwrap();
                                }
                                common::Op::Clear => txn.clear().unwrap(),
                                common::Op::Txn(_) => panic!("nested txn in test ops"),
                            }
                        }
                        txn.commit().unwrap();
                    }
                }
                // Occasionally compact mid-stream to exercise snapshot +
                // WAL replay together.
                if i > 0 && i % 11 == 0 {
                    db.compact().unwrap();
                }
            }
            // Simulate a crash: drop without close.
        }

        // 3. Simulate a crash. Most iterations truncate the WAL at a random
        //    byte (torn tail). A few splice garbage into the *middle* of the
        //    log: recovery must refuse to open, not silently truncate.
        if let Ok(wal_bytes) = std::fs::read(&wal_path) {
            let roll = (seed >> 16) % 20;
            if roll == 1 {
                // Mid-file corruption: only when there is enough valid data
                // after the splice that the forward scan must find it.
                let len = wal_bytes.len();
                if len >= 20 + 64 + 500 {
                    let p = 20 + (len - 20) / 2;
                    let mut corrupted = wal_bytes;
                    for b in corrupted.iter_mut().skip(p).take(64) {
                        *b = 0xEE;
                    }
                    std::fs::write(&wal_path, &corrupted).unwrap();
                    let opened = Db::open(&path);
                    let is_corrupt = matches!(&opened, Err(SlimDbError::Corrupt(_)));
                    drop(opened);
                    assert!(
                        is_corrupt,
                        "iteration {it}: mid-file corruption must be a loud error, not a silent truncation"
                    );
                    let _ = std::fs::remove_dir_all(&dir.path);
                    continue;
                }
            }
            let len = wal_bytes.len() as u64;
            let cut = if roll == 0 {
                // Torn write inside the header: WAL becomes unusable.
                (seed % 20) as usize
            } else {
                // Torn write at a random byte.
                (seed % len.max(1)) as usize
            };
            std::fs::write(&wal_path, &wal_bytes[..cut]).unwrap();
        }

        // 4. Reopen and check the prefix invariant.
        let mut recovered_shadow = Shadow::new();
        match Db::open(&path) {
            Ok(db) => {
                for key in db.keys() {
                    let v = db.get(&key).unwrap().unwrap();
                    recovered_shadow.map.insert(key, v);
                }
                assert!(
                    recovered_matches_prefix(&recovered_shadow, &history),
                    "iteration {it}: recovered state is not a prefix of the op history\n\
                     recovered: {:#?}\nops: {}",
                    recovered_shadow.map,
                    ops.len()
                );
                // 5. Recovery must be idempotent: reopening yields the same state.
                let db2 = Db::open(&path).unwrap();
                let mut again = Shadow::new();
                for key in db2.keys() {
                    let v = db2.get(&key).unwrap().unwrap();
                    again.map.insert(key, v);
                }
                assert_eq!(
                    again.map, recovered_shadow.map,
                    "iteration {it}: recovery not idempotent"
                );
                drop(db2);

                // 6. The recovered database must accept new writes and persist them.
                let mut db = db;
                let marker = Value::Str(format!("marker-{it}"));
                db.set("post-recovery-marker", marker.clone()).unwrap();
                db.close().unwrap();
                let db3 = Db::open(&path).unwrap();
                assert_eq!(
                    db3.get("post-recovery-marker").unwrap(),
                    Some(marker),
                    "iteration {it}: writes after recovery are lost"
                );
            }
            Err(e) => {
                // Corruption is acceptable only if the torn region was *not*
                // at the tail of the log. A random truncation always produces
                // a truncatable tail, so this must not happen.
                panic!("iteration {it}: unexpected open failure after torn tail: {e}");
            }
        }
    }
}
