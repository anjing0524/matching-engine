//! Order journal: the durability seam used by the matching service.
//!
//! The journal owns WAL sequencing, record construction, append framing, and
//! flush policy. Callers provide only a [`WalEntry`] and timestamp; they never
//! see sequence numbers, record framing, rotation, or recovery mechanics.
use super::{
    PersistenceConfig, RecoveryManager, Result, WalEntry, WalReader, WalRecord, WalWriter,
    read_start_file, wal_path, write_start_file,
};
use std::fs::File;
use std::path::Path;

/// Recovered journal contents.
#[derive(Debug)]
pub struct JournalRecovery {
    /// Complete records in write order.
    pub records: Vec<WalRecord>,
    /// Last sequence number found, or `0` for an empty journal.
    pub last_seq: u64,
}

/// Append-only order journal.
pub struct Journal {
    writer: WalWriter,
}

impl Journal {
    /// Opens a fresh journal (used when the data directory has no prior WAL).
    ///
    /// # Errors
    /// Returns an error if the WAL file cannot be created.
    pub fn create(config: &PersistenceConfig) -> Result<Self> {
        Ok(Self {
            writer: WalWriter::new(
                config.data_dir.clone(),
                config.wal_sync,
                config.wal_max_size,
            )?,
        })
    }

    /// Recovers records and opens the journal for appending at the next sequence.
    ///
    /// # Errors
    /// Returns an error if the WAL is corrupted or has a sequence gap.
    pub fn recover(config: &PersistenceConfig) -> Result<(Self, JournalRecovery)> {
        Self::recover_after(config, None)
    }

    /// Recovers records after `min_seq` and opens the journal for appending.
    ///
    /// # Errors
    /// Returns an error if the WAL is corrupted or has a sequence gap.
    pub fn recover_after(
        config: &PersistenceConfig,
        min_seq: Option<u64>,
    ) -> Result<(Self, JournalRecovery)> {
        let recovery = RecoveryManager::new(config.data_dir.clone())?.recover_after(min_seq)?;
        let writer = WalWriter::new(
            config.data_dir.clone(),
            config.wal_sync,
            config.wal_max_size,
        )?
        .with_seq(recovery.last_seq + 1);
        Ok((
            Self { writer },
            JournalRecovery {
                records: recovery.records,
                last_seq: recovery.last_seq,
            },
        ))
    }

    /// Appends one entry, assigning its sequence number.
    ///
    /// Acknowledging the caller is still the service's responsibility and must
    /// happen only after [`Journal::commit`].
    ///
    /// # Errors
    /// Returns an error if the record cannot be written.
    pub fn append(&mut self, entry: WalEntry, timestamp: u64) -> Result<u64> {
        let seq = self.writer.next_seq();
        self.writer.write(&WalRecord {
            seq,
            timestamp,
            entry,
        })?;
        Ok(seq)
    }

    /// Appends an already-sequenced record (used by a decoupled journaler).
    pub fn append_record(&mut self, record: &WalRecord) -> Result<u64> {
        let written = self.writer.write(record)?;
        self.writer.observe_seq(written);
        Ok(written)
    }

    /// Returns the next sequence number that will be assigned.
    ///
    /// The last durable sequence is `current_seq() - 1`.
    #[must_use]
    pub fn current_seq(&self) -> u64 {
        self.writer.current_seq()
    }

    /// Rotates the active WAL file.
    ///
    /// # Errors
    /// Returns an error if the new file cannot be created/fsynced.
    pub fn rotate(&mut self) -> Result<()> {
        self.writer.rotate_now()
    }

