//! Append-only command log segments for replay.
//!
//! Layout: 16-byte header (`EXLOG1\0`, version u32) followed by fixed 256-byte
//! command slots. The log is the authoritative input for replay after a core
//! snapshot.

use crate::trading::protocol::{COMMAND_SLOT_SIZE, Command};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"EXLOG1\0\0";
const HEADER_SIZE: usize = 16;
const VERSION: u32 = 1;

pub struct CommandLogWriter {
    file: File,
    path: PathBuf,
}

impl CommandLogWriter {
    /// Creates a new log segment.
    ///
    /// # Errors
    /// Returns an I/O error if the file cannot be created.
    pub fn create(path: &Path) -> io::Result<Self> {
        let mut file = OpenOptions::new()
            .create(false)
            .write(true)
            .open(path)
            .or_else(|_| OpenOptions::new().create_new(true).write(true).open(path))?;
        file.set_len(0)?;
        file.write_all(MAGIC)?;
        file.write_all(&VERSION.to_le_bytes())?;
        file.write_all(&0u32.to_le_bytes())?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    /// Appends one command slot.
    ///
    /// # Errors
    /// Returns an I/O error from the write.
    pub fn append(&mut self, command: &Command) -> io::Result<()> {
        self.file.write_all(&command.encode())
    }

    /// Flushes the segment to stable storage.
    ///
    /// # Errors
    /// Returns an I/O error from `fsync`.
    pub fn sync(&mut self) -> io::Result<()> {
        self.file.sync_data()
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Reads a complete command segment.
///
/// # Errors
/// Returns an I/O error for invalid magic/version, trailing bytes, or decode
/// failures.
pub fn read_segment(path: &Path) -> io::Result<Vec<Command>> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if bytes.len() < HEADER_SIZE {
        return Err(invalid("short command log header"));
    }
    if &bytes[0..8] != MAGIC {
        return Err(invalid("invalid command log magic"));
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().expect("4 bytes"));
    if version != VERSION {
        return Err(invalid("unsupported command log version"));
    }
    let payload = &bytes[HEADER_SIZE..];
    if !payload.len().is_multiple_of(COMMAND_SLOT_SIZE) {
        return Err(invalid("truncated command slot"));
    }
    let mut commands = Vec::with_capacity(payload.len() / COMMAND_SLOT_SIZE);
    for slot in payload.as_chunks::<COMMAND_SLOT_SIZE>().0 {
        commands.push(Command::decode(slot).map_err(|error| invalid(&format!("{error:?}")))?);
    }
    Ok(commands)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// Rotating command-log segments with bounded retention.
pub struct RotatingCommandLog {
    directory: PathBuf,
    prefix: String,
    max_segment_bytes: u64,
    retain_segments: usize,
    next_segment: u64,
    current_bytes: u64,
    writer: Option<CommandLogWriter>,
}

impl RotatingCommandLog {
    /// Creates a rotating log rooted at `directory`.
    ///
    /// # Errors
    /// Returns an I/O error if the directory cannot be created.
    pub fn open(
        directory: &Path,
        prefix: &str,
        max_segment_bytes: u64,
        retain_segments: usize,
    ) -> io::Result<Self> {
        std::fs::create_dir_all(directory)?;
        Ok(Self {
            directory: directory.to_path_buf(),
            prefix: prefix.to_string(),
            max_segment_bytes: max_segment_bytes.max(HEADER_SIZE as u64 + COMMAND_SLOT_SIZE as u64),
            retain_segments: retain_segments.max(1),
            next_segment: 1,
            current_bytes: 0,
            writer: None,
        })
    }

    /// Appends a command, rotating before the slot would exceed the size limit.
    ///
    /// # Errors
    /// Returns an I/O error from segment creation/write/sync.
    pub fn append(&mut self, command: &Command) -> io::Result<()> {
        let slot = COMMAND_SLOT_SIZE as u64;
        if self.writer.is_none() || self.current_bytes.saturating_add(slot) > self.max_segment_bytes
        {
            self.rotate()?;
        }
        if let Some(writer) = &mut self.writer {
            writer.append(command)?;
            writer.sync()?;
            self.current_bytes = self.current_bytes.saturating_add(slot);
        }
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        let name = format!("{}-{:08}.log", self.prefix, self.next_segment);
        self.next_segment += 1;
        let path = self.directory.join(name);
        self.writer = Some(CommandLogWriter::create(&path)?);
        self.current_bytes = HEADER_SIZE as u64;
        self.prune()?;
        Ok(())
    }

    fn prune(&self) -> io::Result<()> {
        let mut segments = self.segments()?;
        segments.sort();
        while segments.len() > self.retain_segments {
            let oldest = segments.remove(0);
            std::fs::remove_file(oldest)?;
        }
        Ok(())
    }

    /// Existing segment paths, oldest first.
    ///
    /// # Errors
    /// Returns an I/O error if the directory cannot be read.
    pub fn segments(&self) -> io::Result<Vec<PathBuf>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trading::protocol::Command;

    fn command(seq: u64) -> Command {
        let mut command = Command::zeroed();
        command.global_seq = seq;
        command.client_order_id = seq;
        command
    }

    #[test]
    fn writes_and_reads_segment() {
        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-command-log-{}.log",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut writer = CommandLogWriter::create(&path).unwrap();
        for seq in 1..=3 {
            writer.append(&command(seq)).unwrap();
        }
        writer.sync().unwrap();
        let commands = read_segment(&path).unwrap();
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[2].global_seq, 3);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rotating_log_bounds_segments() {
        let directory = std::env::temp_dir().join(format!(
            "trading-system-engine-rotating-log-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let mut log = RotatingCommandLog::open(&directory, "commands", 16 + 256 * 2, 2).unwrap();
        for seq in 1..=5 {
            log.append(&command(seq)).unwrap();
        }
        let segments = log.segments().unwrap();
        assert_eq!(segments.len(), 2);
        let total: usize = segments
            .iter()
            .map(|path| read_segment(path).unwrap().len())
            .sum();
        assert!((1..=5).contains(&total));
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn rejects_truncated_slot() {
        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-command-log-truncated-{}.log",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut writer = CommandLogWriter::create(&path).unwrap();
        writer.append(&command(1)).unwrap();
        writer.sync().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.pop();
        std::fs::write(&path, bytes).unwrap();
        assert!(read_segment(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
