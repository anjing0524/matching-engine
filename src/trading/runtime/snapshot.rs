//! Runtime snapshot envelope and schema handling.

use crate::trading::core::kernel::CoreSnapshot;
use crate::trading::core::registry::RegistrySnapshot;

/// Full runtime snapshot for either command path.
///
/// The command owner is a closed enum: a snapshot cannot simultaneously carry
/// core and registry state, or carry neither. Version/schema changes are still
/// rejected by `snapshot_file`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TradingSnapshot {
    /// Per-user private sequence and replay log.
    #[serde(default)]
    pub private: Option<crate::trading::gateway::private_replay::PrivateDownlinkSnapshot>,
    /// Exactly one command-owner shape.
    pub data: TradingSnapshotData,
}

/// The mutually exclusive command-owner shapes.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradingSnapshotData {
    /// Legacy/split core runtime.
    Core {
        /// Single Contract core state.
        core: CoreSnapshot,
    },
    /// Production single-process registry runtime.
    Registry {
        /// Registry-backed venue state.
        registry: RegistrySnapshot,
    },
}

const TRADING_SNAPSHOT_VERSION: u32 = crate::trading::snapshot::schema::TRADING.version;

impl TradingSnapshot {
    /// Returns the core shape, if present.
    #[must_use]
    pub fn core(&self) -> Option<&CoreSnapshot> {
        match &self.data {
            TradingSnapshotData::Core { core } => Some(core),
            TradingSnapshotData::Registry { .. } => None,
        }
    }

    /// Returns the registry shape, if present.
    #[must_use]
    pub fn registry(&self) -> Option<&RegistrySnapshot> {
        match &self.data {
            TradingSnapshotData::Registry { registry } => Some(registry),
            TradingSnapshotData::Core { .. } => None,
        }
    }

    /// Writes a versioned, checksummed snapshot with atomic replacement.
    ///
    /// # Errors
    /// Returns an I/O error on serialization or file replacement.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        crate::trading::snapshot::snapshot_file::save(path, self, TRADING_SNAPSHOT_VERSION)
    }

    /// Loads a snapshot written by [`TradingSnapshot::save`].
    ///
    /// # Errors
    /// Returns an I/O error for read, JSON, version, or checksum failures.
    pub fn load(path: &std::path::Path) -> std::io::Result<Self> {
        crate::trading::snapshot::snapshot_file::load(path, TRADING_SNAPSHOT_VERSION)
    }
}
