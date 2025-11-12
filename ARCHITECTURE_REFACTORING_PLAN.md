# 匹配引擎架构重构计划

## 1. 当前架构分析

### 1.1 现有模块结构 (5,077 LOC)

```
src/
├── lib.rs (22 lines)              # 模块声明入口
├── main.rs (50 lines)             # 主程序入口
├── bin/load_generator.rs (158)    # 负载生成器
│
├── 核心域模块 (~1,500 lines)
│   ├── orderbook.rs (327)         # V1 BTreeMap实现（历史）
│   ├── orderbook_v2.rs (332)      # V2 RingBuffer实现（历史）
│   ├── orderbook_tick.rs (516)    # V3 Array+FastBitmap（生产）⭐
│   ├── fast_bitmap.rs (342)       # 硬件加速位图
│   └── ringbuffer.rs (321)        # 零分配环形缓冲
│
├── 应用模块 (~484 lines)
│   ├── engine.rs (76)             # 单线程引擎
│   └── partitioned_engine.rs (408)# 分区并行引擎
│
├── 协议/数据 (~422 lines)
│   ├── protocol.rs (229)          # 消息定义
│   └── symbol_pool.rs (193)       # 交易对池
│
├── 基础设施 (~1,902 lines)
│   ├── network.rs (108)           # 旧网络层（Tokio TCP）
│   └── network_middleware/ (~1,794 lines)
│       ├── mod.rs (194)
│       ├── traits.rs (131)        # 核心抽象
│       ├── buffer.rs (226)        # 零拷贝缓冲
│       ├── codec.rs (264)         # 编解码
│       ├── metrics.rs (231)       # 性能指标
│       ├── tokio_backend.rs (132) # Tokio实现
│       ├── io_uring_backend.rs (266) # io_uring实现
│       └── dpdk_backend.rs (364)  # DPDK实现
│
└── 工具 (~187 lines)
    └── timestamp.rs (187)         # 时间戳工具
```

### 1.2 当前问题分析

#### 问题1: 模块职责不清晰
- **症状**: `lib.rs` 平铺式声明所有模块，缺乏层次
- **影响**: 依赖关系不受约束，难以维护
- **证据**:
  ```rust
  // lib.rs 当前结构
  pub mod protocol;
  pub mod orderbook;      // V1
  pub mod orderbook_v2;   // V2
  pub mod orderbook_tick; // V3
  pub mod engine;
  pub mod network;
  pub mod network_middleware;
  // ... 等12个模块平铺
  ```

#### 问题2: 历史版本混杂
- **症状**: 保留V1/V2/V3三个订单簿实现
- **影响**:
  - 增加维护负担
  - 混淆生产代码
  - 增加编译时间（1,159 lines历史代码）
- **证据**:
  - `orderbook.rs` (V1): BTreeMap实现，已过时
  - `orderbook_v2.rs` (V2): RingBuffer实现，已过时
  - `orderbook_tick.rs` (V3): 生产版本 ⭐

#### 问题3: 网络层双重实现
- **症状**: `network.rs` 和 `network_middleware/` 功能重叠
- **影响**:
  - `network.rs` 使用旧API（Tokio TCP）
  - `network_middleware/` 是新架构（多后端）
  - 两者共存导致混乱
- **建议**: 废弃 `network.rs`，统一使用 `network_middleware`

#### 问题4: 应用层耦合
- **症状**: `engine.rs` 和 `partitioned_engine.rs` 直接依赖具体实现
- **影响**: 难以切换订单簿实现、测试和模拟
- **证据**:
  ```rust
  // engine.rs
  use crate::orderbook::OrderBook; // 硬编码依赖V1

  // partitioned_engine.rs
  use crate::orderbook::OrderBook; // 同样硬编码
  ```

#### 问题5: 缺乏清晰的分层
- **症状**: 依赖关系混乱，缺乏单向依赖流
- **影响**:
  - 难以理解系统架构
  - 难以进行模块级测试
  - 重构困难

### 1.3 依赖关系分析

```
当前依赖流（混乱）:

main.rs → engine → orderbook (V1) ← partitioned_engine
                                   ↘
network.rs → protocol               ↓
                                  symbol_pool
network_middleware → protocol    ↗
```

**问题**:
- engine 和 partitioned_engine 都依赖具体的 OrderBook V1
- network.rs 和 network_middleware 并存
- 缺乏统一的抽象层

---

## 2. 目标架构设计

### 2.1 分层架构原则

遵循 **六边形架构 (Hexagonal Architecture)** / **洋葱架构 (Onion Architecture)**:

