//! Persistence infrastructure.
//!
//! Durability is WAL-only: every accepted order and cancellation is appended to
//! a CRC-checked, length-framed log before it is applied. The public durability
//! seam is [`Journal`], which owns sequencing, framing, flush, and recovery;
//! [`WalWriter`]/[`WalReader`]/[`RecoveryManager`] are implementation details.
//! Recovery replays the log from the beginning, which keeps the persisted and
//! in-memory states identical.
pub mod journal;
pub(crate) mod recovery;
pub mod snapshot;
pub mod types;
pub(crate) mod wal;

pub use journal::{Journal, JournalRecovery};
pub(crate) use recovery::RecoveryManager;
pub use snapshot::Snapshot;
pub use types::{PersistenceConfig, WalEntry, WalRecord};
pub(crate) use wal::{WalReader, WalWriter, read_start_file, write_start_file};

use std::path::{Path, PathBuf};
use thiserror::Error;

/// Returns the path of WAL file number `file_num` inside `data_dir`.
pub(crate) fn wal_path(data_dir: &Path, file_num: u64) -> PathBuf {
    data_dir.join(format!("wal.{file_num:06}.log"))
}

/// Returns the path of the first-live-WAL-file manifest.
pub(crate) fn wal_start_path(data_dir: &Path) -> PathBuf {
    data_dir.join("wal.start")
}

/// Persistence error type.
#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Deserialization error: {0}")]
    Deserialization(String),

    #[error("Corruption detected: {0}")]
    Corruption(String),
}

pub type Result<T> = std::result::Result<T, PersistenceError>;
