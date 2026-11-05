//! Write-ahead log.
//!
//! The WAL is an append-only sequence of length-framed, CRC-checked records.
//! Writers flush before an order is acknowledged and optionally fsync; readers
//! validate every record and distinguish a torn tail (expected after a crash)
//! from real corruption.
//!
//! ## File format
//! ```text
//! [Header] [Record1] [Record2] ... [RecordN]
//!
//! Header: magic(4B) | version(2B) | flags(2B)
//! Record: length(4B) | seq(8B) | timestamp(8B) | crc32(4B) | payload(length B)
//! ```
//!
//! The payload is a [`WalEntry`] encoded with [`crate::shared::wire`].
use super::types::{WalEntry, WalRecord};
use super::{PersistenceError, Result, wal_path, wal_start_path};
use crate::shared::wire::{decode, encode_to_vec};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const WAL_MAGIC: u32 = 0x5741_4C46; // "WALF"
const WAL_VERSION: u16 = 3;
/// Size of the file header in bytes.
pub(crate) const HEADER_LEN: u64 = 8;
/// Bytes of per-record framing: length + seq + timestamp + crc.
const RECORD_OVERHEAD: u64 = 4 + 8 + 8 + 4;
/// Upper bound for one encoded WAL payload. Order/trade entries are far below
/// this; the bound prevents a corrupted length field from triggering a multi-
/// gigabyte allocation during recovery.
const MAX_RECORD_LEN: usize = 1024 * 1024;

/// Highest existing WAL file number in `data_dir`, or 0 when none exist.
fn latest_file_num(data_dir: &Path) -> Result<u64> {
    let mut latest = 0u64;
    for entry in std::fs::read_dir(data_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(number) = name
            .strip_prefix("wal.")
            .and_then(|name| name.strip_suffix(".log"))
            .and_then(|number| number.parse::<u64>().ok())
        else {
            continue;
        };
        latest = latest.max(number);
    }
    Ok(latest)
}

/// Reads the first live WAL file number (0 when absent or uninitialised).
pub(crate) fn read_start_file(data_dir: &Path) -> Result<u64> {
    let path = wal_start_path(data_dir);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let Some(bytes) = bytes.get(..8) else {
                return Err(PersistenceError::Corruption(format!(
                    "{} is truncated",
                    path.display()
                )));
            };
            Ok(u64::from_le_bytes(bytes.try_into().expect("8 bytes")))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

/// Atomically records the first live WAL file number.
pub(crate) fn write_start_file(data_dir: &Path, start: u64) -> Result<()> {
    let temp = data_dir.join(".wal.start.tmp");
    let final_path = wal_start_path(data_dir);
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&temp)?;
    file.write_all(&start.to_le_bytes())?;
    file.flush()?;
    file.sync_all()?;
    std::fs::rename(&temp, &final_path)?;
    fsync_dir(data_dir)?;
    Ok(())
}

/// Appends records to the active WAL file.
pub struct WalWriter {
    file: BufWriter<File>,
    seq: AtomicU64,
    sync: bool,
    file_size: usize,
    max_file_size: usize,
    file_num: u64,
    data_dir: PathBuf,
}

impl WalWriter {
    /// Opens (or creates) the WAL in `data_dir`.
    ///
    /// An empty or partially written header is treated as a fresh file: no
    /// record can exist before the header, so the file is reset.
    pub fn new(data_dir: PathBuf, sync: bool, max_file_size: usize) -> Result<Self> {
        std::fs::create_dir_all(&data_dir)?;

        let file_num = latest_file_num(&data_dir)?;
        let file_path = wal_path(&data_dir, file_num);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)?;

        let mut writer = BufWriter::new(file);
        let existing_len = writer.get_ref().metadata()?.len();
        let file_size = if existing_len < HEADER_LEN {
            writer.get_mut().set_len(0)?;
            Self::write_header(&mut writer)?;
            // Make the new file durable, including its directory entry.
            writer.flush()?;
            writer.get_mut().sync_all()?;
            fsync_dir(&data_dir)?;
            HEADER_LEN as usize
        } else {
            existing_len as usize
        };