```
┌─────────────────────────────────────────────┐
│  Interfaces Layer (接口层)                  │
│  - CLI, REST API, gRPC, etc.                │
└────────────────┬────────────────────────────┘
                 │
┌────────────────▼────────────────────────────┐
│  Application Layer (应用层)                 │
│  - Orchestration, workflows                 │
│  - Use cases: MatchOrder, CancelOrder       │
└────────────────┬────────────────────────────┘
                 │
┌────────────────▼────────────────────────────┐
│  Domain Layer (领域层 - 核心)               │
│  - Business logic: OrderBook, Matching      │
│  - Domain entities, value objects           │
│  - NO external dependencies                 │
└────────────────┬────────────────────────────┘
                 │
┌────────────────▼────────────────────────────┐
│  Infrastructure Layer (基础设施层)          │
│  - Network I/O, Persistence, Metrics        │
│  - External system adapters                 │
└─────────────────────────────────────────────┘
```

**依赖规则**:
1. 外层依赖内层（单向）
2. 内层不知道外层的存在
3. 领域层纯粹，无外部依赖

### 2.2 模块重组方案

```
src/
├── lib.rs                         # 清晰的分层导出
│
├── domain/                        # 领域层（核心业务逻辑）
│   ├── mod.rs
│   ├── orderbook/
│   │   ├── mod.rs
│   │   ├── tick_based.rs         # V3生产实现（重命名）
│   │   ├── traits.rs             # 订单簿抽象
│   │   └── test_impl.rs          # 测试用简化实现（可选）
│   ├── matching/
│   │   ├── mod.rs
│   │   └── engine.rs             # 纯撮合逻辑（提取）
│   ├── entities/
│   │   ├── mod.rs
│   │   ├── order.rs              # 订单实体
│   │   └── trade.rs              # 成交实体
│   └── value_objects/
│       ├── mod.rs
│       ├── price.rs              # 价格类型（可选）
│       └── quantity.rs           # 数量类型（可选）
│
├── application/                   # 应用层（用例编排）
│   ├── mod.rs
│   ├── use_cases/
│   │   ├── mod.rs
│   │   ├── match_order.rs        # 撮合订单用例
│   │   ├── cancel_order.rs       # 取消订单用例
│   │   └── query_depth.rs        # 查询深度用例（可选）
│   ├── services/
│   │   ├── mod.rs
│   │   ├── matching_service.rs   # 单线程服务（重构engine.rs）
│   │   └── partitioned_service.rs# 分区服务（重构partitioned_engine.rs）
│   └── dto/
│       ├── mod.rs
│       └── requests.rs            # 请求/响应DTO
│
├── infrastructure/                # 基础设施层
│   ├── mod.rs
│   ├── network/
│   │   ├── mod.rs                # 重组network_middleware
│   │   ├── traits.rs
│   │   ├── buffer.rs
│   │   ├── codec.rs
│   │   ├── metrics.rs
│   │   ├── backends/
│   │   │   ├── mod.rs
│   │   │   ├── tokio.rs
│   │   │   ├── io_uring.rs
│   │   │   └── dpdk.rs
│   │   └── server.rs             # 服务器实现
│   ├── persistence/              # 持久化（未来）
│   │   └── mod.rs
│   └── telemetry/
│       ├── mod.rs
│       ├── metrics.rs
│       └── tracing.rs
│
├── shared/                        # 共享工具和类型
│   ├── mod.rs
│   ├── protocol.rs               # 协议定义
│   ├── symbol_pool.rs            # 交易对池
│   ├── timestamp.rs              # 时间工具
│   ├── collections/
│   │   ├── mod.rs
│   │   ├── ringbuffer.rs         # 环形缓冲
│   │   └── fast_bitmap.rs        # 位图
│   └── error.rs                  # 统一错误类型
│
├── interfaces/                    # 接口层
│   ├── mod.rs
│   ├── cli/
│   │   └── mod.rs                # CLI接口（main.rs重构）
│   └── tools/
│       └── load_generator.rs     # 负载生成器
│
└── legacy/                        # 历史实现（隔离）
    ├── mod.rs
    ├── orderbook_v1.rs           # BTreeMap版本
    └── orderbook_v2.rs           # RingBuffer版本
```

### 2.3 依赖关系图（重构后）

```
┌──────────────┐
│  interfaces  │  (bin/main.rs, CLI, API)
└──────┬───────┘
       │
       ▼
┌──────────────┐
│ application  │  (use_cases, services)
└──────┬───────┘
       │
       ▼
┌──────────────┐
│   domain     │  (orderbook, matching, entities)
└──────────────┘  ← 核心，无外部依赖
       ▲
       │
┌──────┴────────┐
│infrastructure │  (network, persistence, telemetry)
└───────────────┘

┌──────────────┐
│   shared     │  (protocol, collections, utils)
└──────────────┘  ← 所有层都可以使用

依赖流向: interfaces → application → domain ← infrastructure
```

