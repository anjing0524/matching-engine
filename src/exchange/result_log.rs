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

/// Size-bounded rotating result log.
pub struct RotatingResultLog {
    directory: std::path::PathBuf,
    prefix: String,
    slot_size: usize,
    max_segment_bytes: u64,
    retain_segments: usize,
    next_segment: u64,
    current_bytes: u64,
    writer: Option<ResultLogWriter>,
}

impl RotatingResultLog {
    /// # Errors
    /// Returns an I/O error if the directory cannot be created.
    pub fn open(
        directory: &Path,
        prefix: &str,
        slot_size: usize,
        max_segment_bytes: u64,
        retain_segments: usize,
    ) -> io::Result<Self> {
        std::fs::create_dir_all(directory)?;
        Ok(Self {
            directory: directory.to_path_buf(),
            prefix: prefix.to_string(),
            slot_size,
            max_segment_bytes: max_segment_bytes.max(HEADER_SIZE as u64 + slot_size as u64),
            retain_segments: retain_segments.max(1),
            next_segment: 1,
            current_bytes: 0,
            writer: None,
        })
    }

    /// # Errors
    /// Returns an I/O error from segment creation/write/sync.
    pub fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.writer.is_none()
            || self.current_bytes.saturating_add(self.slot_size as u64) > self.max_segment_bytes
        {
            self.rotate()?;
        }
        if let Some(writer) = &mut self.writer {
            writer.append(bytes)?;
            writer.sync()?;
            self.current_bytes = self.current_bytes.saturating_add(self.slot_size as u64);
        }
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        let path = self
            .directory
            .join(format!("{}-{:08}.log", self.prefix, self.next_segment));
        self.next_segment += 1;
        self.writer = Some(ResultLogWriter::create(&path, self.slot_size)?);
        self.current_bytes = HEADER_SIZE as u64;
        self.prune()?;
        Ok(())
    }

    fn prune(&self) -> io::Result<()> {
        let mut segments = self.segments()?;
        segments.sort();
        while segments.len() > self.retain_segments {
            std::fs::remove_file(segments.remove(0))?;
        }
        Ok(())
    }

    /// # Errors
    /// Returns an I/O error if the directory cannot be read.
    pub fn segments(&self) -> io::Result<Vec<std::path::PathBuf>> {
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(&self.directory)? {
            let path = entry?.path();
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&format!("{}-", self.prefix)))
            {
                paths.push(path);
            }
        }
        paths.sort();
        Ok(paths)
    }
}

/// Rotating public result log.
pub struct RotatingPublicResultLog {
    inner: RotatingResultLog,
}

impl RotatingPublicResultLog {
    /// # Errors
    /// Returns an I/O error if the directory cannot be created.
    pub fn open(
        directory: &Path,
        prefix: &str,
        max_segment_bytes: u64,
        retain_segments: usize,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: RotatingResultLog::open(
                directory,
                prefix,
                PUBLIC_EVENT_SLOT_SIZE,
                max_segment_bytes,
                retain_segments,
            )?,
        })
    }

    /// # Errors
    /// Returns an I/O error from the write.
    pub fn append(&mut self, event: &PublicEvent) -> io::Result<()> {
        self.inner.append(&encode_public_event(event))
    }
}

/// Rotating private result log.
pub struct RotatingPrivateResultLog {
    inner: RotatingResultLog,
}

impl RotatingPrivateResultLog {
    /// # Errors
    /// Returns an I/O error if the directory cannot be created.
    pub fn open(
        directory: &Path,
        prefix: &str,
        max_segment_bytes: u64,
        retain_segments: usize,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: RotatingResultLog::open(
                directory,
                prefix,
                PRIVATE_EVENT_SLOT_SIZE,
                max_segment_bytes,
                retain_segments,
            )?,
        })
    }

    /// # Errors
    /// Returns an I/O error from the write.
    pub fn append(&mut self, event: &PrivateEvent) -> io::Result<()> {
        self.inner.append(&encode_private_event(event))
    }
}

/// Reads all public segments in a directory.
///
/// # Errors
/// Returns an I/O error for read/decode failures.
pub fn read_public_log_dir(directory: &Path) -> io::Result<Vec<PublicEvent>> {
    read_dir_slots(directory, PUBLIC_EVENT_SLOT_SIZE)?
        .iter()
        .map(|slot| decode_public_event(slot).map_err(|error| invalid(&format!("{error:?}"))))
        .collect()
}

/// Reads all private segments in a directory.
///
/// # Errors
/// Returns an I/O error for read/decode failures.
pub fn read_private_log_dir(directory: &Path) -> io::Result<Vec<PrivateEvent>> {
    read_dir_slots(directory, PRIVATE_EVENT_SLOT_SIZE)?
        .iter()
        .map(|slot| decode_private_event(slot).map_err(|error| invalid(&format!("{error:?}"))))
        .collect()
}

fn read_dir_slots(directory: &Path, slot_size: usize) -> io::Result<Vec<[u8; 128]>> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "log") {
            paths.push(path);
        }
    }
    paths.sort();
    let mut slots = Vec::new();
    for path in paths {
        slots.extend(read_slots(&path, slot_size)?);
    }
    Ok(slots)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotating_public_log_bounds_segments() {
        let directory = std::env::temp_dir().join(format!(
            "matching-engine-rotating-result-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let mut log = RotatingPublicResultLog::open(&directory, "public", 16 + 128 * 2, 2).unwrap();
        for seq in 1..=5 {
            log.append(&PublicEvent {
                seq,
                event_type: crate::exchange::public_events::PublicEventType::TimeTick,
                exchange_ts_ns: 0,
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
        }
        let events = read_public_log_dir(&directory).unwrap();
        assert!(!events.is_empty());
        let _ = std::fs::remove_dir_all(&directory);
    }

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
