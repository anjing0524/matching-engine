/// 带持久化的匹配服务
///
/// 包装 TickBasedOrderBook 并添加持久化功能：
/// - WAL记录所有订单操作
/// - 定期创建快照
/// - 启动时自动恢复状态
use crate::domain::orderbook::{ContractSpec, TickBasedOrderBook};
use crate::infrastructure::persistence::snapshot::create_snapshot;
use crate::infrastructure::persistence::{
    PersistenceConfig, PersistenceError, RecoveryManager, SnapshotWriter, WalEntry, WalWriter,
};
use crate::shared::protocol::{NewOrderRequest, OrderConfirmation, TradeNotification};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};

/// 带持久化的匹配服务
pub struct PersistentMatchingService {
    /// 订单簿
    orderbook: TickBasedOrderBook,

    /// WAL写入器
    wal_writer: WalWriter,

    /// 快照写入器
    snapshot_writer: SnapshotWriter,

    /// 配置
    config: PersistenceConfig,

    /// 上次快照时间
    last_snapshot_time: SystemTime,

    /// 自上次快照以来的操作数
    ops_since_snapshot: u64,
}

impl PersistentMatchingService {
    /// 创建新的持久化匹配服务
    pub fn new(spec: ContractSpec, config: PersistenceConfig) -> Result<Self, PersistenceError> {
        // 创建WAL写入器
        let wal_writer = WalWriter::new(
            config.data_dir.clone(),
            config.wal_sync,
            config.wal_batch_size,
            config.wal_max_size,
        )?;

        // 创建快照写入器
        let snapshot_writer = SnapshotWriter::new(
            config.data_dir.clone(),
            config.snapshot_compression,
            config.snapshot_retention,
        )?;

        // 创建订单簿
        let orderbook = TickBasedOrderBook::new(spec);

        Ok(Self {
            orderbook,
            wal_writer,
            snapshot_writer,
            config,
            last_snapshot_time: SystemTime::now(),
            ops_since_snapshot: 0,
        })
    }

    /// 从持久化状态恢复
    pub fn recover(
        spec: ContractSpec,
        config: PersistenceConfig,
    ) -> Result<Self, PersistenceError> {
        info!("开始从持久化状态恢复...");

        // 创建恢复管理器
        let recovery = RecoveryManager::new(config.data_dir.clone())?;

        // 执行恢复
        let result = recovery.recover()?;

        info!(
            "恢复完成: last_seq={}, entries={}",
            result.last_seq, result.entry_count
        );

        // 创建订单簿
        let mut orderbook = TickBasedOrderBook::new(spec.clone());

        // 如果有快照，加载快照数据
        if let Some(snapshot) = result.snapshot {
            info!("从快照恢复: snapshot_id={}", snapshot.snapshot_id);
            // TODO: 实现从快照数据恢复订单簿状态
            // 这需要订单簿支持序列化/反序列化
            warn!("快照恢复尚未实现，将从WAL重放所有操作");
        }

        // 重放WAL条目
        info!("重放 {} 条WAL记录...", result.wal_entries.len());
        for entry in result.wal_entries {
            match entry {
                WalEntry::NewOrder { order, .. } => {
                    // 重放订单（不记录到WAL）
                    let _ = orderbook.match_order(order);
                }
                WalEntry::CancelOrder { .. } => {
                    // TODO: 实现取消订单重放
                    warn!("取消订单重放尚未实现");
                }
                _ => {
                    // Trade和Confirmation是结果，不需要重放
                }
            }
        }

        info!("WAL重放完成");

        // 创建WAL写入器（从恢复的序列号继续）
        let wal_writer = WalWriter::new(
            config.data_dir.clone(),
            config.wal_sync,
            config.wal_batch_size,
            config.wal_max_size,
        )?
        .with_seq(result.last_seq + 1);

        // 创建快照写入器
        let snapshot_writer = SnapshotWriter::new(
            config.data_dir.clone(),
            config.snapshot_compression,
            config.snapshot_retention,
        )?;

        Ok(Self {
            orderbook,
            wal_writer,
            snapshot_writer,
            config,
            last_snapshot_time: SystemTime::now(),
            ops_since_snapshot: 0,
        })
    }