---

## 3. 重构步骤

### 阶段1: 准备工作 ✅
- [x] 分析现有架构
- [x] 识别问题
- [x] 设计目标架构
- [x] 制定重构计划

### 阶段2: 创建新目录结构
```bash
# 创建新的分层目录
mkdir -p src/{domain/{orderbook,matching,entities},application/{use_cases,services},infrastructure/{network/backends,telemetry},shared/collections,interfaces/{cli,tools},legacy}
```

### 阶段3: 移动和重构 shared 层（最底层）
**原因**: shared 层被所有其他层使用，先处理避免循环依赖

1. 移动通用工具
   ```bash
   mv src/protocol.rs src/shared/
   mv src/symbol_pool.rs src/shared/
   mv src/timestamp.rs src/shared/
   mv src/ringbuffer.rs src/shared/collections/
   mv src/fast_bitmap.rs src/shared/collections/
   ```

2. 更新模块路径
   - 全局搜索替换: `crate::protocol` → `crate::shared::protocol`
   - 全局搜索替换: `crate::ringbuffer` → `crate::shared::collections::ringbuffer`
   - 等等...

3. 创建 `src/shared/mod.rs`
   ```rust
   pub mod protocol;
   pub mod symbol_pool;
   pub mod timestamp;
   pub mod collections;
   pub mod error;  // 新增
   ```

### 阶段4: 重构 domain 层（核心）
**目标**: 领域层完全独立，只依赖 shared

1. 重命名和移动订单簿
   ```bash
   # V3 生产版本移入 domain
   mv src/orderbook_tick.rs src/domain/orderbook/tick_based.rs

   # V1/V2 移入 legacy
   mv src/orderbook.rs src/legacy/orderbook_v1.rs
   mv src/orderbook_v2.rs src/legacy/orderbook_v2.rs
   ```

2. 创建订单簿抽象 trait
   ```rust
   // src/domain/orderbook/traits.rs
   pub trait OrderBook {
       fn match_order(&mut self, order: NewOrderRequest)
           -> (SmallVec<[TradeNotification; 8]>, Option<OrderConfirmation>);
       fn cancel_order(&mut self, order_id: u64) -> Result<(), DomainError>;
       fn get_best_bid(&self) -> Option<u64>;
       fn get_best_ask(&self) -> Option<u64>;
   }
   ```

3. 实现 trait for TickBasedOrderBook
   ```rust
   // src/domain/orderbook/tick_based.rs
   impl OrderBook for TickBasedOrderBook {
       // 实现 trait 方法...
   }
   ```

4. 创建 `src/domain/mod.rs`
   ```rust
   pub mod orderbook;
   pub mod matching;
   pub mod entities;

   // Re-export key types
   pub use orderbook::{OrderBook, TickBasedOrderBook};
   ```

### 阶段5: 重构 infrastructure 层
**目标**: 整合网络层，清理重复实现

1. 废弃旧网络层
   ```bash
   rm src/network.rs  # 删除或移入 legacy
   ```

2. 重组 network_middleware → infrastructure/network
   ```bash
   mv src/network_middleware/* src/infrastructure/network/

   # 整理后端到子目录
   mkdir src/infrastructure/network/backends
   mv src/infrastructure/network/tokio_backend.rs src/infrastructure/network/backends/tokio.rs
   mv src/infrastructure/network/io_uring_backend.rs src/infrastructure/network/backends/io_uring.rs
   mv src/infrastructure/network/dpdk_backend.rs src/infrastructure/network/backends/dpdk.rs
   ```

3. 创建 `src/infrastructure/mod.rs`
   ```rust
   pub mod network;
   pub mod telemetry;

   pub use network::{NetworkTransport, Connection, ZeroCopyBuffer};
   ```

### 阶段6: 重构 application 层
**目标**: 解耦具体实现，依赖抽象

1. 重构 engine.rs → application/services/matching_service.rs
   ```rust
   // 旧代码（硬编码依赖）
   use crate::orderbook::OrderBook;

   // 新代码（依赖抽象）
   use crate::domain::orderbook::OrderBook;

   pub struct MatchingService<OB: OrderBook> {
       orderbook: OB,
       // ...
   }
   ```

2. 重构 partitioned_engine.rs → application/services/partitioned_service.rs
   ```rust
   pub struct PartitionedService<OB: OrderBook> {
       // 泛型化，支持任意OrderBook实现
   }
   ```