    /// Deletes fully-covered closed WAL files and advances the live-file
    /// manifest before unlinking them.
    ///
    /// The active file is left alone. The manifest is written first so a crash
    /// between the two steps can only leave an ignored file, never make
    /// recovery skip a live one.
    ///
    /// # Errors
    /// Returns an error if an old file cannot be scanned or removed.
    pub fn prune_through(&mut self, snapshot_seq: u64) -> Result<()> {
        let active = self.writer.active_file_num();
        let data_dir = self.writer.data_dir().to_path_buf();
        let reader = WalReader::new(data_dir.clone());
        let start = read_start_file(&data_dir)?;
        let mut new_start = start;
        let mut remove = Vec::new();

        for file_num in start..active {
            match reader.last_seq_in_file(file_num)? {
                Some(last_seq) if last_seq > snapshot_seq => break,
                _ => {
                    remove.push(file_num);
                    new_start = file_num + 1;
                }
            }
        }
        if remove.is_empty() {
            return Ok(());
        }
        write_start_file(&data_dir, new_start)?;
        for file_num in remove {
            let path = wal_path(&data_dir, file_num);
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        fsync_dir(&data_dir)?;
        Ok(())
    }

    /// Flushes the append buffer (and fsyncs when configured).
    ///
    /// # Errors
    /// Returns an error if flush/fsync fails.
    /// Flushes buffered WAL bytes to the OS and returns a handle for a
    /// separate fsync thread. The caller must synchronize before acknowledging.
    pub(crate) fn flush_handle(&mut self) -> Result<File> {
        self.writer.flush_handle()
    }

    pub fn commit(&mut self) -> Result<()> {
        self.writer.flush()
    }
}

fn fsync_dir(dir: &Path) -> Result<()> {
    let file = std::fs::File::open(dir)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::persistence::PersistenceConfig;
    use crate::shared::protocol::CancelOrderRequest;
    use tempfile::TempDir;

    fn cancel(order_id: u64) -> WalEntry {
        WalEntry::CancelOrder(CancelOrderRequest {
            user_id: 1,
            order_id,
        })
    }

    fn config(dir: &TempDir) -> PersistenceConfig {
        PersistenceConfig {
            data_dir: dir.path().to_path_buf(),
            wal_sync: false,
            ..Default::default()
        }
    }

    #[test]
    fn test_append_assigns_contiguous_sequences_and_recovers() {
        let dir = TempDir::new().unwrap();
        let config = config(&dir);
        let mut journal = Journal::create(&config).unwrap();
        let first = journal.append(cancel(1), 1).unwrap();
        let second = journal.append(cancel(1), 2).unwrap();
        assert_eq!(first, 0);
        assert_eq!(second, 1);
        journal.commit().unwrap();
        drop(journal);

        let (_journal, recovery) = Journal::recover(&config).unwrap();
        assert_eq!(recovery.records.len(), 2);
        assert_eq!(recovery.records[0].seq, 0);
        assert_eq!(recovery.records[1].seq, 1);
        assert_eq!(recovery.last_seq, 1);
    }

    #[test]
    fn test_recovered_journal_appends_to_latest_wal_file() {
        let dir = TempDir::new().unwrap();
        let mut config = config(&dir);
        config.wal_max_size = 160;

        {
            let mut journal = Journal::create(&config).unwrap();
            for i in 0..20 {
                journal.append(cancel(i + 1), i).unwrap();
            }
            journal.commit().unwrap();
        }

        let (mut journal, recovery) = Journal::recover(&config).unwrap();
        assert_eq!(recovery.last_seq, 19);
        for i in 0..5 {
            journal.append(cancel(100 + i), 100 + i).unwrap();
        }
        journal.commit().unwrap();
        drop(journal);

        let (_journal, recovery) = Journal::recover(&config).unwrap();
        assert_eq!(recovery.last_seq, 24);
        assert_eq!(recovery.records.len(), 25);
        assert!(
            recovery
                .records
                .windows(2)
                .all(|pair| pair[1].seq == pair[0].seq + 1)
        );
    }

    #[test]
    fn test_pruning_old_wal_files_keeps_recovery_working() {
        let dir = TempDir::new().unwrap();
        let mut config = config(&dir);
        config.wal_max_size = 160;

        let mut journal = Journal::create(&config).unwrap();
        for i in 0..20 {
            journal.append(cancel(i + 1), i).unwrap();
        }
        journal.commit().unwrap();
        journal.prune_through(5).unwrap();
        drop(journal);

        let files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("wal."))
            .collect();
        assert!(files.len() > 1, "small max_file_size must rotate");
        let start = read_start_file(dir.path()).unwrap();
        assert!(start > 0, "pruning must advance the live-file manifest");
        assert!(
            !dir.path().join("wal.000000.log").exists(),
            "fully covered files must be deleted"
        );

        let (_journal, recovery) = Journal::recover_after(&config, Some(5)).unwrap();
        assert!(recovery.records.iter().all(|record| record.seq > 5));
        assert_eq!(recovery.last_seq, 19);
    }

    #[test]
    fn test_recovered_journal_continues_sequence() {
        let dir = TempDir::new().unwrap();
        let config = config(&dir);
        let mut journal = Journal::create(&config).unwrap();
        journal.append(cancel(1), 1).unwrap();
        journal.commit().unwrap();
        drop(journal);

        let (mut journal, recovery) = Journal::recover(&config).unwrap();
        assert_eq!(recovery.last_seq, 0);
        let next = journal.append(cancel(1), 2).unwrap();
        assert_eq!(next, 1);
        journal.commit().unwrap();
    }
}
