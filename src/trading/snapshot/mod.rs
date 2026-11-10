//! Versioned snapshot files and deterministic replay.

pub mod replay;
pub mod snapshot_file;

/// On-disk core snapshot schema version.
pub const CORE_SNAPSHOT_VERSION: u32 = 6;
