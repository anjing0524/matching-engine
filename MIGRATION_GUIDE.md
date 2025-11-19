# 架构迁移指南 🔄

本指南帮助你将代码从Legacy API迁移到新的DDD架构。

> **目标架构**: DDD + Hexagonal Architecture (v4.0)
> **向后兼容**: Legacy API仍可用，但已标记为 `@deprecated`
> **推荐行动**: 尽快迁移到新架构以获得更好的性能和维护性

---

## 📋 目录

- [迁移概览](#迁移概览)
- [模块映射对照表](#模块映射对照表)
- [API迁移指南](#api迁移指南)
- [迁移检查清单](#迁移检查清单)
- [常见问题](#常见问题)

---

## 🔍 迁移概览

### 架构变更

**旧架构 (Legacy)**:
```
src/
├── orderbook.rs          # BTreeMap实现
├── orderbook_v2.rs       # BTreeMap + RingBuffer
├── engine.rs             # 单线程引擎
├── partitioned_engine.rs # 多线程引擎
└── network.rs            # 网络层
```

**新架构 (DDD)**:
```
src/
├── domain/orderbook/tick_based.rs    # 核心订单簿
├── application/services/             # 应用服务
├── infrastructure/network/           # 网络实现
└── legacy/                           # 旧代码（向后兼容）
```

### 为什么要迁移？

✅ **更好的性能**: TickBasedOrderBook比BTreeMap快245%
✅ **更清晰的架构**: DDD分层设计，职责明确
✅ **更易维护**: 依赖注入，易于测试
✅ **更好的扩展性**: 易于添加新功能

---

## 📊 模块映射对照表

### 订单簿 (OrderBook)

| Legacy API | 新架构 API | 说明 |
|-----------|-----------|------|
| `orderbook::OrderBook` | `domain::orderbook::TickBasedOrderBook` | 核心订单簿实现 |
| `orderbook_v2::OrderBookV2` | `domain::orderbook::TickBasedOrderBook` | V2已废弃 |
| `OrderBook::new()` | `TickBasedOrderBook::new(ContractSpec)` | 需要合约规格 |
| `OrderBook::match_order()` | `TickBasedOrderBook::match_order()` | 相同API |
| `OrderBook::cancel_order()` | `TickBasedOrderBook::cancel_order()` | 相同API |

### 撮合引擎 (Engine)

| Legacy API | 新架构 API | 说明 |
|-----------|-----------|------|
| `engine::MatchingEngine` | `application::services::MatchingService` | 单线程服务 |
| `partitioned_engine::PartitionedEngine` | `application::services::PartitionedService` | 多线程服务 |
| `EngineCommand` | 直接调用服务方法 | 无需命令枚举 |
| `EngineOutput` | 直接返回结果 | 无需输出枚举 |

### 协议 (Protocol)

| Legacy API | 新架构 API | 说明 |
|-----------|-----------|------|
| `protocol::NewOrderRequest` | `shared::protocol::NewOrderRequest` | 移至shared层 |
| `protocol::OrderType` | `shared::protocol::OrderType` | 移至shared层 |
| `protocol::TradeNotification` | `shared::protocol::TradeNotification` | 移至shared层 |
| `protocol::OrderConfirmation` | `shared::protocol::OrderConfirmation` | 移至shared层 |

### 网络 (Network)

| Legacy API | 新架构 API | 说明 |
|-----------|-----------|------|
| `network::run_server()` | `infrastructure::network::backends::tokio::*` | 模块化后端 |
| `network::connect_client()` | `infrastructure::network::backends::tokio::*` | 模块化后端 |

---

## 🔧 API迁移指南

### 1. 订单簿迁移

#### Before (Legacy)
```rust
use matching_engine::orderbook::OrderBook;

let mut orderbook = OrderBook::new();

let order = NewOrderRequest {
    user_id: 1,
    symbol: Arc::from("BTC/USD"),
    order_type: OrderType::Buy,
    price: 50000,
    quantity: 10,
};

let (trades, confirmation) = orderbook.match_order(order);
```

#### After (New)
```rust
use matching_engine::domain::orderbook::{ContractSpec, TickBasedOrderBook};
use matching_engine::shared::protocol::{NewOrderRequest, OrderType};

// 创建合约规格
let spec = ContractSpec::new(
    "BTC/USD",
    1,           // tick_size
    1000,        // min_price
    100000,      // max_price
);

let mut orderbook = TickBasedOrderBook::new(spec);

let order = NewOrderRequest {
    user_id: 1,
    symbol: Arc::from("BTC/USD"),
    order_type: OrderType::Buy,
    price: 50000,
    quantity: 10,
};

let (trades, confirmation) = orderbook.match_order(order);
```

**关键变更**:
- ✅ 必须提供 `ContractSpec`（合约规格）
- ✅ 导入路径改为 `domain::orderbook`
- ✅ 协议类型从 `shared::protocol` 导入

### 2. 撮合引擎迁移

#### Before (Legacy)
```rust
use matching_engine::engine::{MatchingEngine, EngineCommand};

let mut engine = MatchingEngine::new();

let (tx, rx) = mpsc::unbounded();

// 通过channel发送命令
tx.send(EngineCommand::NewOrder(order)).unwrap();

// 从channel接收结果
while let Some(output) = rx.recv().await {
    match output {
        EngineOutput::Trade(trade) => { /* ... */ }
        EngineOutput::Confirmation(conf) => { /* ... */ }
    }
}
```

#### After (New)
```rust
use matching_engine::application::services::MatchingService;
use matching_engine::domain::orderbook::{ContractSpec, TickBasedOrderBook};

// 创建订单簿
let spec = ContractSpec::new("BTC/USD", 1, 1000, 100000);
let orderbook = TickBasedOrderBook::new(spec);

// 创建服务（依赖注入）
let service = MatchingService::new(orderbook);

// 直接调用方法
let (trades, confirmation) = service.match_order(order);

// 处理结果
for trade in trades {
    println!("Trade: {:?}", trade);
}

if let Some(conf) = confirmation {
    println!("Confirmation: {:?}", conf);
}
```

**关键变更**:
- ✅ 无需 channel 通信（如果不需要并发）
- ✅ 直接方法调用替代命令模式
- ✅ 使用依赖注入设计

### 3. 分区引擎迁移

#### Before (Legacy)
```rust
use matching_engine::partitioned_engine::{PartitionedEngine, PartitionConfig};

let config = PartitionConfig {
    partition_count: 4,
    queue_capacity: 10000,
    batch_size: 100,
    enable_cpu_affinity: false,
};

let engine = PartitionedEngine::new(config);

// 异步提交
engine.submit_order(order).unwrap();

// 同步提交
let result = engine.submit_order_sync(order);
```

#### After (New)
```rust
use matching_engine::application::services::PartitionedService;
use matching_engine::domain::orderbook::ContractSpec;

// 创建合约规格
let spec = ContractSpec::new("BTC/USD", 1, 1000, 100000);

// 创建分区服务
let service = PartitionedService::new(
    4,      // partition_count
    spec,
);

// 提交订单
let (trades, confirmation) = service.submit_order(order);
```

**关键变更**:
- ✅ 简化配置（只需分区数和规格）
- ✅ 统一API（不区分sync/async）
- ✅ 更清晰的依赖关系

### 4. 网络层迁移

#### Before (Legacy)
```rust
use matching_engine::network;

// 启动服务器
network::run_server("127.0.0.1:8080").await?;

// 连接客户端
let client = network::connect_client("127.0.0.1:8080").await?;
```

#### After (New)
```rust
use matching_engine::infrastructure::network::backends::tokio;

// 启动服务器
tokio::run_server("127.0.0.1:8080", engine).await?;

// 连接客户端
let client = tokio::connect_client("127.0.0.1:8080").await?;
```

**关键变更**:
- ✅ 模块化后端设计
- ✅ 可选择 Tokio / io_uring / DPDK
- ✅ 更清晰的模块组织

---

## ✅ 迁移检查清单

使用以下检查清单确保完整迁移：

### 代码迁移
- [ ] 更新所有 `orderbook::OrderBook` → `domain::orderbook::TickBasedOrderBook`
- [ ] 更新所有 `orderbook_v2::OrderBookV2` → `domain::orderbook::TickBasedOrderBook`
- [ ] 添加 `ContractSpec` 创建代码
- [ ] 更新 `engine::MatchingEngine` → `application::services::MatchingService`
- [ ] 更新 `partitioned_engine::PartitionedEngine` → `application::services::PartitionedService`
- [ ] 更新所有协议导入: `protocol::*` → `shared::protocol::*`
- [ ] 移除 `EngineCommand` 和 `EngineOutput` 枚举使用
- [ ] 更新网络层导入路径

### 测试更新
- [ ] 更新单元测试导入路径
- [ ] 更新集成测试导入路径
- [ ] 添加 `#[allow(deprecated)]` 到仍使用legacy API的测试
- [ ] 运行所有测试确保通过: `cargo test`

### 基准测试更新
- [ ] 更新基准测试导入路径
- [ ] 添加 `#[allow(deprecated)]` 到legacy对比基准
- [ ] 验证基准测试编译: `cargo bench --no-run`

### 文档更新
- [ ] 更新代码注释中的API引用
- [ ] 更新README中的示例代码
- [ ] 更新内部文档

### 验证
- [ ] 编译通过: `cargo build --release`
- [ ] 所有测试通过: `cargo test --all-targets`
- [ ] 基准测试编译: `cargo bench --no-run`
- [ ] Clippy检查通过: `cargo clippy -- -D warnings -A deprecated`

---

## ❓ 常见问题

### Q1: 为什么需要 ContractSpec？
**A**: `ContractSpec` 包含合约的tick_size和价格范围，这对于Array-based订单簿的性能至关重要。Legacy版本使用BTreeMap，不需要这些信息。

**示例**:
```rust
let spec = ContractSpec::new(
    "rb2501",    // symbol
    10,          // tick_size = 10元/吨
    3000,        // min_price = 3000元
    5000,        // max_price = 5000元
);
```

### Q2: Legacy API什么时候会被移除？
**A**: Legacy API将在至少2个主版本中保持可用，预计在v3.0之前不会移除。届时会提前至少3个月通知。

### Q3: 新架构的性能如何？
**A**: 新架构（TickBasedOrderBook）比Legacy实现快：
- vs OrderBook (BTreeMap): **+245%** (9.34M vs 2.71M ops/s)
- vs OrderBookV2 (BTreeMap+RingBuffer): **+160%** (9.34M vs 3.59M ops/s)

### Q4: 可以混用Legacy和新API吗？
**A**: 技术上可以，但不推荐。建议一次性完成迁移以避免代码复杂性。

### Q5: 迁移会影响性能吗？
**A**: 不会！新架构性能更优。架构重构时我们确保了零性能退化。

### Q6: 如何处理现有的持久化数据？
**A**: 如果使用了Legacy引擎的持久化，需要：
1. 导出数据为中间格式（JSON/CSV）
2. 使用新架构的持久化层导入
3. 参考: `infrastructure/persistence/` 模块

### Q7: 遇到编译错误怎么办？
**A**: 常见错误和解决方案：

**错误1: 找不到OrderBook**
```rust
// ❌ 错误
use matching_engine::orderbook::OrderBook;

// ✅ 正确
use matching_engine::domain::orderbook::TickBasedOrderBook;
```

**错误2: ContractSpec参数缺失**
```rust
// ❌ 错误
let book = TickBasedOrderBook::new();

// ✅ 正确
let spec = ContractSpec::new("BTC/USD", 1, 1000, 100000);
let book = TickBasedOrderBook::new(spec);
```

**错误3: 协议类型找不到**
```rust
// ❌ 错误
use matching_engine::protocol::OrderType;

// ✅ 正确
use matching_engine::shared::protocol::OrderType;
```

### Q8: 需要更改配置吗？
**A**: 大部分配置保持不变。主要变更：
- `ContractSpec` 替代了隐式的价格范围
- 分区配置简化（只需分区数，无需queue_capacity等）

### Q9: 如何逐步迁移大型项目？
**A**: 推荐迁移策略：
1. **第一步**: 更新导入路径，添加 `#[allow(deprecated)]`
2. **第二步**: 创建新架构的并行实现
3. **第三步**: 逐模块替换，保留测试覆盖
4. **第四步**: 移除 `#[allow(deprecated)]`，完成迁移

### Q10: 哪里可以找到迁移示例？
**A**: 查看以下文件：
- `tests/chaos_engineering_test.rs` - 新架构完整示例
- `benches/tick_orderbook_benchmark.rs` - 新旧API对比
- `examples/` - 完整示例程序

---

## 📚 相关资源

- **[ARCHITECTURE.md](ARCHITECTURE.md)** - 新架构设计详解
- **[README.md](README.md)** - 项目概览和快速开始
- **[CONTRIBUTING.md](CONTRIBUTING.md)** - 开发规范
- **[API文档]** - 使用 `cargo doc --open` 生成

---

## 💬 获取帮助

遇到迁移问题？

- 📖 查阅 [DOCS_INDEX.md](DOCS_INDEX.md) 文档索引
- 🐛 提交 [GitHub Issue](../../issues)
- 💬 查看已有的迁移相关Issue

---

**最后更新**: 2025-11-19
**适用版本**: v2.0+ (Legacy兼容) → v4.0 (DDD架构)
**维护者**: Matching Engine Team
