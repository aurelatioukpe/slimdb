mod common;

use common::TestDir;
use slimdb_core::{Db, SlimDbError, Value};

#[test]
fn set_get_delete_exists() {
    let dir = TestDir::new("basic");
    let mut db = Db::open(dir.db("t.sdb")).unwrap();

    assert_eq!(db.get("missing").unwrap(), None);
    assert!(!db.exists("missing").unwrap());
    assert!(!db.has("missing").unwrap());
    assert!(!db.delete("missing").unwrap());

    db.set("a", Value::Int(1)).unwrap();
    assert_eq!(db.get("a").unwrap(), Some(Value::Int(1)));
    assert!(db.exists("a").unwrap());
    assert!(db.delete("a").unwrap());
    assert_eq!(db.get("a").unwrap(), None);
    assert!(!db.delete("a").unwrap());
}

#[test]
fn nested_paths() {
    let dir = TestDir::new("nested");
    let mut db = Db::open(dir.db("t.sdb")).unwrap();

    db.set("player.level", Value::Int(42)).unwrap();
    db.set("player.name", Value::Str("Aurel".to_string()))
        .unwrap();
    db.set("player.inventory.sword", Value::Int(5)).unwrap();
    db.set("player.inventory.shield", Value::Bool(true))
        .unwrap();

    assert_eq!(db.get("player.level").unwrap(), Some(Value::Int(42)));
    assert_eq!(
        db.get("player.inventory.sword").unwrap(),
        Some(Value::Int(5))
    );
    assert_eq!(
        db.get("player")
            .unwrap()
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        3
    );

    // Deleting a leaf leaves the parent.
    db.delete("player.inventory.sword").unwrap();
    assert_eq!(db.get("player.inventory.sword").unwrap(), None);
    assert_eq!(
        db.get("player.inventory.shield").unwrap(),
        Some(Value::Bool(true))
    );

    // Deleting a parent removes the subtree.
    db.delete("player.inventory").unwrap();
    assert_eq!(db.get("player.inventory").unwrap(), None);
    assert!(db.exists("player").unwrap());
}

#[test]
fn type_mismatch_is_an_error() {
    let dir = TestDir::new("typemismatch");
    let mut db = Db::open(dir.db("t.sdb")).unwrap();

    db.set("a", Value::Int(5)).unwrap();
    let err = db.set("a.b", Value::Null).unwrap_err();
    assert!(matches!(err, SlimDbError::TypeMismatch { .. }));
    assert!(db.get("a.b").unwrap().is_none());
    // The failed write must not have corrupted the stored value.
    assert_eq!(db.get("a").unwrap(), Some(Value::Int(5)));
}

#[test]
fn bad_paths_are_rejected() {
    let dir = TestDir::new("badpath");
    let mut db = Db::open(dir.db("t.sdb")).unwrap();
    assert!(db.get("").is_err());
    assert!(db.set("a..b", Value::Null).is_err());
    assert!(db.set(".a", Value::Null).is_err());
    assert!(db.delete("a.").is_err());
    assert!(db.exists("..").is_err());
}

#[test]
fn keys_are_sorted_top_level() {
    let dir = TestDir::new("keys");
    let mut db = Db::open(dir.db("t.sdb")).unwrap();
    db.set("zeta", Value::Null).unwrap();
    db.set("alpha", Value::Null).unwrap();
    db.set("mid.x", Value::Int(1)).unwrap();
    assert_eq!(db.keys(), vec!["alpha", "mid", "zeta"]);
}

#[test]
fn clear_removes_everything() {
    let dir = TestDir::new("clear");
    let mut db = Db::open(dir.db("t.sdb")).unwrap();
    db.set("a", Value::Int(1)).unwrap();
    db.set("b.c", Value::Int(2)).unwrap();
    db.clear().unwrap();
    assert!(db.keys().is_empty());
    db.clear().unwrap(); // idempotent
    assert!(db.keys().is_empty());
}

#[test]
fn update_read_modify_write() {
    let dir = TestDir::new("update");
    let mut db = Db::open(dir.db("t.sdb")).unwrap();

    db.update("counter", |v| {
        let n = v.and_then(|v| v.as_int()).unwrap_or(0);
        Some(Value::Int(n + 1))
    })
    .unwrap();
    db.update("counter", |v| {
        let n = v.and_then(|v| v.as_int()).unwrap_or(0);
        Some(Value::Int(n + 1))
    })
    .unwrap();
    assert_eq!(db.get("counter").unwrap(), Some(Value::Int(2)));

    // Returning None deletes.
    db.update("counter", |_| None).unwrap();
    assert_eq!(db.get("counter").unwrap(), None);
}

#[test]
fn all_value_types_roundtrip() {
    let dir = TestDir::new("types");
    let mut db = Db::open(dir.db("t.sdb")).unwrap();

    let doc = Value::Object(
        [
            ("null".to_string(), Value::Null),
            ("bool".to_string(), Value::Bool(false)),
            ("int".to_string(), Value::Int(-1234567890123)),
            ("float".to_string(), Value::Float(2.5)),
            ("nan".to_string(), Value::Float(f64::NAN)),
            ("str".to_string(), Value::Str("héllo wörld 🔥".to_string())),
            ("bin".to_string(), Value::Binary(vec![0, 1, 2, 254, 255])),
            (
                "arr".to_string(),
                Value::Array(vec![Value::Int(1), Value::Str("x".to_string())]),
            ),
        ]
        .into_iter()
        .collect(),
    );
    db.set("doc", doc.clone()).unwrap();
    assert_eq!(db.get("doc").unwrap(), Some(doc));

    // NaN survives as the canonical NaN.
    let nan = db.get("doc.nan").unwrap().unwrap();
    assert!(nan.as_f64().unwrap().is_nan());
}

#[test]
fn overwrite_values() {
    let dir = TestDir::new("overwrite");
    let mut db = Db::open(dir.db("t.sdb")).unwrap();
    db.set("x", Value::Int(1)).unwrap();
    db.set("x", Value::Str("now a string".to_string())).unwrap();
    assert_eq!(
        db.get("x").unwrap(),
        Some(Value::Str("now a string".to_string()))
    );
    // Traversing a non-object is a type error.
    assert!(db.set("x.y", Value::Int(2)).is_err());
    // Replacing a scalar with an object works at the top level.
    db.set("x", Value::Object(Default::default())).unwrap();
    db.set("x.y", Value::Int(2)).unwrap();
    assert_eq!(db.get("x.y").unwrap(), Some(Value::Int(2)));
}
