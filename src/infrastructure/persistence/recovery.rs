//! WAL recovery.
//!
//! Recovery reads every WAL file in order, verifies the CRC of every record,
//! and checks that sequence numbers are contiguous. The resulting records are
//! replayed by the application service to rebuild the order book. A torn tail in
//! the newest file (a crash during append) is trimmed; any other damage aborts
//! recovery.
use super::{Result, WalReader, WalRecord};
use std::path::PathBuf;

/// Replays a WAL to reconstruct application state.
pub struct RecoveryManager {
    wal_reader: WalReader,
}

/// Records recovered from the WAL.
#[derive(Debug)]
pub struct RecoveryResult {
    /// Complete records in write order.
    pub records: Vec<WalRecord>,
    /// Last sequence number found, or `0` for an empty WAL.
    pub last_seq: u64,
}

impl RecoveryManager {
    /// Creates a recovery manager for `data_dir`.
    pub fn new(data_dir: PathBuf) -> Result<Self> {
        Ok(Self {
            wal_reader: WalReader::new(data_dir),
        })
    }

    /// Reads and validates all WAL records.
    ///
    /// # Errors
    /// Returns an error if any record fails CRC validation, has invalid
    /// encoding, or if sequence numbers are not contiguous.
    #[cfg(test)]
    pub fn recover(&self) -> Result<RecoveryResult> {
        self.recover_after(None)
    }

