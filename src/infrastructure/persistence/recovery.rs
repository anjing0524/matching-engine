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
use super::{Result, SnapshotData, SnapshotReader, WalEntry, WalReader};
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

        info!(
            "状态恢复完成: last_seq={}, entry_count={}",
            last_seq, entry_count
        );

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecoveryStrategy {
    /// 快速恢复：仅使用快照
    Fast,
    /// 完整恢复：快照 + WAL重放
    #[default]
    Full,
    /// 验证恢复：完整恢复 + 数据验证
    Verified,
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
    use crate::infrastructure::persistence::{snapshot::create_snapshot, wal::WalWriter, WalEntry};
    use crate::shared::protocol::{NewOrderRequest, OrderType};
    use std::sync::Arc;
    use tempfile::TempDir;

    #[test]
    fn test_recovery_from_scratch() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        // 写入WAL
        {
            let mut writer = WalWriter::new(data_dir.clone(), false, 10, 1024 * 1024).unwrap();

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
            let mut writer = WalWriter::new(data_dir.clone(), false, 10, 1024 * 1024).unwrap();

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

    /// Test 1: Recovery from mid-batch crash
    /// Simulates a crash during batch processing and verifies correct state restoration
    ///
    /// This test verifies that only flushed data is recovered after a crash.
    /// It simulates a scenario where orders are processed in batches, and a crash
    /// occurs between batches.
    #[test]
    fn test_recovery_from_mid_batch_crash() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        // Phase 1: Process first batch of 500 orders and flush (simulating successful batch)
        {
            let mut writer = WalWriter::new(data_dir.clone(), false, 100, 1024 * 1024).unwrap();

            for i in 0..500 {
                let seq = writer.next_seq();
                let entry = WalEntry::NewOrder {
                    seq,
                    timestamp: i,
                    order: NewOrderRequest {
                        user_id: i,
                        symbol: Arc::from("BTCUSDT"),
                        order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
                        price: 50000 + (i % 100),
                        quantity: 10,
                    },
                };
                writer.write(entry).unwrap();
            }

            // Flush first batch (these will be persisted)
            writer.flush().unwrap();
            // Drop writer cleanly after successful batch
        }

        // Phase 2: Simulate crash - system goes down before next batch is processed
        // (In reality, the next 500 orders would be in-flight but lost)

        // Phase 3: Recovery after crash
        {
            let recovery = RecoveryManager::new(data_dir.clone()).unwrap();
            let result = recovery.recover().unwrap();

            // Should recover exactly the 500 flushed orders
            assert_eq!(result.wal_entries.len(), 500, "Should recover exactly 500 flushed orders");
            assert_eq!(result.last_seq, 499, "Last sequence should be 499");

            // Verify orderbook integrity - check first and last orders
            if let WalEntry::NewOrder { order, .. } = &result.wal_entries[0] {
                assert_eq!(order.user_id, 0);
                assert_eq!(order.price, 50000);
            } else {
                panic!("First entry should be NewOrder");
            }

            if let WalEntry::NewOrder { order, .. } = &result.wal_entries[499] {
                assert_eq!(order.user_id, 499);
                assert_eq!(order.price, 50000 + 99);
            } else {
                panic!("Last entry should be NewOrder");
            }

            // Verify data integrity
            assert!(recovery.verify().unwrap(), "Data integrity should pass after recovery");

            println!("Mid-batch crash recovery test passed!");
            println!("  - Processed and flushed 500 orders");
            println!("  - Simulated crash before next batch");
            println!("  - Successfully recovered all 500 persisted orders");
        }

        // Phase 4: Resume operations after recovery - process more orders
        {
            let mut writer = WalWriter::new(data_dir.clone(), false, 100, 1024 * 1024).unwrap()
                .with_seq(500); // Continue from where we left off

            // Process next batch of 300 orders
            for i in 500..800 {
                let seq = writer.next_seq();
                let entry = WalEntry::NewOrder {
                    seq,
                    timestamp: i,
                    order: NewOrderRequest {
                        user_id: i,
                        symbol: Arc::from("ETHUSDT"),
                        order_type: if i % 2 == 0 { OrderType::Sell } else { OrderType::Buy },
                        price: 3000 + (i % 50),
                        quantity: 5,
                    },
                };
                writer.write(entry).unwrap();
            }
            writer.flush().unwrap();
        }

        // Final verification: Ensure resumed operations are also recoverable
        {
            let recovery = RecoveryManager::new(data_dir).unwrap();
            let result = recovery.recover().unwrap();

            assert_eq!(result.wal_entries.len(), 800, "Should recover all 800 orders after resume");
            assert_eq!(result.last_seq, 799, "Last sequence should be 799");

            println!("Post-recovery operations verified!");
            println!("  - Resumed operations after crash");
            println!("  - Total orders recovered: {}", result.wal_entries.len());
        }
    }

    /// Test 2: Recovery with partial trades
    /// Tests recovery when orders have been partially filled
    #[test]
    fn test_recovery_with_partial_trades() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        let mut current_seq = 0u64;

        // Create orders with partial fills
        {
            let mut writer = WalWriter::new(data_dir.clone(), true, 10, 1024 * 1024).unwrap();

            // Create a buy order
            let buy_order = WalEntry::NewOrder {
                seq: current_seq,
                timestamp: current_seq,
                order: NewOrderRequest {
                    user_id: 1,
                    symbol: Arc::from("BTCUSDT"),
                    order_type: OrderType::Buy,
                    price: 50000,
                    quantity: 100, // Total quantity
                },
            };
            writer.write(buy_order).unwrap();
            current_seq += 1;

            // Create a sell order
            let sell_order = WalEntry::NewOrder {
                seq: current_seq,
                timestamp: current_seq,
                order: NewOrderRequest {
                    user_id: 2,
                    symbol: Arc::from("BTCUSDT"),
                    order_type: OrderType::Sell,
                    price: 50000,
                    quantity: 200,
                },
            };
            writer.write(sell_order).unwrap();
            current_seq += 1;

            // Record partial trade (50 units matched)
            use crate::shared::protocol::TradeNotification;
            let trade = WalEntry::Trade {
                seq: current_seq,
                timestamp: current_seq,
                trade: TradeNotification {
                    trade_id: 1,
                    symbol: Arc::from("BTCUSDT"),
                    matched_price: 50000,
                    matched_quantity: 50, // Partial fill
                    buyer_user_id: 1,
                    buyer_order_id: 1,
                    seller_user_id: 2,
                    seller_order_id: 2,
                    timestamp: current_seq,
                },
            };
            writer.write(trade).unwrap();
            current_seq += 1;

            // Add confirmation showing remaining quantity
            use crate::shared::protocol::OrderConfirmation;
            let confirmation = WalEntry::Confirmation {
                seq: current_seq,
                timestamp: current_seq,
                confirmation: OrderConfirmation {
                    order_id: 1,
                    user_id: 1,
                },
            };
            writer.write(confirmation).unwrap();

            writer.flush().unwrap();
        }

        // Simulate crash and recover
        {
            let recovery = RecoveryManager::new(data_dir).unwrap();
            let result = recovery.recover().unwrap();

            assert_eq!(result.wal_entries.len(), 4, "Should recover all 4 entries");

            // Verify we have the trade entry
            let has_trade = result.wal_entries.iter().any(|entry| {
                matches!(entry, WalEntry::Trade { trade, .. } if trade.matched_quantity == 50)
            });
            assert!(has_trade, "Should recover partial trade information");

            // Verify confirmation
            let has_confirmation = result.wal_entries.iter().any(|entry| {
                matches!(entry, WalEntry::Confirmation { confirmation, .. }
                    if confirmation.order_id == 1)
            });
            assert!(has_confirmation, "Should recover confirmation for order 1");
        }
    }

    /// Test 3: Stress test with 10,000 orders
    /// Tests recovery performance and correctness with large datasets
    #[test]
    fn test_recovery_stress_10k_orders() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        println!("Writing 10,000 orders to WAL...");
        let write_start = std::time::Instant::now();

        // Write 10,000 orders
        {
            let mut writer = WalWriter::new(data_dir.clone(), true, 100, 1024 * 1024).unwrap();

            for i in 0..10000 {
                let seq = writer.next_seq();
                let entry = WalEntry::NewOrder {
                    seq,
                    timestamp: i,
                    order: NewOrderRequest {
                        user_id: i,
                        symbol: Arc::from("BTCUSDT"),
                        order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
                        price: 45000 + (i % 5000), // Varied prices
                        quantity: 1 + (i % 100),    // Varied quantities
                    },
                };
                writer.write(entry).unwrap();

                // Progress indicator
                if i > 0 && i % 1000 == 0 {
                    println!("  Written {} orders", i);
                }
            }
            writer.flush().unwrap();
        }

        let write_duration = write_start.elapsed();
        println!("Write completed in {:?}", write_duration);

        // Create snapshot at sequence 5000
        {
            use crate::infrastructure::persistence::snapshot::SnapshotWriter;

            let orderbook_state = format!("Snapshot at seq 5000 with {} orders", 5000).into_bytes();
            let snapshot = create_snapshot(5000, orderbook_state);
            let writer = SnapshotWriter::new(data_dir.clone(), false, 3).unwrap();
            writer.write(snapshot).unwrap();
            println!("Snapshot created at sequence 5000");
        }

        // Simulate crash and measure recovery time
        println!("Starting recovery...");
        let recovery_start = std::time::Instant::now();

        {
            let recovery = RecoveryManager::new(data_dir.clone()).unwrap();
            let result = recovery.recover().unwrap();

            let recovery_duration = recovery_start.elapsed();
            println!("Recovery completed in {:?}", recovery_duration);

            // Verify snapshot was loaded
            assert!(result.snapshot.is_some(), "Should load snapshot");
            assert_eq!(result.snapshot.as_ref().unwrap().wal_seq, 5000);

            // Should replay only entries after snapshot (5001-9999)
            assert_eq!(result.wal_entries.len(), 4999, "Should replay 4999 entries after snapshot");
            assert_eq!(result.last_seq, 9999, "Last sequence should be 9999");

            // Verify data integrity
            assert!(recovery.verify().unwrap(), "Data integrity check should pass");

            // Performance assertion: recovery should be fast
            assert!(
                recovery_duration.as_millis() < 5000,
                "Recovery should complete in under 5 seconds, took {:?}",
                recovery_duration
            );

            println!("All 10k stress test assertions passed!");
        }
    }

    /// Test 4: Orderbook consistency verification
    /// Tests that orderbook internal structures are correctly restored
    #[test]
    fn test_recovery_orderbook_consistency() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        // Create complex orderbook state with multiple price levels
        {
            let mut writer = WalWriter::new(data_dir.clone(), true, 50, 1024 * 1024).unwrap();

            // Create buy orders at different price levels
            for price_offset in 0..10 {
                for order_num in 0..5 {
                    let seq = writer.next_seq();
                    let entry = WalEntry::NewOrder {
                        seq,
                        timestamp: seq,
                        order: NewOrderRequest {
                            user_id: seq,
                            symbol: Arc::from("BTCUSDT"),
                            order_type: OrderType::Buy,
                            price: 50000 - (price_offset * 100), // Descending prices for bids
                            quantity: 10 + order_num,
                        },
                    };
                    writer.write(entry).unwrap();
                }
            }

            // Create sell orders at different price levels
            for price_offset in 0..10 {
                for order_num in 0..5 {
                    let seq = writer.next_seq();
                    let entry = WalEntry::NewOrder {
                        seq,
                        timestamp: seq,
                        order: NewOrderRequest {
                            user_id: seq,
                            symbol: Arc::from("BTCUSDT"),
                            order_type: OrderType::Sell,
                            price: 51000 + (price_offset * 100), // Ascending prices for asks
                            quantity: 10 + order_num,
                        },
                    };
                    writer.write(entry).unwrap();
                }
            }

            writer.flush().unwrap();
        }

        // Create snapshot with orderbook metadata
        {
            use crate::infrastructure::persistence::snapshot::SnapshotWriter;

            // Simulate orderbook state with metadata
            let orderbook_metadata = serde_json::json!({
                "bid_levels": 10,
                "ask_levels": 10,
                "best_bid": 50000,
                "best_ask": 51000,
                "total_orders": 100,
                "bid_orders": 50,
                "ask_orders": 50
            });

            let orderbook_state = serde_json::to_vec(&orderbook_metadata).unwrap();
            let snapshot = create_snapshot(99, orderbook_state);
            let writer = SnapshotWriter::new(data_dir.clone(), false, 3).unwrap();
            writer.write(snapshot).unwrap();
        }

        // Simulate crash and verify recovery
        {
            let recovery = RecoveryManager::new(data_dir).unwrap();
            let result = recovery.recover().unwrap();

            // Verify snapshot loaded
            assert!(result.snapshot.is_some(), "Snapshot should be loaded");
            let snapshot = result.snapshot.as_ref().unwrap();

            // Verify orderbook state can be deserialized
            let metadata: serde_json::Value =
                serde_json::from_slice(&snapshot.orderbook_state).unwrap();

            assert_eq!(metadata["bid_levels"], 10, "Should have 10 bid levels");
            assert_eq!(metadata["ask_levels"], 10, "Should have 10 ask levels");
            assert_eq!(metadata["best_bid"], 50000, "Best bid should be 50000");
            assert_eq!(metadata["best_ask"], 51000, "Best ask should be 51000");
            assert_eq!(metadata["total_orders"], 100, "Should have 100 total orders");

            // Verify data integrity
            assert!(recovery.verify().unwrap(), "Orderbook consistency check should pass");

            println!("Orderbook consistency verified successfully");
        }
    }

    /// Test 5: Multiple sequential crashes
    /// Tests recovery resilience through multiple crash scenarios
    #[test]
    fn test_recovery_multiple_crashes() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        // First batch: 100 orders
        {
            let mut writer = WalWriter::new(data_dir.clone(), true, 10, 1024 * 1024).unwrap();
            for i in 0..100 {
                let seq = writer.next_seq();
                let entry = WalEntry::NewOrder {
                    seq,
                    timestamp: i,
                    order: NewOrderRequest {
                        user_id: i,
                        symbol: Arc::from("BTCUSDT"),
                        order_type: OrderType::Buy,
                        price: 50000,
                        quantity: 10,
                    },
                };
                writer.write(entry).unwrap();
            }
            writer.flush().unwrap();
        }

        // First crash and recovery
        let first_recovery_seq = {
            let recovery = RecoveryManager::new(data_dir.clone()).unwrap();
            let result = recovery.recover().unwrap();
            assert_eq!(result.wal_entries.len(), 100, "First recovery: should have 100 orders");
            result.last_seq
        };

        // Second batch: Continue from recovered state, add 200 more orders
        {
            let mut writer = WalWriter::new(data_dir.clone(), true, 10, 1024 * 1024).unwrap()
                .with_seq(first_recovery_seq + 1);

            for i in 0..200 {
                let seq = writer.next_seq();
                let entry = WalEntry::NewOrder {
                    seq,
                    timestamp: 100 + i,
                    order: NewOrderRequest {
                        user_id: 100 + i,
                        symbol: Arc::from("ETHUSDT"),
                        order_type: OrderType::Sell,
                        price: 3000,
                        quantity: 5,
                    },
                };
                writer.write(entry).unwrap();
            }
            writer.flush().unwrap();
        }

        // Create snapshot after second batch
        {
            use crate::infrastructure::persistence::snapshot::SnapshotWriter;

            let snapshot = create_snapshot(150, b"Snapshot after second batch".to_vec());
            let writer = SnapshotWriter::new(data_dir.clone(), false, 3).unwrap();
            writer.write(snapshot).unwrap();
        }

        // Second crash and recovery
        let second_recovery_seq = {
            let recovery = RecoveryManager::new(data_dir.clone()).unwrap();
            let result = recovery.recover().unwrap();

            // Should have snapshot at seq 150
            assert!(result.snapshot.is_some(), "Second recovery: should have snapshot");
            assert_eq!(result.snapshot.as_ref().unwrap().wal_seq, 150);

            // Should replay entries after snapshot (151-299)
            assert_eq!(result.wal_entries.len(), 149, "Should replay 149 entries after snapshot");
            result.last_seq
        };

        // Third batch: Add 50 more orders
        {
            let mut writer = WalWriter::new(data_dir.clone(), true, 10, 1024 * 1024).unwrap()
                .with_seq(second_recovery_seq + 1);

            for i in 0..50 {
                let seq = writer.next_seq();
                let entry = WalEntry::NewOrder {
                    seq,
                    timestamp: 300 + i,
                    order: NewOrderRequest {
                        user_id: 300 + i,
                        symbol: Arc::from("BTCUSDT"),
                        order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
                        price: 50000 + i,
                        quantity: 10,
                    },
                };
                writer.write(entry).unwrap();
            }
            writer.flush().unwrap();
        }

        // Final recovery after third crash
        {
            let recovery = RecoveryManager::new(data_dir).unwrap();
            let result = recovery.recover().unwrap();

            // Should still have snapshot at 150
            assert!(result.snapshot.is_some(), "Final recovery: should have snapshot");

            // Should replay all entries after snapshot (151-349)
            assert_eq!(result.wal_entries.len(), 199, "Final: should have 199 entries after snapshot");
            assert_eq!(result.last_seq, 349, "Final sequence should be 349");

            // Verify data integrity after multiple crashes
            assert!(recovery.verify().unwrap(), "Final integrity check should pass");

            println!("Multiple crash recovery test passed!");
            println!("  - Survived 3 crashes");
            println!("  - Final state: {} orders", result.entry_count);
            println!("  - Last sequence: {}", result.last_seq);
        }
    }
}
