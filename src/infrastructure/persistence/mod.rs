pub mod async_wal;
pub mod recovery;
pub mod snapshot;
pub mod types;
/// 持久化基础设施模块
///
/// 提供完整的持久化和灾难恢复能力：
/// - WAL (Write-Ahead Log): 预写日志，记录所有订单操作
/// - Snapshot: 定期快照，保存完整订单簿状态
/// - Recovery: 从快照和WAL恢复系统状态
///
/// ## 设计目标
/// - **持久性**: 确保所有已确认的操作不会丢失
/// - **性能**: 最小化对撮合延迟的影响（<1μs overhead）
/// - **可靠性**: 支持崩溃恢复和数据完整性验证
/// - **效率**: 使用二进制格式和批量刷盘
pub mod wal;

pub use async_wal::AsyncWalWriter;
pub use recovery::RecoveryManager;
pub use snapshot::{SnapshotReader, SnapshotWriter};
pub use types::{PersistenceConfig, SnapshotData, SnapshotMetadata, WalEntry, WalMetadata};
pub use wal::{WalReader, WalWriter};

use std::path::PathBuf;
use thiserror::Error;

/// 持久化错误类型
#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Deserialization error: {0}")]
    Deserialization(String),

    #[error("Corruption detected: {0}")]
    Corruption(String),

    #[error("File not found: {0}")]
    FileNotFound(PathBuf),

    #[error("Invalid WAL entry")]
    InvalidWalEntry,

    #[error("Snapshot error: {0}")]
    Snapshot(String),

    #[error("Lock poisoned: {0}")]
    LockPoisoned(String),
}

pub type Result<T> = std::result::Result<T, PersistenceError>;
