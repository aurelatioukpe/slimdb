//! The SlimDB C ABI — the fundamental contract between the core and every
//! binding (spec §5, §14).
//!
//! All bindings (Python, Node, Godot, …) sit on the `slimdb_*` functions in
//! this crate, never on the core's internal structures.
//!
//! # Conventions
//!
//! * Opaque handles: `slimdb_db_t*` and `slimdb_txn_t*` are returned by the
//!   library and passed back to it. The caller owns their lifetime: close the
//!   database with `slimdb_close`, finish a transaction with
//!   `slimdb_commit`/`slimdb_rollback`. Using a handle after close is
//!   undefined behavior, exactly like `fclose`d `FILE*`s.
//! * Threading: a single handle must not be used concurrently (matching the
//!   core's documented single-process, single-writer domain).
//! * Values cross the ABI as JSON strings (JSON is the API model, never the
//!   storage format). Strings returned by this library must be freed with
//!   `slimdb_string_free` / `slimdb_string_list_free`.
//! * No panic ever crosses the ABI: every export is guarded by `catch_unwind`
//!   and reports `SLIMDB_ERR_PANIC` instead.
//!
//! See `include/slimdb.h` for the C-side contract.
//
// The exported functions are deliberately *not* `unsafe fn`: they are the C
// ABI, whose safety contract is documented in `include/slimdb.h` (handle
// lifetime, single-writer, string ownership). Marking each one `unsafe`
// would only add noise for C callers who cannot see Rust's `unsafe` anyway.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use slimdb_core::{Db, DurabilityMode, SlimDbError, Transaction};

// ---------------------------------------------------------------------------
// Opaque handle types
// ---------------------------------------------------------------------------

/// Opaque database handle (the actual object is a `Box<Db>`).
#[repr(C)]
pub struct slimdb_db_t {
    _private: [u8; 0],
}

/// Opaque transaction handle (the actual object is a `Box<Transaction<'static>>`).
#[repr(C)]
pub struct slimdb_txn_t {
    _private: [u8; 0],
}

// ---------------------------------------------------------------------------
// Error codes and durability modes (must match include/slimdb.h)
// ---------------------------------------------------------------------------

pub const SLIMDB_OK: i32 = 0;
pub const SLIMDB_ERR_IO: i32 = 1;
pub const SLIMDB_ERR_CORRUPT: i32 = 2;
pub const SLIMDB_ERR_DECODE: i32 = 3;
pub const SLIMDB_ERR_UNSUPPORTED_VERSION: i32 = 4;
pub const SLIMDB_ERR_BAD_PATH: i32 = 5;
pub const SLIMDB_ERR_TYPE_MISMATCH: i32 = 6;
pub const SLIMDB_ERR_INVALID_ARG: i32 = 7;
pub const SLIMDB_ERR_TXN: i32 = 8;
pub const SLIMDB_ERR_PANIC: i32 = 9;

pub const SLIMDB_DURABILITY_SAFE: i32 = 0;
pub const SLIMDB_DURABILITY_BALANCED: i32 = 1;
pub const SLIMDB_DURABILITY_FAST: i32 = 2;

fn err_code(e: &SlimDbError) -> i32 {
    match e {
        SlimDbError::Io(_) => SLIMDB_ERR_IO,
        SlimDbError::Corrupt(_) => SLIMDB_ERR_CORRUPT,
        SlimDbError::Decode(_) => SLIMDB_ERR_DECODE,
        SlimDbError::UnsupportedVersion(_) => SLIMDB_ERR_UNSUPPORTED_VERSION,
        SlimDbError::BadPath(_) => SLIMDB_ERR_BAD_PATH,
        SlimDbError::TypeMismatch { .. } => SLIMDB_ERR_TYPE_MISMATCH,
        SlimDbError::InvalidArgument(_) => SLIMDB_ERR_INVALID_ARG,
        SlimDbError::Transaction(_) => SLIMDB_ERR_TXN,
    }
}