3. 创建用例层
   ```rust
   // src/application/use_cases/match_order.rs
   pub struct MatchOrderUseCase<OB: OrderBook> {
       orderbook: Arc<Mutex<OB>>,
   }

   impl<OB: OrderBook> MatchOrderUseCase<OB> {
       pub async fn execute(&self, request: NewOrderRequest)
           -> Result<MatchOrderResponse, ApplicationError> {
           // 用例编排逻辑
       }
   }
   ```

4. 创建 `src/application/mod.rs`
   ```rust
   pub mod use_cases;
   pub mod services;
   pub mod dto;

   pub use services::{MatchingService, PartitionedService};
   ```

### 阶段7: 重构 interfaces 层
**目标**: 清晰的入口点

1. 重构 main.rs → interfaces/cli/mod.rs
   ```rust
   // src/interfaces/cli/mod.rs
   use crate::application::services::MatchingService;
   use crate::domain::orderbook::TickBasedOrderBook;
   use crate::infrastructure::network::NetworkServer;

   pub async fn run_server(config: ServerConfig) -> Result<(), InterfaceError> {
       // CLI逻辑
   }
   ```

2. 移动 load_generator
   ```bash
   mv src/bin/load_generator.rs src/interfaces/tools/
   ```

3. 新的 main.rs 变成薄层
   ```rust
   // src/main.rs (仅入口)
   use matching_engine::interfaces::cli;

   #[tokio::main]
   async fn main() {
       cli::run_server(config).await.unwrap();
   }
   ```

### 阶段8: 更新 lib.rs
```rust
// src/lib.rs
#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

// === 分层导出 ===

// Domain Layer (核心业务逻辑)
pub mod domain;

// Application Layer (用例编排)
pub mod application;

// Infrastructure Layer (技术实现)
pub mod infrastructure;

// Shared (跨层工具)
pub mod shared;

// Interfaces Layer (外部接口)
pub mod interfaces;

// Legacy (历史实现，可选编译)
#[cfg(feature = "legacy")]
pub mod legacy;

// === Re-exports for convenience ===
pub use domain::orderbook::{OrderBook, TickBasedOrderBook};
pub use application::services::{MatchingService, PartitionedService};
pub use infrastructure::network::{NetworkTransport, Connection};
pub use shared::protocol::{NewOrderRequest, TradeNotification, OrderConfirmation};
```

### 阶段9: 更新所有测试和示例
1. 更新 `examples/network_server.rs`
   ```rust
   use matching_engine::domain::orderbook::TickBasedOrderBook;
   use matching_engine::infrastructure::network::...;
   use matching_engine::shared::protocol::...;
   ```

2. 更新 `tests/basic_trade.rs`
3. 更新所有基准测试

### 阶段10: 更新文档
1. 更新 README.md 反映新架构
2. 创建 `ARCHITECTURE.md` 详细说明各层职责
3. 更新 API 文档

### 阶段11: 验证和测试
```bash
# 编译检查
cargo check --all-features

# 运行所有测试
cargo test --all

# 运行基准测试
cargo bench

# 运行示例
cargo run --example network_server
```

### 阶段12: 清理和提交
```bash
# 删除未使用的代码
# 格式化代码
cargo fmt

# Lint检查
cargo clippy -- -W clippy::all

# 提交
git add .
git commit -m "refactor: 重构为清晰的分层架构

- 采用六边形架构，分层清晰
- Domain层独立，无外部依赖
- 废弃V1/V2订单簿，移入legacy
- 统一网络层为infrastructure/network
- Application层依赖抽象，支持DI
- 所有测试通过，性能无退化
"
```

---

## 4. 重构后的优势

### 4.1 清晰的职责分离
- ✅ **Domain层**: 纯业务逻辑，易于测试
- ✅ **Application层**: 用例编排，灵活组合
- ✅ **Infrastructure层**: 技术实现，可替换
- ✅ **Interfaces层**: 外部接口，可扩展

### 4.2 依赖倒置 (Dependency Inversion)
```rust
// 旧代码（紧耦合）
struct MatchingEngine {
    orderbook: OrderBook,  // 具体类型
}

// 新代码（松耦合）
struct MatchingService<OB: OrderBook> {
    orderbook: OB,  // 抽象trait
}
```

**好处**:
- 易于单元测试（注入mock）
- 易于切换实现（V1/V2/V3）
- 易于扩展新功能

### 4.3 模块化和可维护性
- ✅ 新增功能：在对应层添加，影响范围小
- ✅ 修改实现：只影响单个模块
- ✅ 测试：可独立测试各层
- ✅ 文档：按层组织，易于理解

