//! Versioned snapshot files and deterministic replay.

pub mod registry;
pub mod replay;
pub mod snapshot_file;

/// On-disk core snapshot schema version.
pub const CORE_SNAPSHOT_VERSION: u32 = 7;

/// On-disk registry-backed combo snapshot schema version.
pub const REGISTRY_SNAPSHOT_VERSION: u32 = 2;
