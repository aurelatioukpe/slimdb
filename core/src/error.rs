//! SlimDB error types.
//!
//! Every fallible operation in the core returns `Result<T, SlimDbError>`.
//! The core never panics on user input or I/O failures: all failures are
//! surfaced as values of this type.

use std::fmt;

/// Result alias used across the whole core.
pub type Result<T> = std::result::Result<T, SlimDbError>;

/// Errors produced by the SlimDB engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlimDbError {
    /// An underlying I/O failure, with human-readable context.
    Io(String),
    /// The database or WAL file is corrupted (bad magic, bad checksum,
    /// records that cannot be parsed outside of a truncatable tail).
    Corrupt(String),
    /// A value could not be encoded or decoded.
    Decode(String),
    /// The on-disk format version is not supported by this build.
    UnsupportedVersion(u32),
    /// A path such as `"a..b"` or `""` is malformed.
    BadPath(String),
    /// A path traverses a value that is not an object (e.g. `set("a.b", 1)`
    /// when `a` is an integer).
    TypeMismatch { path: String, detail: String },
    /// An argument is invalid (e.g. nesting deeper than the limit).
    InvalidArgument(String),
    /// A transaction operation was used illegally (e.g. nested transaction).
    Transaction(String),
}

impl fmt::Display for SlimDbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SlimDbError::Io(msg) => write!(f, "I/O error: {msg}"),
            SlimDbError::Corrupt(msg) => write!(f, "corrupt database: {msg}"),
            SlimDbError::Decode(msg) => write!(f, "decode error: {msg}"),
            SlimDbError::UnsupportedVersion(v) => {
                write!(f, "unsupported format version {v}")
            }
            SlimDbError::BadPath(p) => write!(f, "invalid path: {p:?}"),
            SlimDbError::TypeMismatch { path, detail } => {
                write!(f, "type mismatch at path {path:?}: {detail}")
            }
            SlimDbError::InvalidArgument(msg) => write!(f, "invalid argument: {msg}"),
            SlimDbError::Transaction(msg) => write!(f, "transaction error: {msg}"),
        }
    }
}

impl std::error::Error for SlimDbError {}

impl From<std::io::Error> for SlimDbError {
    fn from(e: std::io::Error) -> Self {
        SlimDbError::Io(e.to_string())
    }
}

/// Wrap an [`std::io::Error`] with context describing what was being done.
pub fn io_ctx(context: &str, e: std::io::Error) -> SlimDbError {
    SlimDbError::Io(format!("{context}: {e}"))
}
