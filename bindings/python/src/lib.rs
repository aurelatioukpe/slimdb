//! SlimDB Python binding.
//!
//! Per the spec, bindings sit on the C ABI (`slimdb_*`), never on the core's
//! internal structures: every method below forwards to a `slimdb_*` function.
//! Values cross as JSON strings; the Pythonic `slimdb` package converts to and
//! from Python objects with `json`.
//!
//! The GIL serializes all access to a handle, which upholds the C contract
//! (single-writer per handle): the raw pointers are therefore `Send`/`Sync`.

use std::ffi::{CStr, CString};
use std::ptr;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use slimdb_c::*;

fn code_error(code: i32, ctx: &str) -> PyErr {
    let msg = unsafe { CStr::from_ptr(slimdb_error_string(code)) }.to_string_lossy();
    PyRuntimeError::new_err(format!("{ctx}: {msg}"))
}

fn cstring(s: &str) -> PyResult<CString> {
    CString::new(s).map_err(|_| PyRuntimeError::new_err("string contains NUL byte"))
}

fn durability_code(mode: &str) -> PyResult<i32> {
    match mode {
        "safe" => Ok(SLIMDB_DURABILITY_SAFE),
        "balanced" => Ok(SLIMDB_DURABILITY_BALANCED),
        "fast" => Ok(SLIMDB_DURABILITY_FAST),
        other => Err(PyRuntimeError::new_err(format!(
            "invalid durability mode {other:?} (expected safe|balanced|fast)"
        ))),
    }
}

fn check(code: i32, ctx: &str) -> PyResult<()> {
    if code == SLIMDB_OK {
        Ok(())
    } else {
        Err(code_error(code, ctx))
    }
}

/// A SlimDB database handle.
#[pyclass]
struct Db {
    handle: *mut slimdb_db_t,
    closed: bool,
}

// SAFETY justification: the GIL guarantees that no two Python threads execute
// methods on the same object at once, matching the C ABI's single-writer
// contract; the pointer is only dereferenced by the C ABI functions.
unsafe impl Send for Db {}
unsafe impl Sync for Db {}

impl Db {
    fn check_open(&self) -> PyResult<()> {
        if self.closed || self.handle.is_null() {
            return Err(PyRuntimeError::new_err("database is closed"));
        }
        Ok(())
    }
}

// The raw pointers below are private fields created by this module (never
// caller-supplied), so dereferencing them through the C ABI is sound;
// clippy's `not_unsafe_ptr_arg_deref` does not apply.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[pymethods]
impl Db {
    /// Open (or create) the database at `path`.
    #[new]
    #[pyo3(signature = (path, durability="safe"))]
    fn new(path: &str, durability: &str) -> PyResult<Self> {
        let mode = durability_code(durability)?;
        let path = cstring(path)?;
        let mut handle: *mut slimdb_db_t = ptr::null_mut();
        let code = slimdb_open_with_mode(path.as_ptr(), mode, &mut handle);
        if code != SLIMDB_OK || handle.is_null() {
            return Err(code_error(code, "open"));
        }
        Ok(Db {
            handle,
            closed: false,
        })
    }

    /// Set the value at `path` from a JSON string.
    fn set_json(&mut self, path: &str, json: &str) -> PyResult<()> {
        self.check_open()?;
        let path = cstring(path)?;
        let json = cstring(json)?;
        check(
            slimdb_set_json(self.handle, path.as_ptr(), json.as_ptr()),
            "set",
        )
    }

    /// Get the value at `path` as a JSON string, or None if missing.
    fn get_json(&self, path: &str) -> PyResult<Option<String>> {
        self.check_open()?;
        let path = cstring(path)?;
        let mut out: *mut std::ffi::c_char = ptr::null_mut();
        let code = slimdb_get_json(self.handle, path.as_ptr(), &mut out);
        if code != SLIMDB_OK {
            return Err(code_error(code, "get"));
        }
        if out.is_null() {
            return Ok(None);
        }
        let s = unsafe { CStr::from_ptr(out) }
            .to_string_lossy()
            .into_owned();
        slimdb_string_free(out);
        Ok(Some(s))
    }

    /// Delete the value at `path`; returns True if something was removed.
    fn delete(&mut self, path: &str) -> PyResult<bool> {
        self.check_open()?;
        let path = cstring(path)?;
        let mut removed = 0i32;
        check(
            slimdb_delete(self.handle, path.as_ptr(), &mut removed),
            "delete",
        )?;
        Ok(removed != 0)
    }

    /// Whether a value exists at `path`.
    fn exists(&self, path: &str) -> PyResult<bool> {
        self.check_open()?;
        let path = cstring(path)?;
        let mut out = 0i32;
        check(
            slimdb_exists(self.handle, path.as_ptr(), &mut out),
            "exists",
        )?;
        Ok(out != 0)
    }

