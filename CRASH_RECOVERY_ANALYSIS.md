# 崩溃恢复场景分析

## 当前实现评估

### ✅ 已实现的保护措施

1. **WAL预写日志**
   - 所有订单操作先写入WAL，再执行撮合
   - 使用CRC32校验和保证数据完整性
   - 支持fsync强制刷盘（`wal_sync: true`）

2. **定期快照**
   - 每5分钟或10000个操作创建快照
   - 加速恢复过程（避免重放大量WAL）
   - 压缩存储，减少磁盘占用

3. **自动恢复机制**
   - 启动时自动检测数据目录
   - 加载快照 + 重放WAL日志
   - 数据完整性验证

### ⚠️ 潜在风险点

#### 1. **批量刷盘导致数据丢失（高风险）**

**问题描述**：
```rust
// src/infrastructure/persistence/types.rs:97
wal_batch_size: 1000,  // 每1000条才刷盘
```

**影响**：
- 最近1000条以内的订单可能在OS缓冲区中，未真正写入磁盘
- 如果服务器突然断电、崩溃（SIGKILL）或内核panic，这些订单会丢失
- 即使`wal_sync: true`，也只在达到批量大小时才执行fsync

**场景示例**：
```
1. 客户端提交订单 #1-#999
2. WAL写入内存缓冲区（未fsync）
3. 💥 服务器突然断电
4. 重启后：订单 #1-#999 全部丢失
```

**数据丢失窗口**：
- 低负载：几秒到几分钟（取决于订单频率）
- 高负载：毫秒级（订单快速累积到1000条）

#### 2. **没有优雅关闭机制（中风险）**

**问题描述**：
- 没有处理SIGTERM、SIGINT信号
- 无法保证正常关闭时刷新所有数据
- 依赖Drop trait（不可靠）

**影响**：
- `systemctl stop matching-engine` 可能丢失数据
- Docker容器重启可能丢失数据
- Kubernetes Pod终止可能丢失数据

#### 3. **客户端无法确认订单已持久化（中风险）**

**问题描述**：
```rust
// src/interfaces/cli/mod.rs:228
match svc.match_order(order.clone()) {
    Ok((trades, _confirmation)) => {
        // 没有向客户端发送响应
        println!("  ✅ 订单撮合成功...");
    }
}
```

**影响**：
- 客户端不知道订单是否已提交
- 无法实现at-least-once或exactly-once语义
- 可能导致重复提交或订单丢失

#### 4. **连接断开时的状态不一致（低风险）**

**问题描述**：
- 客户端连接断开时，正在处理的订单状态不明
- 没有事务机制确保WAL写入和撮合的原子性

## 风险评估矩阵

| 风险点 | 严重程度 | 发生概率 | 综合风险 | 优先级 |
|--------|----------|----------|----------|--------|
| 批量刷盘数据丢失 | 🔴 高 | 🟡 中 | 🔴 高 | P0 |
| 无优雅关闭机制 | 🟡 中 | 🟢 高 | 🟡 中 | P1 |
| 客户端无确认 | 🟡 中 | 🟢 高 | 🟡 中 | P1 |
| 连接状态不一致 | 🟢 低 | 🟡 中 | 🟢 低 | P2 |

## 测试场景

### 场景1：服务器突然断电
```bash
# 1. 启动服务器
cargo run --release

# 2. 发送100个订单
# 3. 立即kill -9（模拟断电）
kill -9 <pid>

# 4. 重启服务器，检查订单恢复情况
# 预期结果：如果批次未满，订单可能丢失
```

### 场景2：正常关闭（systemctl stop）
```bash
# 1. 启动服务器
cargo run --release

# 2. 发送订单
# 3. 正常关闭
kill -TERM <pid>

# 4. 检查是否所有订单都已持久化
# 当前实现：依赖Drop trait，不保证执行
```

### 场景3：高负载下崩溃
```bash
# 1. 并发发送大量订单
# 2. 在处理过程中kill -9
# 3. 检查数据完整性和一致性
```

## 改进建议

### P0：修复批量刷盘数据丢失问题

