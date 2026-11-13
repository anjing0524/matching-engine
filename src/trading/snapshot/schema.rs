//! One registry of snapshot schemas and versions.
//!
//! These versions move independently, but their values and names live here so a
//! change cannot silently update one writer without the compatibility table.

/// Which snapshot payload a version describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SnapshotKind {
    /// Single Contract core state.
    Core,
    /// Registry-backed venue state.
    Registry,
    /// Runtime envelope (core or registry shape plus private replay).
    Trading,
}

/// One schema/version pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotSchema {
    /// Payload kind.
    pub kind: SnapshotKind,
    /// Current accepted version.
    pub version: u32,
}

/// Current core snapshot schema.
pub const CORE: SnapshotSchema = SnapshotSchema {
    kind: SnapshotKind::Core,
    version: 7,
};

/// Current registry snapshot schema.
pub const REGISTRY: SnapshotSchema = SnapshotSchema {
    kind: SnapshotKind::Registry,
    version: 4,
};

/// Current runtime envelope schema.
pub const TRADING: SnapshotSchema = SnapshotSchema {
    kind: SnapshotKind::Trading,
    version: 10,
};

/// Returns the current version for `kind`.
#[must_use]
pub const fn version(kind: SnapshotKind) -> u32 {
    match kind {
        SnapshotKind::Core => CORE.version,
        SnapshotKind::Registry => REGISTRY.version,
        SnapshotKind::Trading => TRADING.version,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_unique_per_kind_and_stable() {
        assert_eq!(version(SnapshotKind::Core), 7);
        assert_eq!(version(SnapshotKind::Registry), 4);
        assert_eq!(version(SnapshotKind::Trading), 10);
    }
}
