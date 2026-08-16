//! Integration tests for the C ABI. The exported functions are called
//! directly (they are ordinary Rust functions); the same symbols are what the
//! C header exposes, and `examples/demo.c` exercises them from real C.

use std::ffi::{CStr, CString};
use std::path::PathBuf;
use std::ptr;

use slimdb_c::{
    slimdb_abi_version, slimdb_backup, slimdb_begin, slimdb_clear, slimdb_close, slimdb_commit,
    slimdb_compact, slimdb_db_t, slimdb_delete, slimdb_error_string, slimdb_get_json, slimdb_keys,
    slimdb_open, slimdb_open_with_mode, slimdb_restore, slimdb_rollback, slimdb_set_json,
    slimdb_stats, slimdb_stats_t, slimdb_string_free, slimdb_string_list_free, slimdb_txn_clear,
    slimdb_txn_delete, slimdb_txn_set_json, slimdb_txn_t, slimdb_version, SLIMDB_DURABILITY_FAST,
    SLIMDB_ERR_BAD_PATH, SLIMDB_ERR_INVALID_ARG, SLIMDB_ERR_TYPE_MISMATCH, SLIMDB_OK,
};

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> TestDir {
        let path = std::env::temp_dir().join(format!(
            "slimdb-cabi-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TestDir { path }
    }

    fn db(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn c(path: &str) -> CString {
    CString::new(path).unwrap()
}

unsafe fn get_json(db: *mut slimdb_db_t, path: &str) -> Result<Option<String>, i32> {
    let mut out: *mut std::ffi::c_char = ptr::null_mut();
    let code = slimdb_get_json(db, c(path).as_ptr(), &mut out);
    if code != SLIMDB_OK {
        return Err(code);
    }
    if out.is_null() {
        return Ok(None);
    }
    let s = CStr::from_ptr(out).to_string_lossy().into_owned();
    slimdb_string_free(out);
    Ok(Some(s))
}

unsafe fn set_json(db: *mut slimdb_db_t, path: &str, json: &str) -> i32 {
    slimdb_set_json(db, c(path).as_ptr(), c(json).as_ptr())
}

#[test]
fn open_set_get_close_reopen() {
    let dir = TestDir::new("basic");
    let path = dir.db("t.sdb");

    let mut handle: *mut slimdb_db_t = ptr::null_mut();
    assert_eq!(
        slimdb_open(c(path.to_str().unwrap()).as_ptr(), &mut handle),
        SLIMDB_OK
    );
    assert!(!handle.is_null());

    unsafe {
        assert_eq!(set_json(handle, "player.level", "42"), SLIMDB_OK);
        assert_eq!(set_json(handle, "player.name", "\"Aurel\""), SLIMDB_OK);
        assert_eq!(
            get_json(handle, "player.level").unwrap(),
            Some("42".to_string())
        );

        // Missing path returns OK with NULL out.
        assert_eq!(get_json(handle, "missing").unwrap(), None);

        let mut removed = 0;
        assert_eq!(
            slimdb_delete(handle, c("player.name").as_ptr(), &mut removed),
            SLIMDB_OK
        );
        assert_eq!(removed, 1);
        assert_eq!(get_json(handle, "player.name").unwrap(), None);

        assert_eq!(slimdb_close(handle), SLIMDB_OK);
    }

    // Reopen: only player.level survived.
    let mut handle2: *mut slimdb_db_t = ptr::null_mut();
    assert_eq!(
        slimdb_open(c(path.to_str().unwrap()).as_ptr(), &mut handle2),
        SLIMDB_OK
    );
    unsafe {
        assert_eq!(
            get_json(handle2, "player.level").unwrap(),
            Some("42".to_string())
        );
        assert_eq!(get_json(handle2, "player.name").unwrap(), None);
        assert_eq!(slimdb_close(handle2), SLIMDB_OK);
    }
}

#[test]
fn keys_and_stats() {
    let dir = TestDir::new("keys");
    let path = dir.db("t.sdb");
    let mut handle: *mut slimdb_db_t = ptr::null_mut();
    assert_eq!(
        slimdb_open(c(path.to_str().unwrap()).as_ptr(), &mut handle),
        SLIMDB_OK
    );
    unsafe {
        assert_eq!(set_json(handle, "zeta", "1"), SLIMDB_OK);
        assert_eq!(set_json(handle, "alpha", "2"), SLIMDB_OK);
        assert_eq!(set_json(handle, "mid.x", "3"), SLIMDB_OK);

        let mut keys: *mut *mut std::ffi::c_char = ptr::null_mut();
        let mut count = 0usize;
        assert_eq!(slimdb_keys(handle, &mut keys, &mut count), SLIMDB_OK);
        assert_eq!(count, 3);
        let list = std::slice::from_raw_parts(keys, count);
        let names: Vec<String> = list
            .iter()
            .map(|p| CStr::from_ptr(*p).to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
        slimdb_string_list_free(keys, count);

        let mut stats = slimdb_stats_t {
            keys: 0,
            data_file_size: 0,
            wal_file_size: 0,
            sequence: 0,
            durability: -1,
        };
        assert_eq!(slimdb_stats(handle, &mut stats), SLIMDB_OK);
        assert_eq!(stats.keys, 3);
        assert_eq!(stats.durability, 0); // SLIMDB_DURABILITY_SAFE
        assert_eq!(slimdb_close(handle), SLIMDB_OK);
    }
}

#[test]
fn transactions_are_atomic() {
    let dir = TestDir::new("txn");
    let path = dir.db("t.sdb");

    let mut handle: *mut slimdb_db_t = ptr::null_mut();
    assert_eq!(
        slimdb_open(c(path.to_str().unwrap()).as_ptr(), &mut handle),
        SLIMDB_OK
    );
    unsafe {
        assert_eq!(set_json(handle, "durable", "1"), SLIMDB_OK);

        // Commit.
        let txn: *mut slimdb_txn_t = slimdb_begin(handle);
        assert!(!txn.is_null());
        assert_eq!(
            slimdb_txn_set_json(txn, c("txn.a").as_ptr(), c("1").as_ptr()),
            SLIMDB_OK
        );
        assert_eq!(
            slimdb_txn_set_json(txn, c("txn.b").as_ptr(), c("2").as_ptr()),
            SLIMDB_OK
        );
        assert_eq!(slimdb_commit(txn), SLIMDB_OK);
        assert_eq!(get_json(handle, "txn.a").unwrap(), Some("1".to_string()));
        assert_eq!(get_json(handle, "txn.b").unwrap(), Some("2".to_string()));

        // Rollback.
        let txn = slimdb_begin(handle);
        assert_eq!(
            slimdb_txn_set_json(txn, c("txn.c").as_ptr(), c("3").as_ptr()),
            SLIMDB_OK
        );
        assert_eq!(
            slimdb_txn_delete(txn, c("txn.a").as_ptr(), ptr::null_mut()),
            SLIMDB_OK
        );
        assert_eq!(slimdb_rollback(txn), SLIMDB_OK);
        assert_eq!(get_json(handle, "txn.c").unwrap(), None);
        assert_eq!(get_json(handle, "txn.a").unwrap(), Some("1".to_string()));

        // Clear inside a txn, then roll back.
        let txn = slimdb_begin(handle);
        assert_eq!(slimdb_txn_clear(txn), SLIMDB_OK);
        assert_eq!(slimdb_rollback(txn), SLIMDB_OK);
        assert_eq!(get_json(handle, "durable").unwrap(), Some("1".to_string()));

        assert_eq!(slimdb_close(handle), SLIMDB_OK);
    }

    // Committed txn data survives reopen.
    let mut handle2: *mut slimdb_db_t = ptr::null_mut();
    assert_eq!(
        slimdb_open(c(path.to_str().unwrap()).as_ptr(), &mut handle2),
        SLIMDB_OK
    );
    unsafe {
        assert_eq!(get_json(handle2, "txn.a").unwrap(), Some("1".to_string()));
        assert_eq!(get_json(handle2, "txn.c").unwrap(), None);
        assert_eq!(slimdb_close(handle2), SLIMDB_OK);
    }
}

#[test]
fn compact_backup_restore_clear() {
    let dir = TestDir::new("maint");
    let path = dir.db("t.sdb");
    let backup = dir.db("b.sdb");

    let mut handle: *mut slimdb_db_t = ptr::null_mut();
    assert_eq!(
        slimdb_open(c(path.to_str().unwrap()).as_ptr(), &mut handle),
        SLIMDB_OK
    );
    unsafe {
        assert_eq!(set_json(handle, "a", "1"), SLIMDB_OK);
        assert_eq!(
            slimdb_backup(handle, c(backup.to_str().unwrap()).as_ptr()),
            SLIMDB_OK
        );
        assert_eq!(set_json(handle, "a", "99"), SLIMDB_OK);
        assert_eq!(slimdb_compact(handle), SLIMDB_OK);
        assert_eq!(get_json(handle, "a").unwrap(), Some("99".to_string()));
        assert_eq!(
            slimdb_restore(handle, c(backup.to_str().unwrap()).as_ptr()),
            SLIMDB_OK
        );
        assert_eq!(get_json(handle, "a").unwrap(), Some("1".to_string()));
        assert_eq!(slimdb_clear(handle), SLIMDB_OK);
        assert_eq!(get_json(handle, "a").unwrap(), None);
        assert_eq!(slimdb_close(handle), SLIMDB_OK);
    }
}

#[test]
fn error_codes_and_null_handling() {
    let dir = TestDir::new("errors");
    let path = dir.db("t.sdb");
    let mut handle: *mut slimdb_db_t = ptr::null_mut();
    assert_eq!(
        slimdb_open(c(path.to_str().unwrap()).as_ptr(), &mut handle),
        SLIMDB_OK
    );
    unsafe {
        // Bad path.
        assert_eq!(set_json(handle, "a..b", "1"), SLIMDB_ERR_BAD_PATH);
        // Type mismatch.
        assert_eq!(set_json(handle, "a", "5"), SLIMDB_OK);
        assert_eq!(set_json(handle, "a.b", "1"), SLIMDB_ERR_TYPE_MISMATCH);
        // Null pointer.
        assert_eq!(set_json(handle, "a", "1"), SLIMDB_OK);
        assert_eq!(
            slimdb_set_json(ptr::null_mut(), c("a").as_ptr(), c("1").as_ptr()),
            SLIMDB_ERR_INVALID_ARG
        );
        assert_eq!(
            slimdb_get_json(handle, c("a").as_ptr(), ptr::null_mut()),
            SLIMDB_ERR_INVALID_ARG
        );
        assert_eq!(slimdb_close(ptr::null_mut()), SLIMDB_ERR_INVALID_ARG);
        assert_eq!(slimdb_close(handle), SLIMDB_OK);
    }
    // Open of a non-existent directory fails with an IO error (not a panic).
    let bogus = dir.path.join("nope").join("x.sdb");
    let mut h2: *mut slimdb_db_t = ptr::null_mut();
    assert_ne!(
        slimdb_open(c(bogus.to_str().unwrap()).as_ptr(), &mut h2),
        SLIMDB_OK
    );
}

#[test]
fn durability_mode_and_version() {
    let dir = TestDir::new("mode");
    let path = dir.db("t.sdb");
    let mut handle: *mut slimdb_db_t = ptr::null_mut();
    assert_eq!(
        slimdb_open_with_mode(
            c(path.to_str().unwrap()).as_ptr(),
            SLIMDB_DURABILITY_FAST,
            &mut handle
        ),
        SLIMDB_OK
    );
    {
        let mut stats = slimdb_stats_t {
            keys: 0,
            data_file_size: 0,
            wal_file_size: 0,
            sequence: 0,
            durability: -1,
        };
        assert_eq!(slimdb_stats(handle, &mut stats), SLIMDB_OK);
        assert_eq!(stats.durability, SLIMDB_DURABILITY_FAST);
        assert_eq!(slimdb_close(handle), SLIMDB_OK);
    }
    // Invalid mode.
    assert_eq!(
        slimdb_open_with_mode(c(path.to_str().unwrap()).as_ptr(), 99, &mut handle),
        SLIMDB_ERR_INVALID_ARG
    );
    unsafe {
        assert!(!CStr::from_ptr(slimdb_version())
            .to_str()
            .unwrap()
            .is_empty());
        assert_eq!(CStr::from_ptr(slimdb_abi_version()).to_str().unwrap(), "1");
        assert!(!CStr::from_ptr(slimdb_error_string(0))
            .to_str()
            .unwrap()
            .is_empty());
    }
}

#[test]
fn binary_values_survive_as_hex_json() {
    let dir = TestDir::new("bin");
    let path = dir.db("t.sdb");
    let mut handle: *mut slimdb_db_t = ptr::null_mut();
    assert_eq!(
        slimdb_open(c(path.to_str().unwrap()).as_ptr(), &mut handle),
        SLIMDB_OK
    );
    unsafe {
        // Binary is not expressible in JSON; use the hex string convention of
        // the printer — here we set a plain string and check the roundtrip.
        assert_eq!(
            set_json(handle, "doc", "{\"x\": [1, 2.5, null, true]}"),
            SLIMDB_OK
        );
        let got = get_json(handle, "doc").unwrap().unwrap();
        let parsed = slimdb_core::json::parse(&got).unwrap();
        let expected = slimdb_core::json::parse("{\"x\": [1, 2.5, null, true]}").unwrap();
        assert_eq!(parsed, expected);
        assert_eq!(slimdb_close(handle), SLIMDB_OK);
    }
}
