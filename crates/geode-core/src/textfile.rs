//! A tile's request to read or write one UTF-8 text file, and its answer.
//! The types live here so `geode-data` (which does the I/O) and
//! `geode-shell` (which delivers the answer) share them without depending on
//! each other.

use crate::query::QueryKey;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextFileOp {
    /// Read the whole file; refuse one larger than `max_bytes`.
    Read { max_bytes: u64 },
    /// Replace the file with `text`, atomically.
    Write { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFileParams {
    pub key: QueryKey,
    /// The tile's counter, echoed so a stale answer can be told apart.
    pub tag: u64,
    pub path: PathBuf,
    pub op: TextFileOp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextFileResult {
    Read(Result<String, String>),
    Written(Result<(), String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFileOutcome {
    pub key: QueryKey,
    pub tag: u64,
    pub path: PathBuf,
    pub result: TextFileResult,
}
