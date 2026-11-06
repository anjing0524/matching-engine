//! Persistent public/private event result logs.
//!
//! Layout: 16-byte header followed by fixed-size event slots. These logs are
//! the audit/replay counterpart to the command log.

use crate::exchange::private_events::PrivateEvent;
use crate::exchange::public_events::PublicEvent;
use crate::exchange::transport::{
    PRIVATE_EVENT_SLOT_SIZE, PUBLIC_EVENT_SLOT_SIZE, decode_private_event, decode_public_event,
    encode_private_event, encode_public_event,
};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

const MAGIC: &[u8; 8] = b"EXRES1\0\0";
const HEADER_SIZE: usize = 16;
const VERSION: u32 = 1;

pub struct ResultLogWriter {
    file: File,
    slot_size: usize,
}

impl ResultLogWriter {
    /// Creates or truncates a result log for a fixed slot size.
    ///
    /// # Errors
    /// Returns an I/O error if the file cannot be created.
    pub fn create(path: &Path, slot_size: usize) -> io::Result<Self> {
        let mut file = File::create(path)?;
        file.write_all(MAGIC)?;
        file.write_all(&VERSION.to_le_bytes())?;
        file.write_all(&(slot_size as u32).to_le_bytes())?;
        Ok(Self { file, slot_size })
    }

    /// Appends one encoded result slot.
    ///
    /// # Errors
    /// Returns an I/O error from the write.
    pub fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() != self.slot_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "result slot size mismatch",
            ));
        }
        self.file.write_all(bytes)
    }

    /// Flushes to stable storage.
    ///
    /// # Errors
    /// Returns an I/O error from `fsync`.
    pub fn sync(&mut self) -> io::Result<()> {
        self.file.sync_data()
    }
}

pub struct PublicResultLog {
    writer: ResultLogWriter,
}

impl PublicResultLog {
    /// # Errors
    /// Returns an I/O error if the file cannot be created.
    pub fn create(path: &Path) -> io::Result<Self> {
        Ok(Self {
            writer: ResultLogWriter::create(path, PUBLIC_EVENT_SLOT_SIZE)?,
        })
    }

    /// # Errors
    /// Returns an I/O error from the write.
    pub fn append(&mut self, event: &PublicEvent) -> io::Result<()> {
        self.writer.append(&encode_public_event(event))
    }

    /// # Errors
    /// Returns an I/O error from `fsync`.
    pub fn sync(&mut self) -> io::Result<()> {
        self.writer.sync()
    }
}

pub struct PrivateResultLog {
    writer: ResultLogWriter,
}

impl PrivateResultLog {
    /// # Errors
    /// Returns an I/O error if the file cannot be created.
    pub fn create(path: &Path) -> io::Result<Self> {
        Ok(Self {
            writer: ResultLogWriter::create(path, PRIVATE_EVENT_SLOT_SIZE)?,
        })
    }

    /// # Errors
    /// Returns an I/O error from the write.
    pub fn append(&mut self, event: &PrivateEvent) -> io::Result<()> {
        self.writer.append(&encode_private_event(event))
    }

    /// # Errors
    /// Returns an I/O error from `fsync`.
    pub fn sync(&mut self) -> io::Result<()> {
        self.writer.sync()
    }
}

/// Reads a public result log.
///
/// # Errors
/// Returns an I/O error for header/slot/decode failures.
pub fn read_public_log(path: &Path) -> io::Result<Vec<PublicEvent>> {
    let slots = read_slots(path, PUBLIC_EVENT_SLOT_SIZE)?;
    slots
        .iter()
        .map(|slot| decode_public_event(slot).map_err(|error| invalid(&format!("{error:?}"))))
        .collect()
}

/// Reads a private result log.
///
/// # Errors
/// Returns an I/O error for header/slot/decode failures.
pub fn read_private_log(path: &Path) -> io::Result<Vec<PrivateEvent>> {
    let slots = read_slots(path, PRIVATE_EVENT_SLOT_SIZE)?;
    slots
        .iter()
        .map(|slot| decode_private_event(slot).map_err(|error| invalid(&format!("{error:?}"))))
        .collect()
}

fn read_slots(path: &Path, expected_slot_size: usize) -> io::Result<Vec<[u8; 128]>> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if bytes.len() < HEADER_SIZE {
        return Err(invalid("short result log header"));
    }
    if &bytes[0..8] != MAGIC {
        return Err(invalid("invalid result log magic"));
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().expect("4 bytes"));
    if version != VERSION {
        return Err(invalid("unsupported result log version"));
    }
    let slot_size = u32::from_le_bytes(bytes[12..16].try_into().expect("4 bytes")) as usize;
    if slot_size != expected_slot_size {
        return Err(invalid("result log slot size mismatch"));
    }
    let payload = &bytes[HEADER_SIZE..];
    if !payload.len().is_multiple_of(slot_size) {
        return Err(invalid("truncated result slot"));
    }
    let mut slots = Vec::with_capacity(payload.len() / slot_size);
    for chunk in payload.chunks_exact(slot_size) {
        let mut slot = [0u8; 128];
        slot[..slot_size].copy_from_slice(chunk);
        slots.push(slot);
    }
    Ok(slots)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_and_private_logs_roundtrip() {
        let dir =
            std::env::temp_dir().join(format!("matching-engine-result-log-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let public_path = dir.join("public.log");
        let private_path = dir.join("private.log");
        let _ = std::fs::remove_file(&public_path);
        let _ = std::fs::remove_file(&private_path);

        let mut public = PublicResultLog::create(&public_path).unwrap();
        public
            .append(&PublicEvent {
                seq: 1,
                event_type: crate::exchange::public_events::PublicEventType::TimeTick,
                exchange_ts_ns: 2,
                phase: 3,
                instrument_id: 0,
                order_id: 0,
                trade_id: 0,
                buy_order_id: 0,
                sell_order_id: 0,
                side: None,
                price_ticks: 0,
                quantity: 0,
                volume: 0,
            })
            .unwrap();
        public.sync().unwrap();

        let mut private = PrivateResultLog::create(&private_path).unwrap();
        private
            .append(&PrivateEvent {
                seq: 1,
                order_id: 2,
                client_order_id: 3,
                account_id: 4,
                status: crate::exchange::private_events::PrivateStatus::Accepted,
                filled_delta: 0,
                remaining: 0,
                reason: None,
            })
            .unwrap();
        private.sync().unwrap();

        assert_eq!(read_public_log(&public_path).unwrap().len(), 1);
        assert_eq!(read_private_log(&private_path).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
