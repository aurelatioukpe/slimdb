mod common;

use common::TestDir;
use slimdb_core::record::{write_wal_header, Record};
use slimdb_core::{Db, SlimDbError, Value};

/// Build a WAL file at `wal_path` containing the given records.
fn write_wal(wal_path: &std::path::Path, db_id: u64, records: &[Record]) {
    let mut buf = Vec::new();
    write_wal_header(&mut buf, db_id).unwrap();
    for rec in records {
        rec.encode(&mut buf).unwrap();
    }
    std::fs::write(wal_path, &buf).unwrap();
}

/// Create a database whose WAL contains exactly `records` (plus header),
/// using the same db_id as the freshly created database. The snapshot is
/// empty; the records replay on open.
fn db_with_wal(dir: &TestDir, name: &str, records: &[Record]) -> std::path::PathBuf {
    let path = dir.db(name);
    let wal_path = dir.path.join(format!("{name}-wal"));
    {
        let db = Db::open(&path).unwrap();
        db.close().unwrap();
    }
    let bytes = std::fs::read(&path).unwrap();
    let header = slimdb_core::record::parse_db_header(&bytes).unwrap();
    write_wal(&wal_path, header.db_id, records);
    path
}

#[test]
fn torn_tail_is_truncated() {
    let dir = TestDir::new("rec-torn");
    let records = [
        Record::set("a", &Value::Int(1), 1).unwrap(),
        Record::set("b", &Value::Int(2), 2).unwrap(),
    ];
    let path = db_with_wal(&dir, "t.sdb", &records);
    let wal_path = dir.path.join("t.sdb-wal");

    // Simulate a crash mid-append: chop bytes off the tail.
    let bytes = std::fs::read(&wal_path).unwrap();
    let cut = bytes.len() - 7;
    std::fs::write(&wal_path, &bytes[..cut]).unwrap();

    let db = Db::open(&path).unwrap();
    assert_eq!(db.get("a").unwrap(), Some(Value::Int(1)));
    assert_eq!(db.get("b").unwrap(), None);

    // The WAL must have been truncated at the end of the last valid record.
    let after = std::fs::read(&wal_path).unwrap();
    let mut expected = 20; // header
    let mut buf = Vec::new();
    records[0].encode(&mut buf).unwrap();
    expected += buf.len();
    assert_eq!(after.len(), expected);
}

#[test]
fn garbage_at_tail_is_truncated() {
    let dir = TestDir::new("rec-garbage-tail");
    let records = [Record::set("a", &Value::Int(1), 1).unwrap()];
    let path = db_with_wal(&dir, "t.sdb", &records);
    let wal_path = dir.path.join("t.sdb-wal");

    // Append garbage bytes (simulates a torn write of zeros/garbage).
    let mut bytes = std::fs::read(&wal_path).unwrap();
    bytes.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x00, 0x00, 0x00, 0xFF]);
    std::fs::write(&wal_path, &bytes).unwrap();

    let db = Db::open(&path).unwrap();
    assert_eq!(db.get("a").unwrap(), Some(Value::Int(1)));
}

#[test]
fn garbage_in_the_middle_is_corruption() {
    let dir = TestDir::new("rec-garbage-mid");
    let records = [
        Record::set("a", &Value::Int(1), 1).unwrap(),
        Record::set("b", &Value::Int(2), 2).unwrap(),
        Record::set("c", &Value::Int(3), 3).unwrap(),
    ];
    let path = db_with_wal(&dir, "t.sdb", &records);
    let wal_path = dir.path.join("t.sdb-wal");

    // Splice garbage over the middle record.
    let mut bytes = std::fs::read(&wal_path).unwrap();
    let mut first_end = 20; // header
    let mut buf = Vec::new();
    records[0].encode(&mut buf).unwrap();
    first_end += buf.len();
    let mut buf2 = Vec::new();
    records[1].encode(&mut buf2).unwrap();
    let second_len = buf2.len();
    for i in 0..second_len {
        bytes[first_end + i] = 0xEE;
    }
    std::fs::write(&wal_path, &bytes).unwrap();

    let result = Db::open(&path);
    assert!(matches!(result, Err(SlimDbError::Corrupt(_))));
}

