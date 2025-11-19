/// 持久化相关的数据类型
use crate::shared::protocol::{
    CancelOrderRequest, NewOrderRequest, OrderConfirmation, TradeNotification,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// WAL日志条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WalEntry {
    /// 新订单
    NewOrder {
        /// 序列号（单调递增）
        seq: u64,
        /// 时间戳（纳秒）
        timestamp: u64,
        /// 订单请求
        order: NewOrderRequest,
    },
    /// 取消订单
    CancelOrder {
        seq: u64,
        timestamp: u64,
        cancel: CancelOrderRequest,
    },
    /// 成交记录
    Trade {
        seq: u64,
        timestamp: u64,
        trade: TradeNotification,
    },
    /// 订单确认
    Confirmation {
        seq: u64,
        timestamp: u64,
        confirmation: OrderConfirmation,
    },
    /// 检查点标记（用于快照）
    Checkpoint {
        seq: u64,
        timestamp: u64,
        snapshot_id: u64,
    },
}

impl WalEntry {
    /// 获取序列号
    #[must_use]
    pub fn seq(&self) -> u64 {
        match self {
            WalEntry::NewOrder { seq, .. }
            | WalEntry::CancelOrder { seq, .. }
            | WalEntry::Trade { seq, .. }
            | WalEntry::Confirmation { seq, .. }
            | WalEntry::Checkpoint { seq, .. } => *seq,
        }
    }

    /// 获取时间戳
    #[must_use]
    pub fn timestamp(&self) -> u64 {
        match self {
            WalEntry::NewOrder { timestamp, .. }
            | WalEntry::CancelOrder { timestamp, .. }
            | WalEntry::Trade { timestamp, .. }
            | WalEntry::Confirmation { timestamp, .. }
            | WalEntry::Checkpoint { timestamp, .. } => *timestamp,
        }
    }
}

/// 快照数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotData {
    /// 快照ID（时间戳）
    pub snapshot_id: u64,
    /// 对应的WAL序列号
    pub wal_seq: u64,
    /// 快照时间戳
    pub timestamp: u64,
    /// 订单簿状态（序列化的JSON或二进制）
    pub orderbook_state: Vec<u8>,
    /// 校验和（用于完整性验证）
    pub checksum: u64,
}

/// 持久化配置
#[derive(Debug, Clone)]
pub struct PersistenceConfig {
    /// 数据目录
    pub data_dir: PathBuf,

    /// WAL文件最大大小（字节）
    pub wal_max_size: usize,

    /// 是否同步刷盘（fsync）
    pub wal_sync: bool,

    /// 批量刷盘大小
    pub wal_batch_size: usize,

    /// 快照间隔（秒）
    pub snapshot_interval_secs: u64,

    /// 保留的快照数量
    pub snapshot_retention: usize,

    /// 是否压缩快照
    pub snapshot_compression: bool,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./data"),
            wal_max_size: 64 * 1024 * 1024, // 64MB
            wal_sync: true,
            wal_batch_size: 10, // ⚠️ 降低到10以减少数据丢失窗口（之前是1000）
            snapshot_interval_secs: 300, // 5分钟
            snapshot_retention: 3,
            snapshot_compression: true,
        }
    }
}

/// WAL元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalMetadata {
    /// 文件序号
    pub file_num: u64,
    /// 起始序列号
    pub start_seq: u64,
    /// 结束序列号
    pub end_seq: u64,
    /// 条目数量
    pub entry_count: u64,
}

/// 快照元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMetadata {
    /// 快照ID
    pub snapshot_id: u64,
    /// 对应的WAL序列号
    pub wal_seq: u64,
    /// 创建时间戳
    pub created_at: u64,
    /// 文件大小
    pub file_size: u64,
    /// 校验和
    pub checksum: u64,
}