fn durability(mode: i32) -> Result<DurabilityMode, i32> {
    match mode {
        SLIMDB_DURABILITY_SAFE => Ok(DurabilityMode::Safe),
        SLIMDB_DURABILITY_BALANCED => Ok(DurabilityMode::Balanced),
        SLIMDB_DURABILITY_FAST => Ok(DurabilityMode::Fast),
        _ => Err(SLIMDB_ERR_INVALID_ARG),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Run the closure, converting any panic into `SLIMDB_ERR_PANIC`.
/// No Rust panic is ever allowed to cross the C ABI.
fn ffi_guard<F: FnOnce() -> i32>(f: F) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(code) => code,
        Err(_) => SLIMDB_ERR_PANIC,
    }
}

/// Early-return helper for `ffi_guard` closures (which return `i32`):
/// unwraps a `Result<_, i32>`, returning the error code on failure.
macro_rules! ffi_try {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(code) => return code,
        }
    };
}

/// Recover `&mut Db` from an opaque handle.
///
/// # Safety
///
/// The only producer of these pointers is `slimdb_open`, which converts a
/// `Box<Db>` into a raw pointer; the C contract requires callers not to use a
/// handle after `slimdb_close`. Under that contract the pointer is always a
/// valid, uniquely-borrowed `Db` for the duration of the call.
unsafe fn db_ref<'a>(db: *mut slimdb_db_t) -> Result<&'a mut Db, i32> {
    if db.is_null() {
        return Err(SLIMDB_ERR_INVALID_ARG);
    }
    Ok(&mut *(db as *mut Db))
}