    /// Recovers records after `min_seq`; older records are validated but not
    /// decoded or retained.
    ///
    /// # Errors
    /// Returns an error if any record fails CRC validation or sequence numbers
    /// are not contiguous.
    pub fn recover_after(&self, min_seq: Option<u64>) -> Result<RecoveryResult> {
        let scan = self.wal_reader.scan_from(min_seq)?;

        // A crash can leave a torn final record. No acknowledged write is lost
        // because it was never fully flushed; trim the partial bytes so future
        // appends stay parseable.
        if let Some((path, valid_len)) = &scan.truncated_tail {
            tracing::warn!(
                path = %path.display(),
                valid_len,
                "dropping incomplete WAL tail"
            );
            let file = std::fs::OpenOptions::new().write(true).open(path)?;
            file.set_len(*valid_len)?;
        }

        Ok(RecoveryResult {
            records: scan.records,
            // A snapshot may cover the entire WAL; the tail can be empty, but
            // the logical last sequence is still the snapshot point.
            last_seq: scan.last_seq.max(min_seq.unwrap_or(0)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::persistence::{PersistenceError, WalEntry, WalRecord, WalWriter};
    use crate::shared::protocol::{NewOrderRequest, OrderType};
    use tempfile::TempDir;

    fn record(seq: u64, price: u64) -> WalRecord {
        WalRecord {
            seq,
            timestamp: seq,
            entry: WalEntry::NewOrder(NewOrderRequest {
                user_id: seq,
                client_order_id: 0,
                symbol: crate::shared::SymbolId::intern("BTCUSDT"),
                order_type: OrderType::Buy,
                price,
                quantity: 10,
            }),
        }
    }

    fn write_records(dir: &TempDir, start_seq: u64, count: u64) {
        let mut writer = WalWriter::new(dir.path().to_path_buf(), false, 1024 * 1024).unwrap();
        writer = writer.with_seq(start_seq);
        for i in 0..count {
            let seq = writer.next_seq();
            writer.write(&record(seq, 50_000 + i)).unwrap();
        }
        writer.flush().unwrap();
    }

    #[test]
    fn test_recovery_from_scratch() {
        let temp_dir = TempDir::new().unwrap();
        write_records(&temp_dir, 0, 10);

        let recovery = RecoveryManager::new(temp_dir.path().to_path_buf()).unwrap();
        let result = recovery.recover().unwrap();

        assert_eq!(result.records.len(), 10);
        assert_eq!(result.last_seq, 9);
    }

    #[test]
    fn test_recovery_resume_after_crash() {
        let temp_dir = TempDir::new().unwrap();
        write_records(&temp_dir, 0, 500);

        let recovery = RecoveryManager::new(temp_dir.path().to_path_buf()).unwrap();
        let first = recovery.recover().unwrap();
        assert_eq!(first.records.len(), 500);
        assert_eq!(first.last_seq, 499);

        write_records(&temp_dir, first.last_seq + 1, 300);

        let result = recovery.recover().unwrap();
        assert_eq!(result.records.len(), 800);
        assert_eq!(result.last_seq, 799);
    }

    #[test]
    fn test_recovery_rejects_sequence_gap() {
        let temp_dir = TempDir::new().unwrap();
        let mut writer = WalWriter::new(temp_dir.path().to_path_buf(), false, 1024 * 1024).unwrap();

        writer.write(&record(0, 50_000)).unwrap();
        writer.write(&record(2, 50_001)).unwrap();
        writer.flush().unwrap();

        let recovery = RecoveryManager::new(temp_dir.path().to_path_buf()).unwrap();
        let error = recovery.recover().unwrap_err();
        assert!(matches!(error, PersistenceError::Corruption(_)));
    }

    #[test]
    fn test_recovery_drops_truncated_tail() {
        let temp_dir = TempDir::new().unwrap();
        write_records(&temp_dir, 0, 3);

        let path = temp_dir.path().join("wal.000000.log");
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        let len = file.metadata().unwrap().len();
        file.set_len(len - 3).unwrap();
        drop(file);

        let recovery = RecoveryManager::new(temp_dir.path().to_path_buf()).unwrap();
        let result = recovery.recover().unwrap();
        assert_eq!(result.records.len(), 2, "torn record is dropped");
        assert_eq!(result.last_seq, 1);

        // The file was repaired, so appending and recovering again works.
        write_records(&temp_dir, result.last_seq + 1, 1);
        let result = recovery.recover().unwrap();
        assert_eq!(result.records.len(), 3);
    }

    #[test]
    fn test_recovery_handles_empty_wal_file() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::File::create(temp_dir.path().join("wal.000000.log")).unwrap();

        let recovery = RecoveryManager::new(temp_dir.path().to_path_buf()).unwrap();
        let result = recovery.recover().unwrap();
        assert!(result.records.is_empty());
        assert_eq!(result.last_seq, 0);
    }

    #[test]
    fn test_recovery_with_result_entries() {
        let temp_dir = TempDir::new().unwrap();
        let mut writer = WalWriter::new(temp_dir.path().to_path_buf(), false, 1024 * 1024).unwrap();

        writer
            .write(&WalRecord {
                seq: writer.next_seq(),
                timestamp: 0,
                entry: WalEntry::NewOrder(NewOrderRequest {
                    user_id: 1,
                    client_order_id: 0,
                    symbol: crate::shared::SymbolId::intern("BTCUSDT"),
                    order_type: OrderType::Buy,
                    price: 50_000,
                    quantity: 10,
                }),
            })
            .unwrap();
        writer
            .write(&WalRecord {
                seq: writer.next_seq(),
                timestamp: 1,
                entry: WalEntry::Trade(crate::shared::protocol::TradeNotification {
                    trade_id: 1,
                    symbol: crate::shared::SymbolId::intern("BTCUSDT"),
                    matched_price: 50_000,
                    matched_quantity: 10,
                    buyer_user_id: 1,
                    buyer_order_id: 1,
                    seller_user_id: 2,
                    seller_order_id: 2,
                    timestamp: 1,
                }),
            })
            .unwrap();
        writer
            .write(&WalRecord {
                seq: writer.next_seq(),
                timestamp: 1,
                entry: WalEntry::Confirmation(crate::shared::protocol::OrderConfirmation {
                    order_id: 1,
                    user_id: 1,
                }),
            })
            .unwrap();
        writer.flush().unwrap();

        let recovery = RecoveryManager::new(temp_dir.path().to_path_buf()).unwrap();
        let result = recovery.recover().unwrap();
        assert_eq!(result.records.len(), 3);
        assert_eq!(result.last_seq, 2);
    }

    #[test]
    fn test_recovery_stress() {
        let temp_dir = TempDir::new().unwrap();
        const ENTRY_COUNT: u64 = 5_000;
        write_records(&temp_dir, 0, ENTRY_COUNT);

        let recovery = RecoveryManager::new(temp_dir.path().to_path_buf()).unwrap();
        let result = recovery.recover().unwrap();
        assert_eq!(result.records.len(), ENTRY_COUNT as usize);
        assert_eq!(result.last_seq, ENTRY_COUNT - 1);
    }
}