#[test]
fn uncommitted_txn_is_discarded_and_truncated() {
    let dir = TestDir::new("rec-uncommitted");
    let records = [
        Record::set("a", &Value::Int(1), 1).unwrap(),
        Record::txn_begin(2),
        Record::set("b", &Value::Int(2), 3).unwrap(),
        Record::set("c", &Value::Int(3), 4).unwrap(),
        Record::txn_commit(5),
        Record::txn_begin(6),
        Record::set("d", &Value::Int(4), 7).unwrap(),
    ];
    let path = db_with_wal(&dir, "t.sdb", &records);
    let wal_path = dir.path.join("t.sdb-wal");

    let db = Db::open(&path).unwrap();
    assert_eq!(db.get("a").unwrap(), Some(Value::Int(1)));
    assert_eq!(db.get("b").unwrap(), Some(Value::Int(2)));
    assert_eq!(db.get("c").unwrap(), Some(Value::Int(3)));
    assert_eq!(db.get("d").unwrap(), None);

    // The WAL must be truncated after the last commit.
    let bytes = std::fs::read(&wal_path).unwrap();
    let mut expected = 20; // header
    for rec in &records[..5] {
        let mut buf = Vec::new();
        rec.encode(&mut buf).unwrap();
        expected += buf.len();
    }
    assert_eq!(bytes.len(), expected);
}

#[test]
fn torn_transaction_batch_is_discarded() {
    let dir = TestDir::new("rec-torn-batch");
    let records = [
        Record::set("a", &Value::Int(1), 1).unwrap(),
        Record::txn_begin(2),
        Record::set("b", &Value::Int(2), 3).unwrap(),
        Record::set("c", &Value::Int(3), 4).unwrap(),
        Record::txn_commit(5),
    ];
    let path = db_with_wal(&dir, "t.sdb", &records);
    let wal_path = dir.path.join("t.sdb-wal");

    // Chop into the middle of the transaction batch: the whole txn must be
    // discarded (atomicity), including the already-valid first record.
    let bytes = std::fs::read(&wal_path).unwrap();
    let mut a_end = 20;
    let mut buf = Vec::new();
    records[0].encode(&mut buf).unwrap();
    a_end += buf.len();
    let mut buf2 = Vec::new();
    records[1].encode(&mut buf2).unwrap();
    let begin_len = buf2.len();
    let cut = a_end + begin_len + 12; // inside the batch
    std::fs::write(&wal_path, &bytes[..cut]).unwrap();

    let db = Db::open(&path).unwrap();
    assert_eq!(db.get("a").unwrap(), Some(Value::Int(1)));
    assert_eq!(db.get("b").unwrap(), None);
    assert_eq!(db.get("c").unwrap(), None);
}

#[test]
fn recovery_is_idempotent() {
    let dir = TestDir::new("rec-idempotent");
    let records = [
        Record::set("a", &Value::Int(1), 1).unwrap(),
        Record::set("b", &Value::Int(2), 2).unwrap(),
    ];
    let path = db_with_wal(&dir, "t.sdb", &records);
    let wal_path = dir.path.join("t.sdb-wal");
    let bytes = std::fs::read(&wal_path).unwrap();
    std::fs::write(&wal_path, &bytes[..bytes.len() - 3]).unwrap();

    let db1 = Db::open(&path).unwrap();
    let s1 = db1.keys();
    drop(db1);
    // Reopening again must yield exactly the same state.
    let db2 = Db::open(&path).unwrap();
    let s2 = db2.keys();
    assert_eq!(s1, s2);
}

#[test]
fn rolled_back_txn_in_wal_is_discarded() {
    let dir = TestDir::new("rec-rollback");
    let records = [
        Record::txn_begin(1),
        Record::set("x", &Value::Int(9), 2).unwrap(),
        Record::txn_rollback(3),
        Record::set("y", &Value::Int(1), 4).unwrap(),
    ];
    let path = db_with_wal(&dir, "t.sdb", &records);
    let db = Db::open(&path).unwrap();
    assert_eq!(db.get("x").unwrap(), None);
    assert_eq!(db.get("y").unwrap(), Some(Value::Int(1)));
}

#[test]
fn unknown_format_version_is_a_clear_error() {
    let dir = TestDir::new("rec-version");
    let path = dir.db("t.sdb");
    {
        let mut db = Db::open(&path).unwrap();
        db.set("a", Value::Int(1)).unwrap();
        db.close().unwrap();
    }
    let mut bytes = std::fs::read(&path).unwrap();
    // Rewrite the header with version 2 (fixing the checksum).
    bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
    let crc = slimdb_core::crc32::crc32(&bytes[..28]);
    bytes[28..32].copy_from_slice(&crc.to_le_bytes());
    std::fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        Db::open(&path),
        Err(SlimDbError::UnsupportedVersion(2))
    ));
}
