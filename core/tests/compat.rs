//! Backward-compatibility contract (spec §6, "test de compatibilité ascendante").
//!
//! Before every minor release, a `.sdb` written by the previous release must
//! open without error or loss. Until there are real previous releases, this
//! file pins the format contract with byte-for-byte fixtures: if the format
//! ever drifts, these tests fail loudly instead of breaking users silently.
//!
//! To add a fixture from an older release: check in the `.sdb` bytes (as a
//! `#[rustfmt::skip]` byte array or include_bytes!) and open it here.

mod common;

use common::TestDir;
use slimdb_core::crc32::crc32;
use slimdb_core::record::{DB_MAGIC, FORMAT_VERSION};
use slimdb_core::{Db, Value};

/// Hand-craft a minimal v1 snapshot: header + one SET record for
/// `a = 7`, byte by byte, exactly as the encoder would write it.
fn v1_snapshot_bytes(db_id: u64) -> Vec<u8> {
    let mut out = Vec::new();

    // --- Header (32 bytes) ---
    out.extend_from_slice(&DB_MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&db_id.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes()); // reserved
    let header_crc = crc32(&out);
    out.extend_from_slice(&header_crc.to_le_bytes());

    // --- One SET record: path "a", value Int(7) ---
    // Payload: u32 path_len(1) || "a" || value tag(0x02) || i64(7)
    let mut payload = Vec::new();
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload.push(b'a');
    payload.push(0x02); // TAG_INT
    payload.extend_from_slice(&7i64.to_le_bytes());

    let rec_start = out.len();
    out.extend_from_slice(b"SD"); // MAGIC
    out.push(1); // VERSION
    out.push(0); // TYPE_SET
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&1u64.to_le_bytes()); // SEQUENCE
    out.extend_from_slice(&payload);
    let rec_crc = crc32(&out[rec_start..]);
    out.extend_from_slice(&rec_crc.to_le_bytes());

    out
}

#[test]
fn v1_fixture_opens_with_correct_content() {
    let dir = TestDir::new("compat-v1");
    let path = dir.db("fixture.sdb");
    std::fs::write(&path, v1_snapshot_bytes(0xCAFE_1234)).unwrap();

    let db = Db::open(&path).unwrap();
    assert_eq!(db.get("a").unwrap(), Some(Value::Int(7)));
    // And it is writable.
    let mut db = db;
    db.set("b", Value::Bool(true)).unwrap();
    assert_eq!(db.get("b").unwrap(), Some(Value::Bool(true)));
}

#[test]
fn v1_snapshot_plus_wal_merges() {
    let dir = TestDir::new("compat-wal");
    let path = dir.db("fixture.sdb");
    std::fs::write(&path, v1_snapshot_bytes(0xABCD_EF01)).unwrap();

    // Craft a WAL with the matching db_id containing one more SET record.
    let wal_path = dir.path.join("fixture.sdb-wal");
    let mut wal = Vec::new();
    wal.extend_from_slice(b"SWAL");
    wal.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    wal.extend_from_slice(&0xABCD_EF01u64.to_le_bytes());
    let hdr_crc = crc32(&wal);
    wal.extend_from_slice(&hdr_crc.to_le_bytes());

    let mut payload = Vec::new();
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload.push(b'c');
    payload.push(0x02); // TAG_INT
    payload.extend_from_slice(&9i64.to_le_bytes());
    let rec_start = wal.len();
    wal.extend_from_slice(b"SD");
    wal.push(1);
    wal.push(0);
    wal.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    wal.extend_from_slice(&2u64.to_le_bytes());
    wal.extend_from_slice(&payload);
    let rec_crc = crc32(&wal[rec_start..]);
    wal.extend_from_slice(&rec_crc.to_le_bytes());
    std::fs::write(&wal_path, &wal).unwrap();

    let db = Db::open(&path).unwrap();
    assert_eq!(db.get("a").unwrap(), Some(Value::Int(7)));
    assert_eq!(db.get("c").unwrap(), Some(Value::Int(9)));
}

/// The automated release-to-release check plugs in here: before tagging a
/// minor release, generate a `.sdb` with the previous release's CLI
/// (`slimdb set` a few keys, close), then open it with the new build and
/// assert the same values. See docs/FORMAT.md §"Versioning policy".
#[test]
fn roundtrip_is_stable_across_sessions() {
    let dir = TestDir::new("compat-stable");
    let path = dir.db("stable.sdb");
    {
        let mut db = Db::open(&path).unwrap();
        db.set("player.level", Value::Int(42)).unwrap();
        db.set("player.inventory.sword", Value::Int(5)).unwrap();
        db.set("flag", Value::Bool(true)).unwrap();
        db.close().unwrap();
    }
    // A fresh process open must produce byte-identical state.
    let db1 = Db::open(&path).unwrap();
    let keys1 = db1.keys();
    let v1 = keys1
        .iter()
        .map(|k| (k.clone(), db1.get(k).unwrap().unwrap()))
        .collect::<Vec<_>>();
    drop(db1);
    let db2 = Db::open(&path).unwrap();
    let keys2 = db2.keys();
    let v2 = keys2
        .iter()
        .map(|k| (k.clone(), db2.get(k).unwrap().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(v1, v2);
}