    /// 处理新订单（带持久化）
    pub fn match_order(
        &mut self,
        order: NewOrderRequest,
    ) -> Result<(Vec<TradeNotification>, OrderConfirmation), PersistenceError> {
        // 获取WAL序列号
        let seq = self.wal_writer.next_seq();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        // 写入WAL
        let wal_entry = WalEntry::NewOrder {
            seq,
            timestamp,
            order: order.clone(),
        };
        self.wal_writer.write(wal_entry)?;

        // 执行撮合
        let (trades_vec, confirmation_opt) = self.orderbook.match_order(order);

        // 将SmallVec转换为Vec
        let trades: Vec<TradeNotification> = trades_vec.into_iter().collect();

        // 记录成交和确认到WAL
        for trade in &trades {
            let trade_seq = self.wal_writer.next_seq();
            let trade_entry = WalEntry::Trade {
                seq: trade_seq,
                timestamp,
                trade: trade.clone(),
            };
            self.wal_writer.write(trade_entry)?;
        }

        // 确保我们有确认
        if let Some(confirmation) = confirmation_opt.clone() {
            let conf_seq = self.wal_writer.next_seq();
            let conf_entry = WalEntry::Confirmation {
                seq: conf_seq,
                timestamp,
                confirmation: confirmation.clone(),
            };
            self.wal_writer.write(conf_entry)?;
        }

        // 更新操作计数
        self.ops_since_snapshot += 1;

        // 检查是否需要创建快照
        self.check_snapshot()?;

        // 返回结果，如果confirmation为None，创建一个默认的
        let confirmation = confirmation_opt.unwrap_or(OrderConfirmation {
            user_id: 0,
            order_id: 0,
        });

        Ok((trades, confirmation))
    }

    /// 检查并创建快照
    fn check_snapshot(&mut self) -> Result<(), PersistenceError> {
        let elapsed = SystemTime::now()
            .duration_since(self.last_snapshot_time)
            .unwrap_or(Duration::from_secs(0));

        let should_snapshot = elapsed.as_secs() >= self.config.snapshot_interval_secs
            || self.ops_since_snapshot >= 10_000; // 或者每10_000个操作

        if should_snapshot {
            self.create_snapshot()?;
        }

        Ok(())
    }

    /// 创建快照
    fn create_snapshot(&mut self) -> Result<(), PersistenceError> {
        info!("创建快照...");

        let wal_seq = self.wal_writer.current_seq();

        // TODO: 序列化订单簿状态
        // 现在使用占位符数据
        let orderbook_state = format!("Placeholder snapshot at seq {wal_seq}").into_bytes();

        let snapshot = create_snapshot(wal_seq, orderbook_state);
        self.snapshot_writer.write(snapshot)?;

        self.last_snapshot_time = SystemTime::now();
        self.ops_since_snapshot = 0;

        info!("快照创建完成: wal_seq={}", wal_seq);

        Ok(())
    }

    /// 强制刷盘
    pub fn flush(&mut self) -> Result<(), PersistenceError> {
        self.wal_writer.flush()?;
        Ok(())
    }

    /// 获取订单簿引用
    #[must_use]
    pub fn orderbook(&self) -> &TickBasedOrderBook {
        &self.orderbook
    }

    /// 获取订单簿可变引用
    pub fn orderbook_mut(&mut self) -> &mut TickBasedOrderBook {
        &mut self.orderbook
    }
}

impl Drop for PersistentMatchingService {
    fn drop(&mut self) {
        // 确保所有数据都刷盘
        if let Err(e) = self.flush() {
            error!("Failed to flush WAL on drop: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::protocol::OrderType;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[test]
    fn test_persistent_service_basic() {
        let temp_dir = TempDir::new().unwrap();

        let spec = ContractSpec::new("BTCUSDT", 1, 40000, 70000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: false, // 测试时禁用fsync以加快速度
            ..Default::default()
        };

        // 创建服务
        let mut service = PersistentMatchingService::new(spec.clone(), config.clone()).unwrap();

        // 提交订单
        let order = NewOrderRequest {
            user_id: 1,
            symbol: Arc::from("BTCUSDT"),
            order_type: OrderType::Buy,
            price: 50000,
            quantity: 10,
        };

        let result = service.match_order(order);
        assert!(result.is_ok());

        // 刷盘
        service.flush().unwrap();

        drop(service);

        // 从持久化状态恢复
        let _recovered = PersistentMatchingService::recover(spec, config).unwrap();

        // 验证订单簿状态
        // TODO: 添加更多验证
    }
}