        Ok(Self {
            file: writer,
            seq: AtomicU64::new(0),
            sync,
            file_size,
            max_file_size,
            file_num,
            data_dir,
        })
    }

    /// Starts sequence numbering at `seq`.
    #[must_use]
    pub fn with_seq(self, seq: u64) -> Self {
        self.seq.store(seq, Ordering::Relaxed);
        self
    }

    /// Appends one record, returning its sequence number.
    pub fn write(&mut self, record: &WalRecord) -> Result<u64> {
        let payload = encode_to_vec(&record.entry)
            .map_err(|error| PersistenceError::Serialization(error.to_string()))?;
        if payload.len() > MAX_RECORD_LEN {
            return Err(PersistenceError::Serialization(format!(
                "WAL entry is {} bytes, exceeding the {MAX_RECORD_LEN} byte limit",
                payload.len()
            )));
        }
        let crc = crc32fast::hash(&payload);

        self.file.write_all(&(payload.len() as u32).to_le_bytes())?;
        self.file.write_all(&record.seq.to_le_bytes())?;
        self.file.write_all(&record.timestamp.to_le_bytes())?;
        self.file.write_all(&crc.to_le_bytes())?;
        self.file.write_all(&payload)?;

        self.file_size += (RECORD_OVERHEAD + payload.len() as u64) as usize;

        if self.file_size >= self.max_file_size {
            self.rotate()?;
        }

        Ok(record.seq)
    }

    /// Flushes buffered bytes to the OS and returns a clone for a separate
    /// fsync thread; does not call `sync_all`.
    pub(crate) fn flush_handle(&mut self) -> Result<File> {
        self.file.flush()?;
        Ok(self.file.get_ref().try_clone()?)
    }

    /// Flushes buffered records, fsyncing when enabled.
    pub fn flush(&mut self) -> Result<()> {
        self.file.flush()?;
        if self.sync {
            self.file.get_mut().sync_all()?;
        }
        Ok(())
    }

    /// Rotates to a fresh WAL file.
    ///
    /// Used after a snapshot so the previous active file becomes eligible for
    /// pruning without deleting it (file numbering must remain gap-free).
    pub(crate) fn rotate_now(&mut self) -> Result<()> {
        self.rotate()
    }

    /// Rotates to a new WAL file.
    fn rotate(&mut self) -> Result<()> {
        self.flush()?;

        self.file_num += 1;
        let new_path = wal_path(&self.data_dir, self.file_num);
        let new_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&new_path)?;

        let mut new_writer = BufWriter::new(new_file);
        Self::write_header(&mut new_writer)?;
        new_writer.flush()?;
        new_writer.get_mut().sync_all()?;
        fsync_dir(&self.data_dir)?;

        self.file = new_writer;
        self.file_size = HEADER_LEN as usize;

        Ok(())
    }

    fn write_header(writer: &mut BufWriter<File>) -> Result<()> {
        writer.write_all(&WAL_MAGIC.to_le_bytes())?;
        writer.write_all(&WAL_VERSION.to_le_bytes())?;
        writer.write_all(&[0u8; 2])?;
        Ok(())
    }

    /// Returns the next sequence number to assign.
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Number of the active WAL file.
    #[must_use]
    pub fn active_file_num(&self) -> u64 {
        self.file_num
    }

    /// Data directory containing the WAL files.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Advances the writer's next-sequence watermark for externally assigned
    /// records (decoupled journaler path).
    pub(crate) fn observe_seq(&self, seq: u64) {
        self.seq.fetch_max(seq + 1, Ordering::Relaxed);
    }

    /// Returns the sequence number that will be assigned next.
    #[must_use]
    pub fn current_seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }
}

/// Result of scanning a WAL directory.
pub struct WalScan {
    /// Complete records with `seq > min_seq`, in write order.
    pub records: Vec<WalRecord>,
    /// Last sequence number parsed, including records skipped by `min_seq`.
    pub last_seq: u64,
    /// Path and valid length of a final file with a torn tail. Recovery may
    /// truncate the file to this length before appending new records.
    pub truncated_tail: Option<(PathBuf, u64)>,
}

