//! Registry-backed combo snapshot persistence and deterministic replay.
//!
//! A [`RegistrySnapshot`] is self-contained, so replay loads it into a fresh
//! [`InstrumentRegistry`] and applies the remaining command log through the same
//! combo/ordinary routing the live registry uses.

use crate::trading::core::protocol::Command;
use crate::trading::core::registry::{InstrumentRegistry, RegistrySnapshot};
use crate::trading::snapshot::{REGISTRY_SNAPSHOT_VERSION, snapshot_file};
use std::io;
use std::path::Path;

/// Writes the registry snapshot with atomic replacement.
///
/// # Errors
/// Returns an I/O error on serialization or file replacement.
pub fn save(path: &Path, registry: &InstrumentRegistry) -> io::Result<()> {
    snapshot_file::save(path, &registry.snapshot(), REGISTRY_SNAPSHOT_VERSION)
}

/// Loads a registry snapshot written by [`save`].
///
/// # Errors
/// Returns an I/O error for read, JSON, version, or checksum failures.
pub fn load(path: &Path) -> io::Result<RegistrySnapshot> {
    snapshot_file::load(path, REGISTRY_SNAPSHOT_VERSION)
}

/// Applies commands, routing combo commands by their non-zero pair id.
///
/// # Errors
/// Returns the executor's rejection message for the first command that fails.
pub fn apply(
    registry: &mut InstrumentRegistry,
    commands: impl IntoIterator<Item = Command>,
) -> Result<usize, String> {
    let mut applied = 0;
    for command in commands {
        if command.combo_id != 0 {
            registry
                .execute_combo(command)
                .map_err(|error| error.to_string())?;
        } else {
            registry
                .execute(command)
                .map_err(|error| error.to_string())?;
        }
        applied += 1;
    }
    Ok(applied)
}

/// Restores a registry snapshot and applies the remaining command log.
///
/// # Errors
/// Returns a reason if the snapshot cannot be loaded or restored, or if a
/// replayed command is rejected.
pub fn replay_from_snapshot(
    path: &Path,
    commands: impl IntoIterator<Item = Command>,
) -> Result<InstrumentRegistry, String> {
    let snapshot = load(path).map_err(|error| error.to_string())?;
    let mut registry = InstrumentRegistry::new();
    registry
        .restore_snapshot(snapshot)
        .map_err(|error| error.to_string())?;
    apply(&mut registry, commands)?;
    Ok(registry)
}
