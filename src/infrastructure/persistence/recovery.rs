/// 状态恢复（Recovery）实现
///
/// 从快照和WAL日志恢复系统状态，支持崩溃恢复。
///
/// ## 恢复流程
/// 1. 查找最新的有效快照
/// 2. 加载快照数据
/// 3. 从快照对应的WAL序列号开始重放日志
/// 4. 验证数据完整性
/// 5. 恢复完成

use super::{
    Result, SnapshotData, SnapshotReader, WalEntry, WalReader,
};
use std::path::PathBuf;
use tracing::{info, warn};

/// 恢复管理器
pub struct RecoveryManager {
    /// 数据目录
    data_dir: PathBuf,
    /// WAL读取器
    wal_reader: WalReader,
    /// 快照读取器
    snapshot_reader: SnapshotReader,
}

/// 恢复结果
#[derive(Debug)]
pub struct RecoveryResult {
    /// 快照数据（如果有）
    pub snapshot: Option<SnapshotData>,
    /// 需要重放的WAL条目
    pub wal_entries: Vec<WalEntry>,
    /// 恢复的最后序列号
    pub last_seq: u64,
    /// 恢复的条目总数
    pub entry_count: usize,
}

impl RecoveryManager {
    /// 创建新的恢复管理器
    pub fn new(data_dir: PathBuf) -> Result<Self> {
        let wal_reader = WalReader::new(data_dir.clone());
        let snapshot_reader = SnapshotReader::new(data_dir.clone());

        Ok(Self {
            data_dir,
            wal_reader,
            snapshot_reader,
        })
    }

    /// 执行完整恢复
    pub fn recover(&self) -> Result<RecoveryResult> {
        info!("开始状态恢复...");

        // 1. 加载最新快照
        let snapshot = self.snapshot_reader.read_latest()?;

        match &snapshot {
            Some(snap) => {
                info!(
                    "找到快照: snapshot_id={}, wal_seq={}",
                    snap.snapshot_id, snap.wal_seq
                );
            }
            None => {
                info!("未找到快照，将从头开始恢复");
            }
        }

        // 2. 读取所有WAL条目
        let all_wal_entries = self.wal_reader.read_all()?;
        info!("读取WAL条目: {} 条", all_wal_entries.len());

        // 3. 过滤需要重放的WAL条目
        let wal_entries = match &snapshot {
            Some(snap) => {
                // 只重放快照之后的条目
                all_wal_entries
                    .into_iter()
                    .filter(|entry| entry.seq() > snap.wal_seq)
                    .collect()
            }
            None => all_wal_entries,
        };

        info!("需要重放的WAL条目: {} 条", wal_entries.len());

        // 4. 计算最后序列号
        let last_seq = wal_entries
            .last()
            .map(|e| e.seq())
            .or_else(|| snapshot.as_ref().map(|s| s.wal_seq))
            .unwrap_or(0);

        let entry_count = wal_entries.len();

        info!("状态恢复完成: last_seq={}, entry_count={}", last_seq, entry_count);

        Ok(RecoveryResult {
            snapshot,
            wal_entries,
            last_seq,
            entry_count,
        })
    }

    /// 验证数据完整性
    pub fn verify(&self) -> Result<bool> {
        info!("验证数据完整性...");

        // 验证快照
        if let Some(snapshot) = self.snapshot_reader.read_latest()? {
            let checksum = crc32fast::hash(&snapshot.orderbook_state) as u64;
            if checksum != snapshot.checksum {
                warn!("快照校验和不匹配");
                return Ok(false);
            }
        }

        // 验证WAL（读取过程中已验证CRC32）
        let wal_entries = self.wal_reader.read_all()?;

        // 检查序列号连续性
        for (i, entry) in wal_entries.iter().enumerate() {
            if i > 0 && entry.seq() != wal_entries[i - 1].seq() + 1 {
                warn!(
                    "WAL序列号不连续: {} -> {}",
                    wal_entries[i - 1].seq(),
                    entry.seq()
                );
                // 序列号可能不连续（多分区情况下），这不一定是错误
            }
        }

        info!("数据完整性验证通过");
        Ok(true)
    }

