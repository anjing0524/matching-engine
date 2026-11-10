//! Versioned, checksummed JSON snapshot files.
//!
//! The format lives here so core and runtime snapshots share one
//! serialization, CRC, and atomic-replacement implementation.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io;
use std::path::Path;

#[derive(serde::Serialize, serde::Deserialize)]
struct Envelope {
    version: u32,
    checksum: u32,
    payload: String,
}

/// Encodes one snapshot value with a version and CRC32 checksum.
///
/// # Errors
/// Returns a JSON serialization error.
pub fn encode<T: Serialize>(snapshot: &T, version: u32) -> Result<String, serde_json::Error> {
    let payload = serde_json::to_string(snapshot)?;
    serde_json::to_string(&Envelope {
        version,
        checksum: crc32fast::hash(payload.as_bytes()),
        payload,
    })
}

/// Decodes and validates one snapshot value.
///
/// # Errors
/// Returns a message for JSON, version, or checksum failures.
pub fn decode<T: DeserializeOwned>(json: &str, expected_version: u32) -> Result<T, String> {
    let envelope: Envelope = serde_json::from_str(json).map_err(|error| error.to_string())?;
    if envelope.version != expected_version {
        return Err(format!(
            "unsupported snapshot version {} (expected {expected_version})",
            envelope.version
        ));
    }
    if crc32fast::hash(envelope.payload.as_bytes()) != envelope.checksum {
        return Err("snapshot checksum mismatch".to_string());
    }
    serde_json::from_str(&envelope.payload).map_err(|error| error.to_string())
}

/// Writes a snapshot with atomic replacement.
///
/// # Errors
/// Returns an I/O error for serialization, write, or rename failure.
pub fn save<T: Serialize>(path: &Path, snapshot: &T, version: u32) -> io::Result<()> {
    let json = encode(snapshot, version)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, json)?;
    std::fs::rename(temporary, path)
}

/// Reads and validates a snapshot written by [`save`].
///
/// # Errors
/// Returns an I/O error for read failures, or invalid data for JSON, version,
/// or checksum failures.
pub fn load<T: DeserializeOwned>(path: &Path, expected_version: u32) -> io::Result<T> {
    let json = std::fs::read_to_string(path)?;
    decode(&json, expected_version)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct Value {
        id: u64,
    }

    #[test]
    fn roundtrips_and_rejects_bad_version_or_checksum() {
        let json = encode(&Value { id: 7 }, 3).unwrap();
        assert_eq!(decode::<Value>(&json, 3).unwrap(), Value { id: 7 });
        assert!(decode::<Value>(&json, 2).is_err());

        let corrupted = json.replacen("\"payload\":\"", "\"payload\":\"x", 1);
        assert!(decode::<Value>(&corrupted, 3).is_err());
    }
    #[test]
    fn core_snapshot_json_and_file_roundtrip() {
        use crate::domain::orderbook::ContractSpec;
        use crate::shared::SymbolId;
        use crate::shared::protocol::OrderType;
        use crate::trading::core::kernel::{CoreSnapshot, LimitOrderCommand, TradingCore};
        use crate::trading::core::positions::PositionKey;
        use crate::trading::core::protocol::{Hedge, Side};

        fn core() -> TradingCore {
            TradingCore::new(
                ContractSpec::new("BTCUSDT", 1, 40_000, 70_000),
                SymbolId::intern("BTCUSDT"),
            )
        }

        let mut state = core();
        let _ = state.submit_limit(LimitOrderCommand {
            user_id: 1,
            client_order_id: 1,
            side: OrderType::Buy,
            price_ticks: 49_000,
            quantity: 5,
        });
        state
            .positions_mut()
            .apply_open(PositionKey::new(1, Side::Buy, Hedge::Spec), 2)
            .unwrap();
        let expected = state.snapshot();
        let json = encode(
            &state.snapshot(),
            crate::trading::snapshot::CORE_SNAPSHOT_VERSION,
        )
        .unwrap();
        let snapshot: CoreSnapshot =
            decode(&json, crate::trading::snapshot::CORE_SNAPSHOT_VERSION).unwrap();
        let mut restored = core();
        restored.restore_snapshot(snapshot).unwrap();
        assert_eq!(restored.snapshot(), expected);

        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-snapshot-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        save(
            &path,
            &state.snapshot(),
            crate::trading::snapshot::CORE_SNAPSHOT_VERSION,
        )
        .unwrap();
        let loaded: CoreSnapshot =
            load(&path, crate::trading::snapshot::CORE_SNAPSHOT_VERSION).unwrap();
        assert_eq!(loaded, expected);

        let mut tampered: serde_json::Value = serde_json::from_str(&json).unwrap();
        tampered["checksum"] = serde_json::Value::from(0);
        let error = decode::<CoreSnapshot>(
            &tampered.to_string(),
            crate::trading::snapshot::CORE_SNAPSHOT_VERSION,
        )
        .unwrap_err();
        assert!(error.contains("checksum"));
        let _ = std::fs::remove_file(&path);
    }
}
