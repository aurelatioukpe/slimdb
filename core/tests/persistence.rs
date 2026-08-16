mod common;

use common::TestDir;
use slimdb_core::{Db, DurabilityMode, Value};

#[test]
fn write_close_reopen_preserves_state() {
    let dir = TestDir::new("reopen");
    let path = dir.db("game.sdb");
    {
        let mut db = Db::open(&path).unwrap();
        db.set("player.level", Value::Int(42)).unwrap();
        db.set("player.name", Value::Str("Aurel".to_string()))
            .unwrap();
        db.set("items", Value::Array(vec![Value::Int(1), Value::Int(2)]))
            .unwrap();
        db.close().unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(db.get("player.level").unwrap(), Some(Value::Int(42)));
    assert_eq!(
        db.get("player.name").unwrap(),
        Some(Value::Str("Aurel".to_string()))
    );
    assert_eq!(
        db.get("items").unwrap(),
        Some(Value::Array(vec![Value::Int(1), Value::Int(2)]))
    );
}

#[test]
fn open_without_close_recovers_from_wal() {
    let dir = TestDir::new("walonly");
    let path = dir.db("game.sdb");
    // Create a DB, write, and drop without close: state lives in the WAL.
    {
        let mut db = Db::open(&path).unwrap();
        db.set("a", Value::Int(1)).unwrap();
        db.set("b.c", Value::Int(2)).unwrap();
        // No close: drop.
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(db.get("a").unwrap(), Some(Value::Int(1)));
    assert_eq!(db.get("b.c").unwrap(), Some(Value::Int(2)));
}

#[test]
fn durability_modes_reopen() {
    for mode in [
        DurabilityMode::Safe,
        DurabilityMode::Balanced,
        DurabilityMode::Fast,
    ] {
        let dir = TestDir::new("modes");
        let path = dir.db("game.sdb");
        {
            let mut db = Db::open_with_mode(&path, mode).unwrap();
            assert_eq!(db.durability(), mode);
            for i in 0..50 {
                db.set(format!("k{i}"), Value::Int(i)).unwrap();
            }
            // Drop without close: buffered writes may be lost in fast mode,
            // but safe/balanced must survive via flush or fsync-per-write.
            db.flush().unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.get("k0").unwrap(), Some(Value::Int(0)));
        assert_eq!(db.get("k49").unwrap(), Some(Value::Int(49)));
    }
}

#[test]
fn compaction_shrinks_wal_and_preserves_state() {
    let dir = TestDir::new("compact");
    let path = dir.db("game.sdb");
    let mut db = Db::open(&path).unwrap();
    for i in 0..200 {
        db.set(format!("key{i}"), Value::Int(i)).unwrap();
    }
    let wal_before = db.stats().unwrap().wal_file_size;
    assert!(wal_before > 0);
    db.compact().unwrap();
    let stats = db.stats().unwrap();
    assert_eq!(stats.keys, 200);
    assert!(stats.data_file_size > 0);
    // WAL is reset to header size only.
    assert_eq!(stats.wal_file_size, 20);
    db.close().unwrap();

    let db = Db::open(&path).unwrap();
    assert_eq!(db.get("key199").unwrap(), Some(Value::Int(199)));
    assert_eq!(db.get("key0").unwrap(), Some(Value::Int(0)));
}

#[test]
fn writes_after_compaction_persist() {
    let dir = TestDir::new("compact2");
    let path = dir.db("game.sdb");
    let mut db = Db::open(&path).unwrap();
    db.set("a", Value::Int(1)).unwrap();
    db.compact().unwrap();
    db.set("b", Value::Int(2)).unwrap();
    db.close().unwrap();

    let db = Db::open(&path).unwrap();
    assert_eq!(db.get("a").unwrap(), Some(Value::Int(1)));
    assert_eq!(db.get("b").unwrap(), Some(Value::Int(2)));
}

#[test]
fn backup_restore_roundtrip() {
    let dir = TestDir::new("backup");
    let path = dir.db("game.sdb");
    let backup = dir.db("backup.sdb");

    let mut db = Db::open(&path).unwrap();
    db.set("player.level", Value::Int(7)).unwrap();
    db.backup(&backup).unwrap();

    // More writes after the backup.
    db.set("player.level", Value::Int(99)).unwrap();
    db.set("extra", Value::Str("x".to_string())).unwrap();

    // The backup file is a valid standalone DB.
    let b = Db::open(&backup).unwrap();
    assert_eq!(b.get("player.level").unwrap(), Some(Value::Int(7)));

    // Restore rolls the live DB back to the backup state.
    db.restore(&backup).unwrap();
    assert_eq!(db.get("player.level").unwrap(), Some(Value::Int(7)));
    assert_eq!(db.get("extra").unwrap(), None);

    // The restored state survives reopen.
    db.close().unwrap();
    let db = Db::open(&path).unwrap();
    assert_eq!(db.get("player.level").unwrap(), Some(Value::Int(7)));
}

#[test]
fn stats_report_sizes() {
    let dir = TestDir::new("stats");
    let path = dir.db("game.sdb");
    let mut db = Db::open(&path).unwrap();
    db.set("a", Value::Int(1)).unwrap();
    let stats = db.stats().unwrap();
    assert_eq!(stats.keys, 1);
    assert_eq!(stats.durability, DurabilityMode::Safe);
    assert!(stats.wal_file_size >= 20);
}

#[test]
fn stale_wal_is_discarded() {
    let dir = TestDir::new("stale-wal");
    let path_a = dir.db("a.sdb");
    let path_b = dir.db("b.sdb");

    // Two independent databases.
    {
        let mut a = Db::open(&path_a).unwrap();
        a.set("a", Value::Int(1)).unwrap();
        a.close().unwrap();
    }
    {
        let mut b = Db::open(&path_b).unwrap();
        b.set("b", Value::Int(2)).unwrap();
        b.close().unwrap();
    }
    // Copy B's WAL next to A: it must be detected as stale (different
    // database id) and discarded, not replayed.
    let wal_a = dir.path.join("a.sdb-wal");
    let wal_b = dir.path.join("b.sdb-wal");
    std::fs::copy(&wal_b, &wal_a).unwrap();

    let a = Db::open(&path_a).unwrap();
    assert_eq!(a.get("a").unwrap(), Some(Value::Int(1)));
    assert_eq!(a.get("b").unwrap(), None);
}

#[test]
fn snapshot_corruption_is_a_clear_error() {
    let dir = TestDir::new("snapshot-corrupt");
    let path = dir.db("game.sdb");
    {
        let mut db = Db::open(&path).unwrap();
        db.set("a", Value::Int(1)).unwrap();
        db.close().unwrap();
    }
    // Corrupt the snapshot header.
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] ^= 0xFF;
    std::fs::write(&path, &bytes).unwrap();
    assert!(Db::open(&path).is_err());
}