    /// Top-level keys, sorted.
    fn keys(&self) -> PyResult<Vec<String>> {
        self.check_open()?;
        let mut list: *mut *mut std::ffi::c_char = ptr::null_mut();
        let mut count = 0usize;
        check(slimdb_keys(self.handle, &mut list, &mut count), "keys")?;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let p = unsafe { *list.add(i) };
            out.push(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned());
        }
        slimdb_string_list_free(list, count);
        Ok(out)
    }

    fn clear(&mut self) -> PyResult<()> {
        self.check_open()?;
        check(slimdb_clear(self.handle), "clear")
    }

    fn flush(&mut self) -> PyResult<()> {
        self.check_open()?;
        check(slimdb_flush(self.handle), "flush")
    }

    fn compact(&mut self) -> PyResult<()> {
        self.check_open()?;
        check(slimdb_compact(self.handle), "compact")
    }

    fn backup(&mut self, dest: &str) -> PyResult<()> {
        self.check_open()?;
        let dest = cstring(dest)?;
        check(slimdb_backup(self.handle, dest.as_ptr()), "backup")
    }

    fn restore(&mut self, src: &str) -> PyResult<()> {
        self.check_open()?;
        let src = cstring(src)?;
        check(slimdb_restore(self.handle, src.as_ptr()), "restore")
    }

    /// Database statistics as a dict.
    fn stats(&self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        self.check_open()?;
        let mut s = slimdb_stats_t {
            keys: 0,
            data_file_size: 0,
            wal_file_size: 0,
            sequence: 0,
            durability: -1,
        };
        check(slimdb_stats(self.handle, &mut s), "stats")?;
        let d = PyDict::new(py);
        d.set_item("keys", s.keys)?;
        d.set_item("data_file_size", s.data_file_size)?;
        d.set_item("wal_file_size", s.wal_file_size)?;
        d.set_item("sequence", s.sequence)?;
        d.set_item("durability", s.durability)?;
        Ok(d.unbind())
    }

    /// Begin a transaction.
    fn transaction(&mut self) -> PyResult<Txn> {
        self.check_open()?;
        let handle = slimdb_begin(self.handle);
        if handle.is_null() {
            return Err(PyRuntimeError::new_err("begin transaction failed"));
        }
        Ok(Txn {
            handle,
            finished: false,
        })
    }

    /// Close the database (compacts first).
    fn close(&mut self) -> PyResult<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let code = slimdb_close(self.handle);
        self.handle = ptr::null_mut();
        check(code, "close")
    }

    fn __enter__(&mut self) -> PyResult<Self> {
        self.check_open()?;
        Ok(Self {
            handle: self.handle,
            closed: false,
        })
    }

    fn __exit__(&mut self, _ty: Py<PyAny>, _val: Py<PyAny>, _tb: Py<PyAny>) -> PyResult<()> {
        self.close()
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        // Best-effort close on garbage collection; errors are swallowed
        // (Drop cannot report failures). Prefer explicit close()/with.
        if !self.closed && !self.handle.is_null() {
            let _ = slimdb_close(self.handle);
        }
    }
}

/// A transaction handle.
#[pyclass]
struct Txn {
    handle: *mut slimdb_txn_t,
    finished: bool,
}

unsafe impl Send for Txn {}
unsafe impl Sync for Txn {}

impl Txn {
    fn check(&self) -> PyResult<()> {
        if self.finished || self.handle.is_null() {
            return Err(PyRuntimeError::new_err("transaction is finished"));
        }
        Ok(())
    }
}

// Same justification as `Db`: the handle is created by `slimdb_begin` and
// stored in a private field, never taken from the caller.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[pymethods]
impl Txn {
    fn set_json(&mut self, path: &str, json: &str) -> PyResult<()> {
        self.check()?;
        let path = cstring(path)?;
        let json = cstring(json)?;
        check(
            slimdb_txn_set_json(self.handle, path.as_ptr(), json.as_ptr()),
            "txn set",
        )
    }

    fn delete(&mut self, path: &str) -> PyResult<bool> {
        self.check()?;
        let path = cstring(path)?;
        let mut removed = 0i32;
        check(
            slimdb_txn_delete(self.handle, path.as_ptr(), &mut removed),
            "txn delete",
        )?;
        Ok(removed != 0)
    }

    fn clear(&mut self) -> PyResult<()> {
        self.check()?;
        check(slimdb_txn_clear(self.handle), "txn clear")
    }

    fn commit(&mut self) -> PyResult<()> {
        self.check()?;
        self.finished = true;
        let code = slimdb_commit(self.handle);
        self.handle = ptr::null_mut();
        check(code, "commit")
    }

    fn rollback(&mut self) -> PyResult<()> {
        self.check()?;
        self.finished = true;
        let code = slimdb_rollback(self.handle);
        self.handle = ptr::null_mut();
        check(code, "rollback")
    }

    fn __enter__(&mut self) -> PyResult<Self> {
        self.check()?;
        Ok(Self {
            handle: self.handle,
            finished: false,
        })
    }

    fn __exit__(&mut self, _ty: Py<PyAny>, _val: Py<PyAny>, _tb: Py<PyAny>) -> PyResult<()> {
        if !self.finished {
            let _ = slimdb_rollback(self.handle);
            self.finished = true;
            self.handle = ptr::null_mut();
        }
        Ok(())
    }
}

impl Drop for Txn {
    fn drop(&mut self) {
        if !self.finished && !self.handle.is_null() {
            let _ = slimdb_rollback(self.handle);
        }
    }
}

/// SlimDB version string.
#[pyfunction]
fn version() -> &'static str {
    "0.1.0"
}

#[pymodule]
fn _slimdb(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Db>()?;
    m.add_class::<Txn>()?;
    m.add_function(wrap_pyfunction!(version, m)?)?;
    Ok(())
}
