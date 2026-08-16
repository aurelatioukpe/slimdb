mod common;

use common::TestDir;
use slimdb_core::{Db, Value};

#[test]
fn commit_persists_all_writes() {
    let dir = TestDir::new("txn-commit");
    let path = dir.db("t.sdb");
    {
        let mut db = Db::open(&path).unwrap();
        let mut txn = db.transaction();
        txn.set("a", Value::Int(1)).unwrap();
        txn.set("b.c", Value::Int(2)).unwrap();
        txn.delete("a").unwrap();
        txn.commit().unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(db.get("a").unwrap(), None);
    assert_eq!(db.get("b.c").unwrap(), Some(Value::Int(2)));
}

#[test]
fn rollback_restores_previous_state() {
    let dir = TestDir::new("txn-rollback");
    let mut db = Db::open(dir.db("t.sdb")).unwrap();
    db.set("keep", Value::Int(1)).unwrap();

    let mut txn = db.transaction();
    txn.set("keep", Value::Int(99)).unwrap();
    txn.set("new", Value::Str("x".to_string())).unwrap();
    txn.rollback();

    assert_eq!(db.get("keep").unwrap(), Some(Value::Int(1)));
    assert_eq!(db.get("new").unwrap(), None);
}

#[test]
fn dropping_a_transaction_rolls_back() {
    let dir = TestDir::new("txn-drop");
    let mut db = Db::open(dir.db("t.sdb")).unwrap();
    db.set("a", Value::Int(1)).unwrap();

    {
        let mut txn = db.transaction();
        txn.set("a", Value::Int(2)).unwrap();
        txn.set("b", Value::Int(3)).unwrap();
        // Dropped without commit.
    }
    assert_eq!(db.get("a").unwrap(), Some(Value::Int(1)));
    assert_eq!(db.get("b").unwrap(), None);
}

#[test]
fn rollback_of_nested_and_overlapping_writes() {
    let dir = TestDir::new("txn-nested");
    let mut db = Db::open(dir.db("t.sdb")).unwrap();
    db.set("x.y", Value::Int(1)).unwrap();

    let mut txn = db.transaction();
    txn.set("x.y", Value::Int(2)).unwrap();
    txn.set("x", Value::Int(5)).unwrap(); // replaces the object
    txn.set("z", Value::Null).unwrap();
    txn.rollback();

    assert_eq!(db.get("x.y").unwrap(), Some(Value::Int(1)));
    assert_eq!(db.get("z").unwrap(), None);
}

#[test]
fn txn_clear_rolls_back() {
    let dir = TestDir::new("txn-clear");
    let mut db = Db::open(dir.db("t.sdb")).unwrap();
    db.set("a", Value::Int(1)).unwrap();
    db.set("b", Value::Int(2)).unwrap();

    let mut txn = db.transaction();
    txn.clear().unwrap();
    txn.rollback();

    assert_eq!(db.get("a").unwrap(), Some(Value::Int(1)));
    assert_eq!(db.get("b").unwrap(), Some(Value::Int(2)));
}

#[test]
fn empty_transaction_commits_as_noop() {
    let dir = TestDir::new("txn-empty");
    let mut db = Db::open(dir.db("t.sdb")).unwrap();
    let mut txn = db.transaction();
    txn.set("a", Value::Int(1)).unwrap();
    txn.delete("a").unwrap();
    txn.commit().unwrap();
    assert_eq!(db.get("a").unwrap(), None);
}

#[test]
fn txn_reads_see_own_writes() {
    let dir = TestDir::new("txn-read");
    let mut db = Db::open(dir.db("t.sdb")).unwrap();
    let mut txn = db.transaction();
    txn.set("a", Value::Int(1)).unwrap();
    assert_eq!(txn.get("a").unwrap(), Some(Value::Int(1)));
    txn.commit().unwrap();
}

#[test]
fn uncommitted_txn_does_not_survive_reopen() {
    let dir = TestDir::new("txn-crash");
    let path = dir.db("t.sdb");
    {
        let mut db = Db::open(&path).unwrap();
        db.set("durable", Value::Int(1)).unwrap();
        let mut txn = db.transaction();
        txn.set("maybe", Value::Int(2)).unwrap();
        // Simulate a crash: drop without commit and without close.
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(db.get("durable").unwrap(), Some(Value::Int(1)));
    assert_eq!(db.get("maybe").unwrap(), None);
}