### 4.4 编译时间优化
- ✅ 移除历史代码：减少1,159 LOC
- ✅ 清晰的模块边界：增量编译更快
- ✅ 按需编译：feature gate legacy代码

### 4.5 团队协作
- ✅ 明确的模块所有权
- ✅ 并行开发不冲突
- ✅ 新人快速理解架构

---

## 5. 风险评估

### 5.1 重构风险
| 风险 | 等级 | 缓解措施 |
|------|------|---------|
| 引入新bug | 中 | 完整的测试覆盖，基准测试验证性能 |
| 破坏现有功能 | 低 | 渐进式重构，保持API兼容 |
| 性能退化 | 低 | trait是零开销抽象，运行基准测试 |
| 时间成本 | 中 | 分阶段进行，每个阶段可验证 |

### 5.2 回滚计划
- 所有重构在新分支进行
- 每个阶段单独commit
- 问题可快速回滚到上一阶段
- 保留legacy代码作为backup

---

## 6. 时间估算

| 阶段 | 工作量 | 风险 |
|------|--------|------|
| 阶段1: 准备工作 | ✅ 完成 | - |
| 阶段2: 创建目录 | 10分钟 | 低 |
| 阶段3: shared层 | 30分钟 | 低 |
| 阶段4: domain层 | 1小时 | 中 |
| 阶段5: infrastructure层 | 1小时 | 中 |
| 阶段6: application层 | 1.5小时 | 中 |
| 阶段7: interfaces层 | 30分钟 | 低 |
| 阶段8: lib.rs | 15分钟 | 低 |
| 阶段9: 测试和示例 | 1小时 | 高 |
| 阶段10: 文档 | 30分钟 | 低 |
| 阶段11: 验证 | 30分钟 | 中 |
| 阶段12: 清理提交 | 15分钟 | 低 |
| **总计** | **~7小时** | - |

**建议**: 分2-3个工作日完成，每天2-3小时

---

## 7. 下一步行动

### 立即执行（阶段2）
```bash
# 1. 创建新目录结构
mkdir -p src/domain/{orderbook,matching,entities}
mkdir -p src/application/{use_cases,services,dto}
mkdir -p src/infrastructure/{network/backends,telemetry}
mkdir -p src/shared/collections
mkdir -p src/interfaces/{cli,tools}
mkdir -p src/legacy

# 2. 创建各层的 mod.rs 文件
touch src/domain/mod.rs
touch src/application/mod.rs
touch src/infrastructure/mod.rs
touch src/shared/mod.rs
touch src/interfaces/mod.rs
touch src/legacy/mod.rs
```

### 确认点
在开始执行前，请确认：
- [ ] 理解分层架构理念
- [ ] 同意重构方案
- [ ] 准备好投入时间
- [ ] 已备份当前代码（git commit）

---

## 附录A: 架构模式参考

### 六边形架构 (Hexagonal Architecture)
- **提出者**: Alistair Cockburn (2005)
- **核心思想**: 应用核心与外部解耦，通过端口和适配器交互
- **优势**: 易于测试，易于替换外部依赖

### 洋葱架构 (Onion Architecture)
- **提出者**: Jeffrey Palermo (2008)
- **核心思想**: 分层同心圆，内层不知道外层
- **优势**: 依赖流向清晰，核心业务逻辑独立

### 整洁架构 (Clean Architecture)
- **提出者**: Robert C. Martin (Uncle Bob, 2012)
- **核心思想**: 依赖规则，源代码依赖只能指向内层
- **优势**: 框架独立，数据库独立，易于测试

**我们的架构**: 融合以上三者，适配高性能匹配引擎场景

---

## 附录B: 重构检查清单

### 代码质量
- [ ] 所有模块有清晰的文档注释
- [ ] 所有公共API有示例代码
- [ ] 遵循Rust命名规范
- [ ] 无clippy警告
- [ ] 代码格式化 (cargo fmt)

### 测试覆盖
- [ ] 单元测试覆盖核心逻辑
- [ ] 集成测试验证层间交互
- [ ] 基准测试无性能退化
- [ ] 示例程序可正常运行

### 文档更新
- [ ] README.md 反映新架构
- [ ] 创建 ARCHITECTURE.md
- [ ] API文档完整
- [ ] 迁移指南（如需）

### 性能验证
- [ ] 运行所有基准测试
- [ ] 对比重构前后性能
- [ ] 无性能退化 (允许±3%误差)
- [ ] 内存使用无显著增加

---

**文档版本**: v1.0
**创建时间**: 2025-11-12
**状态**: 待审核 → 执行中
