//! Versioned, checksummed JSON snapshot files.
//!
//! The format lives here so core and runtime snapshots share one
//! serialization, CRC, and atomic-replacement implementation.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io;
use std::path::Path;

#[derive(serde::Serialize, serde::Deserialize)]
struct Envelope<T> {
    version: u32,
    snapshot: T,
    checksum: u32,
}

/// Encodes one snapshot value with a version and CRC32 checksum.
///
/// # Errors
/// Returns a JSON serialization error.
pub fn encode<T: Serialize>(snapshot: &T, version: u32) -> Result<String, serde_json::Error> {
    let payload = serde_json::to_vec(snapshot)?;
    serde_json::to_string(&Envelope {
        version,
        snapshot,
        checksum: crc32fast::hash(&payload),
    })
}

/// Decodes and validates one snapshot value.
///
/// # Errors
/// Returns a message for JSON, version, or checksum failures.
pub fn decode<T: DeserializeOwned + Serialize>(
    json: &str,
    expected_version: u32,
) -> Result<T, String> {
    let envelope: Envelope<T> = serde_json::from_str(json).map_err(|error| error.to_string())?;
    if envelope.version != expected_version {
        return Err(format!(
            "unsupported snapshot version {} (expected {expected_version})",
            envelope.version
        ));
    }
    let payload = serde_json::to_vec(&envelope.snapshot).map_err(|error| error.to_string())?;
    if crc32fast::hash(&payload) != envelope.checksum {
        return Err("snapshot checksum mismatch".to_string());
    }
    Ok(envelope.snapshot)
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
pub fn load<T: DeserializeOwned + Serialize>(path: &Path, expected_version: u32) -> io::Result<T> {
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

        let corrupted = json.replacen("\"id\":7", "\"id\":8", 1);
        assert!(decode::<Value>(&corrupted, 3).is_err());
    }
}