/// Outcome of reading a single WAL file.
enum FileOutcome {
    /// The whole file was consumed; `u64` is the end offset of the last record.
    Complete(Vec<WalRecord>, u64),
    /// The file ended in the middle of a record (crash during append).
    Truncated(Vec<WalRecord>, u64),
}

/// fsyncs a directory so a newly created/rotated WAL file survives a crash.
#[cfg(unix)]
fn fsync_dir(dir: &Path) -> Result<()> {
    let file = File::open(dir)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn fsync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

/// Reads WAL files.
pub struct WalReader {
    data_dir: PathBuf,
}

impl WalReader {
    /// Creates a reader for `data_dir`.
    #[must_use]
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }

    /// Reads every complete record, ignoring a torn tail.
    #[cfg(test)]
    fn read_all(&self) -> Result<Vec<WalRecord>> {
        Ok(self.scan()?.records)
    }

    /// Scans all WAL files and reports a torn tail in the last file.
    ///
    /// A torn tail is only accepted in the newest WAL file; the same condition
    /// in an older file indicates real corruption.
    #[cfg(test)]
    pub fn scan(&self) -> Result<WalScan> {
        self.scan_from(None)
    }

    /// Returns the last sequence in one WAL file, if the file has records.
    ///
    /// Used by snapshot pruning; old payloads are validated but not decoded.
    pub(crate) fn last_seq_in_file(&self, file_num: u64) -> Result<Option<u64>> {
        let path = self.wal_path(file_num);
        if !path.exists() {
            return Ok(None);
        }
        let mut previous_seq = None;
        let mut last_seq = 0u64;
        self.read_file(&path, Some(u64::MAX), &mut previous_seq, &mut last_seq)?;
        Ok(previous_seq.map(|_| last_seq))
    }

    /// Scans WAL files and returns only records after `min_seq`.
    ///
    /// CRC and sequence continuity are still validated for every record; old
    /// payloads are not decoded, which bounds recovery memory to the tail.
    pub fn scan_from(&self, min_seq: Option<u64>) -> Result<WalScan> {
        let mut records = Vec::new();
        let mut file_num = read_start_file(&self.data_dir)?;
        let mut previous_seq: Option<u64> = None;
        let mut last_seq = 0u64;

        loop {
            let path = self.wal_path(file_num);
            if !path.exists() {
                break;
            }
            let next_exists = self.wal_path(file_num + 1).exists();

            match self.read_file(&path, min_seq, &mut previous_seq, &mut last_seq)? {
                FileOutcome::Complete(file_records, valid_len) => {
                    records.extend(file_records);
                    let actual_len = std::fs::metadata(&path)?.len();
                    if valid_len < actual_len {
                        if next_exists {
                            return Err(PersistenceError::Corruption(format!(
                                "incomplete WAL record in non-final file {}",
                                path.display()
                            )));
                        }
                        return Ok(WalScan {
                            records,
                            last_seq,
                            truncated_tail: Some((path, valid_len)),
                        });
                    }
                    file_num += 1;
                }
                FileOutcome::Truncated(file_records, valid_len) => {
                    records.extend(file_records);
                    if next_exists {
                        return Err(PersistenceError::Corruption(format!(
                            "incomplete WAL record in non-final file {}",
                            path.display()
                        )));
                    }
                    return Ok(WalScan {
                        records,
                        last_seq,
                        truncated_tail: Some((path, valid_len)),
                    });
                }
            }
        }

        Ok(WalScan {
            records,
            last_seq,
            truncated_tail: None,
        })
    }

    fn read_file(
        &self,
        path: &Path,
        min_seq: Option<u64>,
        previous_seq: &mut Option<u64>,
        last_seq: &mut u64,
    ) -> Result<FileOutcome> {
        use std::io::{BufReader, Read};

        let file = File::open(path)?;
        let file_len = file.metadata()?.len();

        // No complete header means no record was ever flushed to this file.
        if file_len < HEADER_LEN {
            return Ok(FileOutcome::Truncated(Vec::new(), 0));
        }

        let mut reader = BufReader::new(file);
        let mut pos = HEADER_LEN;
        let mut records = Vec::new();

        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic)?;
        if u32::from_le_bytes(magic) != WAL_MAGIC {
            return Err(PersistenceError::Corruption(format!(
                "invalid WAL magic in {}",
                path.display()
            )));
        }

        let mut version = [0u8; 2];
        reader.read_exact(&mut version)?;
        if u16::from_le_bytes(version) != WAL_VERSION {
            return Err(PersistenceError::Corruption(format!(
                "unsupported WAL version {}",
                u16::from_le_bytes(version)
            )));
        }

        let mut flags = [0u8; 2];
        reader.read_exact(&mut flags)?;

        loop {
            let mut len_buf = [0u8; 4];
            match reader.read_exact(&mut len_buf) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    // Bytes after the last complete record mean a torn write.
                    return if file_len > pos {
                        Ok(FileOutcome::Truncated(records, pos))
                    } else {
                        Ok(FileOutcome::Complete(records, pos))
                    };
                }
                Err(error) => return Err(PersistenceError::Io(error)),
            }
            let len = u32::from_le_bytes(len_buf) as u64;
            if len > MAX_RECORD_LEN as u64 {
                return Err(PersistenceError::Corruption(format!(
                    "WAL record length {len} exceeds maximum {MAX_RECORD_LEN}"
                )));
            }

            // From here on, EOF means a torn write, not a clean file end.
            let mut seq_buf = [0u8; 8];
            match reader.read_exact(&mut seq_buf) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(FileOutcome::Truncated(records, pos));
                }
                Err(error) => return Err(PersistenceError::Io(error)),
            }
            let seq = u64::from_le_bytes(seq_buf);

            let mut ts_buf = [0u8; 8];
            match reader.read_exact(&mut ts_buf) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(FileOutcome::Truncated(records, pos));
                }
                Err(error) => return Err(PersistenceError::Io(error)),
            }
            let timestamp = u64::from_le_bytes(ts_buf);

            let mut crc_buf = [0u8; 4];
            match reader.read_exact(&mut crc_buf) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(FileOutcome::Truncated(records, pos));
                }
                Err(error) => return Err(PersistenceError::Io(error)),
            }
            let expected_crc = u32::from_le_bytes(crc_buf);

            let mut payload = vec![0u8; len as usize];
            match reader.read_exact(&mut payload) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(FileOutcome::Truncated(records, pos));
                }
                Err(error) => return Err(PersistenceError::Io(error)),
            }

            let actual_crc = crc32fast::hash(&payload);
            if actual_crc != expected_crc {
                return Err(PersistenceError::Corruption(format!(
                    "CRC32 mismatch in {}",
                    path.display()
                )));
            }

            if previous_seq.is_none()
                && let Some(min_seq) = min_seq
                && seq > min_seq
                && seq != min_seq + 1
            {
                return Err(PersistenceError::Corruption(format!(
                    "WAL tail starts at {seq}, expected {}",
                    min_seq + 1
                )));
            }
            if let Some(previous) = *previous_seq
                && seq != previous + 1
            {
                return Err(PersistenceError::Corruption(format!(
                    "WAL sequence gap: expected {}, found {seq}",
                    previous + 1
                )));
            }
            *previous_seq = Some(seq);
            *last_seq = seq;

            pos += RECORD_OVERHEAD + len;
            if min_seq.is_none_or(|min_seq| seq > min_seq) {
                let entry = decode::<WalEntry>(&payload)
                    .map_err(|error| PersistenceError::Deserialization(error.to_string()))?;
                records.push(WalRecord {
                    seq,
                    timestamp,
                    entry,
                });
            }
        }
    }

    fn wal_path(&self, file_num: u64) -> PathBuf {
        wal_path(&self.data_dir, file_num)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::protocol::{NewOrderRequest, OrderType};
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    fn order(user_id: u64, price: u64) -> NewOrderRequest {
        NewOrderRequest {
            user_id,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("BTCUSDT"),
            order_type: OrderType::Buy,
            price,
            quantity: 10,
        }
    }

    fn record(seq: u64, user_id: u64) -> WalRecord {
        WalRecord {
            seq,
            timestamp: seq,
            entry: WalEntry::NewOrder(order(user_id, 50_000 + user_id)),
        }
    }

    #[test]
    fn test_scan_from_returns_only_tail() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();
        {
            let mut writer = WalWriter::new(data_dir.clone(), false, 1024 * 1024).unwrap();
            for i in 0..10 {
                let seq = writer.next_seq();
                writer.write(&record(seq, i)).unwrap();
            }
            writer.flush().unwrap();
        }

        let reader = WalReader::new(data_dir);
        let scan = reader.scan_from(Some(4)).unwrap();
        assert_eq!(scan.records.len(), 5);
        assert_eq!(scan.records[0].seq, 5);
        assert_eq!(scan.last_seq, 9);
        assert!(reader.scan_from(Some(9)).unwrap().records.is_empty());
    }

    #[test]
    fn test_wal_write_read() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        {
            let mut writer = WalWriter::new(data_dir.clone(), false, 1024 * 1024).unwrap();
            for i in 0..100 {
                let seq = writer.next_seq();
                writer.write(&record(seq, i)).unwrap();
            }
            writer.flush().unwrap();
        }

        let reader = WalReader::new(data_dir);
        let records = reader.read_all().unwrap();
        assert_eq!(records.len(), 100);
        for (i, rec) in records.iter().enumerate() {
            assert_eq!(rec.seq, i as u64);
            assert_eq!(rec.timestamp, i as u64);
        }
    }

    #[test]
    fn test_wal_large_volume_recovery() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();
        const ENTRY_COUNT: u64 = 10_000;

        {
            let mut writer = WalWriter::new(data_dir.clone(), false, 1024 * 1024 * 1024).unwrap();
            for i in 0..ENTRY_COUNT {
                let seq = writer.next_seq();
                writer.write(&record(seq, i)).unwrap();
            }
            writer.flush().unwrap();
        }

        let reader = WalReader::new(data_dir);
        let records = reader.read_all().unwrap();
        assert_eq!(records.len(), ENTRY_COUNT as usize);
        for (i, rec) in records.iter().enumerate() {
            assert_eq!(rec.seq, i as u64);
        }
    }

    #[test]
    fn test_wal_corruption_detection() {
        use std::io::{Seek, SeekFrom, Write as IoWrite};

        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        {
            let mut writer = WalWriter::new(data_dir.clone(), false, 1024 * 1024).unwrap();
            for i in 0..10 {
                let seq = writer.next_seq();
                writer.write(&record(seq, i)).unwrap();
            }
            writer.flush().unwrap();
        }

        // Corrupt a payload byte inside the first record.
        {
            let wal_path = data_dir.join("wal.000000.log");
            let mut file = OpenOptions::new().write(true).open(&wal_path).unwrap();
            file.seek(SeekFrom::Start(HEADER_LEN + RECORD_OVERHEAD + 3))
                .unwrap();
            file.write_all(&[0xFF]).unwrap();
            file.flush().unwrap();
        }

        let reader = WalReader::new(data_dir);
        let error = reader.read_all().unwrap_err();
        assert!(matches!(error, PersistenceError::Corruption(_)));
    }

    #[test]
    fn test_wal_concurrent_writes() {
        use std::thread;

        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();
        const THREADS: usize = 10;
        const ENTRIES_PER_THREAD: u64 = 100;

        let writer = Arc::new(Mutex::new(
            WalWriter::new(data_dir.clone(), false, 1024 * 1024 * 1024).unwrap(),
        ));

        let mut handles = vec![];
        for thread_id in 0..THREADS {
            let writer = Arc::clone(&writer);
            handles.push(thread::spawn(move || {
                for i in 0..ENTRIES_PER_THREAD {
                    let mut writer = writer.lock().unwrap();
                    let seq = writer.next_seq();
                    writer
                        .write(&record(seq, (thread_id as u64) * 1000 + i))
                        .unwrap();
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }
        writer.lock().unwrap().flush().unwrap();

        let reader = WalReader::new(data_dir);
        let records = reader.read_all().unwrap();
        assert_eq!(records.len(), THREADS * ENTRIES_PER_THREAD as usize);

        let mut seqs: Vec<u64> = records.iter().map(|record| record.seq).collect();
        seqs.sort_unstable();
        for (i, seq) in seqs.iter().enumerate() {
            assert_eq!(*seq, i as u64);
        }
    }

    #[test]
    fn test_wal_file_rotation() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();
        const MAX_FILE_SIZE: usize = 1024;
        const ENTRY_COUNT: u64 = 100;

        {
            let mut writer = WalWriter::new(data_dir.clone(), false, MAX_FILE_SIZE).unwrap();
            for i in 0..ENTRY_COUNT {
                let seq = writer.next_seq();
                writer.write(&record(seq, i)).unwrap();
            }
            writer.flush().unwrap();
        }

        let mut file_count = 0;
        let mut file_num = 0u64;
        loop {
            let path = data_dir.join(format!("wal.{file_num:06}.log"));
            if !path.exists() {
                break;
            }
            file_count += 1;
            file_num += 1;
        }
        assert!(file_count > 1);

        let reader = WalReader::new(data_dir);
        let records = reader.read_all().unwrap();
        assert_eq!(records.len(), ENTRY_COUNT as usize);
        for (i, rec) in records.iter().enumerate() {
            assert_eq!(rec.seq, i as u64);
        }
    }

    #[test]
    fn test_wal_fsync_durability() {
        use std::time::Instant;

        let temp_dir = TempDir::new().unwrap();
        const ENTRY_COUNT: u64 = 50;

        let sync_time = {
            let start = Instant::now();
            let mut writer =
                WalWriter::new(temp_dir.path().join("sync"), true, 1024 * 1024).unwrap();
            for i in 0..ENTRY_COUNT {
                let seq = writer.next_seq();
                writer.write(&record(seq, i)).unwrap();
            }
            writer.flush().unwrap();
            start.elapsed()
        };

        let no_sync_time = {
            let start = Instant::now();
            let mut writer =
                WalWriter::new(temp_dir.path().join("no_sync"), false, 1024 * 1024).unwrap();
            for i in 0..ENTRY_COUNT {
                let seq = writer.next_seq();
                writer.write(&record(seq, i)).unwrap();
            }
            writer.flush().unwrap();
            start.elapsed()
        };

        let sync_reader = WalReader::new(temp_dir.path().join("sync"));
        assert_eq!(sync_reader.read_all().unwrap().len(), ENTRY_COUNT as usize);
        let no_sync_reader = WalReader::new(temp_dir.path().join("no_sync"));
        assert_eq!(
            no_sync_reader.read_all().unwrap().len(),
            ENTRY_COUNT as usize
        );

        println!("WAL fsync: sync={sync_time:?}, no_sync={no_sync_time:?}");
    }

    #[test]
    fn test_corrupted_record_length_is_rejected() {
        use std::io::{Seek, SeekFrom, Write as IoWrite};

        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();
        {
            let mut writer = WalWriter::new(data_dir.clone(), false, 1024 * 1024).unwrap();
            let seq = writer.next_seq();
            writer.write(&record(seq, 1)).unwrap();
            writer.flush().unwrap();
        }

        {
            let wal_path = data_dir.join("wal.000000.log");
            let mut file = OpenOptions::new().write(true).open(&wal_path).unwrap();
            file.seek(SeekFrom::Start(HEADER_LEN)).unwrap();
            file.write_all(&u32::MAX.to_le_bytes()).unwrap();
            file.flush().unwrap();
        }

        let reader = WalReader::new(data_dir);
        let error = reader.read_all().unwrap_err();
        assert!(matches!(error, PersistenceError::Corruption(_)));
    }

    #[test]
    fn test_incomplete_header_is_repaired() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        // Simulate a crash before the header was flushed.
        std::fs::write(data_dir.join("wal.000000.log"), b"WA").unwrap();

        let mut writer = WalWriter::new(data_dir.clone(), false, 1024 * 1024).unwrap();
        let seq = writer.next_seq();
        writer.write(&record(seq, 1)).unwrap();
        writer.flush().unwrap();

        let reader = WalReader::new(data_dir);
        assert_eq!(reader.read_all().unwrap().len(), 1);
    }
}