**方案A：每条订单都fsync（最安全）**
```rust
wal_batch_size: 1,  // 每条都刷盘
wal_sync: true,     // 启用fsync
```
- ✅ 数据安全性最高（零丢失）
- ❌ 性能影响大（~1000x慢）
- 适用场景：金融级系统，数据绝对不能丢失

**方案B：小批量刷盘（平衡）**
```rust
wal_batch_size: 10,  // 每10条刷盘
wal_sync: true,
```
- ✅ 数据丢失窗口小（最多10条）
- ✅ 性能影响可控（~10x慢）
- ✅ 推荐方案

**方案C：时间触发刷盘（异步）**
```rust
// 每10ms或100条，先到者优先
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_millis(10));
    loop {
        interval.tick().await;
        wal_writer.flush()?;
    }
});
```
- ✅ 数据丢失窗口可控（10ms内）
- ✅ 性能影响小
- ⚠️ 实现复杂度高

### P1：添加优雅关闭机制

```rust
use tokio::signal;

async fn run_with_graceful_shutdown() {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    // 监听信号
    tokio::spawn(async move {
        signal::ctrl_c().await.expect("failed to listen for ctrl-c");
        println!("收到关闭信号，正在优雅关闭...");
        let _ = tx.send(());
    });

    // 服务器主循环
    tokio::select! {
        _ = server_loop() => {},
        _ = rx => {
            // 刷新所有WAL
            service.flush().expect("flush failed");
            println!("所有数据已刷盘，退出");
        }
    }
}
```

### P1：添加客户端确认机制

```rust
// 向客户端发送响应
let response = ServerMessage::OrderConfirmed {
    order_id: confirmation.order_id,
    timestamp: SystemTime::now(),
};
conn.send(encode(response)?).await?;
```

### P2：添加事务语义

```rust
// 确保WAL写入和撮合的原子性
pub fn match_order_transactional(&mut self, order: NewOrderRequest)
    -> Result<(Vec<TradeNotification>, OrderConfirmation)> {

    // 1. 写入WAL
    let seq = self.wal_writer.write(WalEntry::NewOrder { ... })?;

    // 2. 立即fsync（确保持久化）
    self.wal_writer.flush()?;

    // 3. 执行撮合
    let result = self.orderbook.match_order(order);

    // 4. 记录结果到WAL
    for trade in &result.0 {
        self.wal_writer.write(WalEntry::Trade { ... })?;
    }

    // 5. 最终fsync
    self.wal_writer.flush()?;

    Ok(result)
}
```

## 性能vs可靠性权衡

| 方案 | 延迟 (μs) | 吞吐量 (ops/s) | 数据丢失风险 | 推荐场景 |
|------|-----------|----------------|--------------|----------|
| 无fsync | 1 | 1,000,000 | 🔴 高 | 测试环境 |
| 批量1000+fsync | 1 | 1,000,000 | 🟡 中 | 非关键业务 |
| 批量10+fsync | 10 | 100,000 | 🟢 低 | **推荐** |
| 每条fsync | 1000 | 1,000 | ✅ 零 | 金融系统 |
| 异步10ms刷盘 | 1 | 1,000,000 | 🟢 低 | 高性能系统 |

## 结论

当前实现的**最大风险**是批量刷盘可能导致数据丢失。建议：

1. **立即行动（P0）**：
   - 将`wal_batch_size`从1000改为10
   - 或实现时间触发刷盘机制

2. **短期改进（P1）**：
   - 添加信号处理和优雅关闭
   - 实现客户端响应确认

3. **长期优化（P2）**：
   - 异步WAL写入
   - 内存映射文件（mmap）
   - 增量快照

## 测试计划

需要编写以下测试：

1. ✅ `test_recovery_from_scratch` - 已有
2. ✅ `test_recovery_from_snapshot` - 已有
3. ❌ `test_crash_with_unflushed_data` - 缺失
4. ❌ `test_graceful_shutdown` - 缺失
5. ❌ `test_kill_9_data_loss` - 缺失
6. ❌ `test_concurrent_crash` - 缺失