/// Recover `&mut Transaction<'static>` from an opaque txn handle.
///
/// # Safety
///
/// Same producer contract as `db_ref` (`slimdb_begin`), with the additional
/// caller obligation that the database handle outlives the transaction and
/// that no other operation touches the database while the transaction is
/// open — the C API is not concurrent, matching the core's domain.
unsafe fn txn_ref<'a>(txn: *mut slimdb_txn_t) -> Result<&'a mut Transaction<'static>, i32> {
    if txn.is_null() {
        return Err(SLIMDB_ERR_INVALID_ARG);
    }
    Ok(&mut *(txn as *mut Transaction<'static>))
}

/// Read a NUL-terminated C string as `&str`.
///
/// # Safety
///
/// `p` must be a valid NUL-terminated string for the duration of the call.
unsafe fn cstr<'a>(p: *const c_char) -> Result<&'a str, i32> {
    if p.is_null() {
        return Err(SLIMDB_ERR_INVALID_ARG);
    }
    CStr::from_ptr(p).to_str().map_err(|_| SLIMDB_ERR_DECODE)
}

/// Check that an out-pointer is non-null.
fn out_ptr<T>(p: *mut T) -> Result<*mut T, i32> {
    if p.is_null() {
        Err(SLIMDB_ERR_INVALID_ARG)
    } else {
        Ok(p)
    }
}

/// Write a bool to an optional out-parameter (NULL is allowed for
/// informational flags such as `removed`).
fn write_bool(p: *mut i32, v: bool) -> Result<(), i32> {
    if !p.is_null() {
        unsafe {
            *p = v as i32;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Open a database with safe durability (default).
#[no_mangle]
pub extern "C" fn slimdb_open(path: *const c_char, out: *mut *mut slimdb_db_t) -> i32 {
    ffi_guard(|| {
        let out = ffi_try!(out_ptr(out));
        let path = ffi_try!(unsafe { cstr(path) });
        match Db::open(path) {
            Ok(db) => {
                let handle = Box::into_raw(Box::new(db)) as *mut slimdb_db_t;
                unsafe {
                    *out = handle;
                }
                SLIMDB_OK
            }
            Err(e) => err_code(&e),
        }
    })
}

/// Open a database with an explicit durability mode.
#[no_mangle]
pub extern "C" fn slimdb_open_with_mode(
    path: *const c_char,
    mode: i32,
    out: *mut *mut slimdb_db_t,
) -> i32 {
    ffi_guard(|| {
        let mode = ffi_try!(durability(mode));
        let out = ffi_try!(out_ptr(out));
        let path = ffi_try!(unsafe { cstr(path) });
        match Db::open_with_mode(path, mode) {
            Ok(db) => {
                let handle = Box::into_raw(Box::new(db)) as *mut slimdb_db_t;
                unsafe {
                    *out = handle;
                }
                SLIMDB_OK
            }
            Err(e) => err_code(&e),
        }
    })
}

/// Close a database (compacts it first) and free the handle.
#[no_mangle]
pub extern "C" fn slimdb_close(db: *mut slimdb_db_t) -> i32 {
    ffi_guard(|| {
        if db.is_null() {
            return SLIMDB_ERR_INVALID_ARG;
        }
        // SAFETY: see `db_ref`; close consumes the only owner of the Box.
        let db = unsafe { Box::from_raw(db as *mut Db) };
        match db.close() {
            Ok(()) => SLIMDB_OK,
            Err(e) => err_code(&e),
        }
    })
}

// ---------------------------------------------------------------------------
// Data access (JSON interchange)
// ---------------------------------------------------------------------------

/// Set the value at `path` from a JSON string.
#[no_mangle]
pub extern "C" fn slimdb_set_json(
    db: *mut slimdb_db_t,
    path: *const c_char,
    json: *const c_char,
) -> i32 {
    ffi_guard(|| {
        let db = ffi_try!(unsafe { db_ref(db) });
        let path = ffi_try!(unsafe { cstr(path) });
        let json = ffi_try!(unsafe { cstr(json) });
        let value = match slimdb_core::json::parse(json) {
            Ok(v) => v,
            Err(_) => return SLIMDB_ERR_DECODE,
        };
        match db.set(path, value) {
            Ok(()) => SLIMDB_OK,
            Err(e) => err_code(&e),
        }
    })
}

/// Get the value at `path` as a JSON string.
///
/// On success `out` receives either a string to free with `slimdb_string_free`
/// (value exists) or `NULL` (path missing).
#[no_mangle]
pub extern "C" fn slimdb_get_json(
    db: *mut slimdb_db_t,
    path: *const c_char,
    out: *mut *mut c_char,
) -> i32 {
    ffi_guard(|| {
        let db = ffi_try!(unsafe { db_ref(db) });
        let path = ffi_try!(unsafe { cstr(path) });
        let out = ffi_try!(out_ptr(out));
        match db.get(path) {
            Ok(Some(v)) => {
                let s = slimdb_core::json::stringify(&v);
                match CString::new(s) {
                    Ok(cs) => {
                        unsafe {
                            *out = cs.into_raw();
                        }
                        SLIMDB_OK
                    }
                    Err(_) => SLIMDB_ERR_DECODE,
                }
            }
            Ok(None) => {
                unsafe {
                    *out = ptr::null_mut();
                }
                SLIMDB_OK
            }
            Err(e) => err_code(&e),
        }
    })
}

/// Delete the value at `path`; `removed` receives 1 or 0.
#[no_mangle]
pub extern "C" fn slimdb_delete(
    db: *mut slimdb_db_t,
    path: *const c_char,
    removed: *mut i32,
) -> i32 {
    ffi_guard(|| {
        let db = ffi_try!(unsafe { db_ref(db) });
        let path = ffi_try!(unsafe { cstr(path) });
        match db.delete(path) {
            Ok(v) => {
                ffi_try!(write_bool(removed, v));
                SLIMDB_OK
            }
            Err(e) => err_code(&e),
        }
    })
}

/// Whether a value exists at `path`; `out` receives 1 or 0.
#[no_mangle]
pub extern "C" fn slimdb_exists(db: *mut slimdb_db_t, path: *const c_char, out: *mut i32) -> i32 {
    ffi_guard(|| {
        let db = ffi_try!(unsafe { db_ref(db) });
        let path = ffi_try!(unsafe { cstr(path) });
        match db.exists(path) {
            Ok(v) => {
                ffi_try!(write_bool(out, v));
                SLIMDB_OK
            }
            Err(e) => err_code(&e),
        }
    })
}

/// List top-level keys into a freshly allocated array of strings.
///
/// Free with `slimdb_string_list_free`.
#[no_mangle]
pub extern "C" fn slimdb_keys(
    db: *mut slimdb_db_t,
    out: *mut *mut *mut c_char,
    count: *mut usize,
) -> i32 {
    ffi_guard(|| {
        let db = ffi_try!(unsafe { db_ref(db) });
        let out = ffi_try!(out_ptr(out));
        let count = ffi_try!(out_ptr(count));
        let keys = db.keys();
        let mut list: Vec<*mut c_char> = Vec::with_capacity(keys.len());
        for k in keys {
            // Keys are non-empty and never contain NUL (split_path rejects
            // empty segments; NUL is a control character but is legal in a
            // key — this is the one place we must handle it).
            list.push(
                CString::new(k)
                    .unwrap_or_else(|_| CString::new("<nul>").unwrap())
                    .into_raw(),
            );
        }
        unsafe {
            *out = list.as_mut_ptr();
            *count = list.len();
            std::mem::forget(list);
        }
        SLIMDB_OK
    })
}

/// Remove every key.
#[no_mangle]
pub extern "C" fn slimdb_clear(db: *mut slimdb_db_t) -> i32 {
    ffi_guard(|| {
        let db = ffi_try!(unsafe { db_ref(db) });
        match db.clear() {
            Ok(()) => SLIMDB_OK,
            Err(e) => err_code(&e),
        }
    })
}

// ---------------------------------------------------------------------------
// Durability, maintenance
// ---------------------------------------------------------------------------

/// Flush buffered WAL bytes to disk and fsync.
#[no_mangle]
pub extern "C" fn slimdb_flush(db: *mut slimdb_db_t) -> i32 {
    ffi_guard(|| {
        let db = ffi_try!(unsafe { db_ref(db) });
        match db.flush() {
            Ok(()) => SLIMDB_OK,
            Err(e) => err_code(&e),
        }
    })
}

/// Compact the WAL into the snapshot file (atomic).
#[no_mangle]
pub extern "C" fn slimdb_compact(db: *mut slimdb_db_t) -> i32 {
    ffi_guard(|| {
        let db = ffi_try!(unsafe { db_ref(db) });
        match db.compact() {
            Ok(()) => SLIMDB_OK,
            Err(e) => err_code(&e),
        }
    })
}

/// Write a standalone snapshot to `dest`.
#[no_mangle]
pub extern "C" fn slimdb_backup(db: *mut slimdb_db_t, dest: *const c_char) -> i32 {
    ffi_guard(|| {
        let db = ffi_try!(unsafe { db_ref(db) });
        let dest = ffi_try!(unsafe { cstr(dest) });
        match db.backup(dest) {
            Ok(()) => SLIMDB_OK,
            Err(e) => err_code(&e),
        }
    })
}

/// Replace the database contents with the backup at `src`.
#[no_mangle]
pub extern "C" fn slimdb_restore(db: *mut slimdb_db_t, src: *const c_char) -> i32 {
    ffi_guard(|| {
        let db = ffi_try!(unsafe { db_ref(db) });
        let src = ffi_try!(unsafe { cstr(src) });
        match db.restore(src) {
            Ok(()) => SLIMDB_OK,
            Err(e) => err_code(&e),
        }
    })
}

/// Database statistics.
#[repr(C)]
pub struct slimdb_stats_t {
    pub keys: usize,
    pub data_file_size: u64,
    pub wal_file_size: u64,
    pub sequence: u64,
    /// One of the `SLIMDB_DURABILITY_*` constants.
    pub durability: i32,
}

#[no_mangle]
pub extern "C" fn slimdb_stats(db: *mut slimdb_db_t, out: *mut slimdb_stats_t) -> i32 {
    ffi_guard(|| {
        let db = ffi_try!(unsafe { db_ref(db) });
        let out = ffi_try!(out_ptr(out));
        match db.stats() {
            Ok(s) => {
                unsafe {
                    *out = slimdb_stats_t {
                        keys: s.keys,
                        data_file_size: s.data_file_size,
                        wal_file_size: s.wal_file_size,
                        sequence: s.sequence,
                        durability: match s.durability {
                            DurabilityMode::Safe => SLIMDB_DURABILITY_SAFE,
                            DurabilityMode::Balanced => SLIMDB_DURABILITY_BALANCED,
                            DurabilityMode::Fast => SLIMDB_DURABILITY_FAST,
                        },
                    };
                }
                SLIMDB_OK
            }
            Err(e) => err_code(&e),
        }
    })
}

// ---------------------------------------------------------------------------
// Transactions
// ---------------------------------------------------------------------------

/// Begin a transaction. Returns NULL on failure (invalid handle).
///
/// The database handle must stay alive and unused by other calls until the
/// transaction is finished. See the module docs for the threading contract.
#[no_mangle]
pub extern "C" fn slimdb_begin(db: *mut slimdb_db_t) -> *mut slimdb_txn_t {
    match catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `db` comes from `slimdb_open`; see `db_ref`.
        let db = match unsafe { db_ref(db) } {
            Ok(d) => d,
            Err(_) => return ptr::null_mut(),
        };
        let txn = db.transaction();
        // SAFETY justification (spec §14: no unsafe without justification and
        // a dedicated test):
        //
        // `Transaction` borrows `&mut Db` to guarantee exclusive access. The C
        // contract already guarantees exclusivity: a caller must not touch the
        // database handle while a transaction is open, and must finish the
        // transaction before closing the database. Erasing the borrow to
        // `'static` therefore cannot create aliasing that the C caller has not
        // already promised to avoid — the same reasoning that makes SQLite's
        // C API sound. `txn_ref` re-derives the mutable borrow per call, and
        // `slimdb_commit`/`slimdb_rollback` consume the boxed transaction.
        // Dedicated tests: `transactions_are_atomic` in tests/c_abi.rs, plus
        // the crash-test suite (transactions under torn writes).
        let txn: Transaction<'static> = unsafe { std::mem::transmute(txn) };
        Box::into_raw(Box::new(txn)) as *mut slimdb_txn_t
    })) {
        Ok(p) => p,
        Err(_) => ptr::null_mut(),
    }
}

/// Set a value within the transaction.
#[no_mangle]
pub extern "C" fn slimdb_txn_set_json(
    txn: *mut slimdb_txn_t,
    path: *const c_char,
    json: *const c_char,
) -> i32 {
    ffi_guard(|| {
        let txn = ffi_try!(unsafe { txn_ref(txn) });
        let path = ffi_try!(unsafe { cstr(path) });
        let json = ffi_try!(unsafe { cstr(json) });
        let value = match slimdb_core::json::parse(json) {
            Ok(v) => v,
            Err(_) => return SLIMDB_ERR_DECODE,
        };
        match txn.set(path, value) {
            Ok(()) => SLIMDB_OK,
            Err(e) => err_code(&e),
        }
    })
}

/// Delete a path within the transaction; `removed` receives 1 or 0.
#[no_mangle]
pub extern "C" fn slimdb_txn_delete(
    txn: *mut slimdb_txn_t,
    path: *const c_char,
    removed: *mut i32,
) -> i32 {
    ffi_guard(|| {
        let txn = ffi_try!(unsafe { txn_ref(txn) });
        let path = ffi_try!(unsafe { cstr(path) });
        match txn.delete(path) {
            Ok(v) => {
                ffi_try!(write_bool(removed, v));
                SLIMDB_OK
            }
            Err(e) => err_code(&e),
        }
    })
}

/// Clear all keys within the transaction.
#[no_mangle]
pub extern "C" fn slimdb_txn_clear(txn: *mut slimdb_txn_t) -> i32 {
    ffi_guard(|| {
        let txn = ffi_try!(unsafe { txn_ref(txn) });
        match txn.clear() {
            Ok(()) => SLIMDB_OK,
            Err(e) => err_code(&e),
        }
    })
}

/// Commit the transaction and free the handle.
#[no_mangle]
pub extern "C" fn slimdb_commit(txn: *mut slimdb_txn_t) -> i32 {
    ffi_guard(|| {
        if txn.is_null() {
            return SLIMDB_ERR_INVALID_ARG;
        }
        // SAFETY: see `txn_ref`; commit consumes the only owner of the Box.
        let txn = unsafe { Box::from_raw(txn as *mut Transaction<'static>) };
        match txn.commit() {
            Ok(()) => SLIMDB_OK,
            Err(e) => err_code(&e),
        }
    })
}

/// Roll back the transaction and free the handle.
#[no_mangle]
pub extern "C" fn slimdb_rollback(txn: *mut slimdb_txn_t) -> i32 {
    ffi_guard(|| {
        if txn.is_null() {
            return SLIMDB_ERR_INVALID_ARG;
        }
        // SAFETY: see `txn_ref`; rollback consumes the only owner of the Box.
        let txn = unsafe { Box::from_raw(txn as *mut Transaction<'static>) };
        txn.rollback();
        SLIMDB_OK
    })
}

// ---------------------------------------------------------------------------
// Memory helpers and version
// ---------------------------------------------------------------------------

/// Free a string returned by this library.
#[no_mangle]
pub extern "C" fn slimdb_string_free(s: *mut c_char) {
    if !s.is_null() {
        // SAFETY: only strings produced by `CString::into_raw` in this crate
        // are ever passed here (documented in the header).
        unsafe {
            drop(CString::from_raw(s));
        }
    }
}

/// Free a string list returned by `slimdb_keys`.
#[no_mangle]
pub extern "C" fn slimdb_string_list_free(list: *mut *mut c_char, count: usize) {
    if list.is_null() {
        return;
    }
    // SAFETY: `list` was produced by `slimdb_keys` as a `Vec<*mut c_char>`
    // with capacity `count`; each element is a `CString::into_raw` pointer.
    unsafe {
        let vec = Vec::from_raw_parts(list, count, count);
        for p in vec {
            drop(CString::from_raw(p));
        }
    }
}

/// Human-readable message for an error code (static, never freed).
#[no_mangle]
pub extern "C" fn slimdb_error_string(code: i32) -> *const c_char {
    let msg: &'static [u8] = match code {
        SLIMDB_OK => b"ok\0",
        SLIMDB_ERR_IO => b"I/O error\0",
        SLIMDB_ERR_CORRUPT => b"corrupt database\0",
        SLIMDB_ERR_DECODE => b"decode error\0",
        SLIMDB_ERR_UNSUPPORTED_VERSION => b"unsupported format version\0",
        SLIMDB_ERR_BAD_PATH => b"invalid path\0",
        SLIMDB_ERR_TYPE_MISMATCH => b"type mismatch\0",
        SLIMDB_ERR_INVALID_ARG => b"invalid argument\0",
        SLIMDB_ERR_TXN => b"transaction error\0",
        SLIMDB_ERR_PANIC => b"internal panic\0",
        _ => b"unknown error\0",
    };
    msg.as_ptr() as *const c_char
}

/// Library version (static, never freed).
#[no_mangle]
pub extern "C" fn slimdb_version() -> *const c_char {
    c"0.1.0".as_ptr()
}

/// ABI contract version (static, never freed). Bumped only on breaking ABI
/// changes.
#[no_mangle]
pub extern "C" fn slimdb_abi_version() -> *const c_char {
    c"1".as_ptr()
}