    /// 清理所有持久化数据
    pub fn cleanup_all(&self) -> Result<()> {
        warn!("清理所有持久化数据...");

        // 删除数据目录
        if self.data_dir.exists() {
            std::fs::remove_dir_all(&self.data_dir)?;
        }

        info!("清理完成");
        Ok(())
    }
}

/// 恢复策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryStrategy {
    /// 快速恢复：仅使用快照
    Fast,
    /// 完整恢复：快照 + WAL重放
    Full,
    /// 验证恢复：完整恢复 + 数据验证
    Verified,
}

impl Default for RecoveryStrategy {
    fn default() -> Self {
        RecoveryStrategy::Full
    }
}

/// 恢复选项
#[derive(Debug, Clone)]
pub struct RecoveryOptions {
    /// 恢复策略
    pub strategy: RecoveryStrategy,
    /// 是否在恢复失败时跳过损坏的条目
    pub skip_corrupted: bool,
    /// 最大重试次数
    pub max_retries: usize,
}

impl Default for RecoveryOptions {
    fn default() -> Self {
        Self {
            strategy: RecoveryStrategy::Full,
            skip_corrupted: false,
            max_retries: 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::persistence::{
        snapshot::create_snapshot, wal::WalWriter, WalEntry,
    };
    use crate::shared::protocol::{NewOrderRequest, OrderType};
    use std::sync::Arc;
    use tempfile::TempDir;

    #[test]
    fn test_recovery_from_scratch() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        // 写入WAL
        {
            let mut writer =
                WalWriter::new(data_dir.clone(), false, 10, 1024 * 1024).unwrap();

            for i in 0..10 {
                let seq = writer.next_seq();
                let entry = WalEntry::NewOrder {
                    seq,
                    timestamp: i,
                    order: NewOrderRequest {
                        user_id: i,
                        symbol: Arc::from("BTCUSDT"),
                        order_type: OrderType::Buy,
                        price: 50000 + i,
                        quantity: 10,
                    },
                };
                writer.write(entry).unwrap();
            }
            writer.flush().unwrap();
        }

        // 恢复
        {
            let recovery = RecoveryManager::new(data_dir).unwrap();
            let result = recovery.recover().unwrap();

            assert!(result.snapshot.is_none());
            assert_eq!(result.wal_entries.len(), 10);
            assert_eq!(result.last_seq, 9);
        }
    }

    #[test]
    fn test_recovery_from_snapshot() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        // 写入WAL
        {
            let mut writer =
                WalWriter::new(data_dir.clone(), false, 10, 1024 * 1024).unwrap();

            for i in 0..20 {
                let seq = writer.next_seq();
                let entry = WalEntry::NewOrder {
                    seq,
                    timestamp: i,
                    order: NewOrderRequest {
                        user_id: i,
                        symbol: Arc::from("BTCUSDT"),
                        order_type: OrderType::Buy,
                        price: 50000 + i,
                        quantity: 10,
                    },
                };
                writer.write(entry).unwrap();
            }
            writer.flush().unwrap();
        }

        // 创建快照（在序列号10处）
        {
            use crate::infrastructure::persistence::snapshot::SnapshotWriter;

            let snapshot = create_snapshot(10, b"snapshot at seq 10".to_vec());
            let writer = SnapshotWriter::new(data_dir.clone(), false, 3).unwrap();
            writer.write(snapshot).unwrap();
        }

        // 恢复
        {
            let recovery = RecoveryManager::new(data_dir).unwrap();
            let result = recovery.recover().unwrap();

            assert!(result.snapshot.is_some());
            assert_eq!(result.snapshot.as_ref().unwrap().wal_seq, 10);
            // 应该只重放序列号 > 10 的条目
            assert_eq!(result.wal_entries.len(), 9); // 11-19
            assert_eq!(result.last_seq, 19);
        }
    }
}
