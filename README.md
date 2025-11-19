# Rust高性能期货撮合引擎

一个基于100% Safe Rust实现的超高性能、低延迟订单撮合引擎，专为期货交易场景优化。采用**Hexagonal/Onion Architecture**的清晰分层设计，实现业务逻辑与技术实现的完全解耦。

## 🎯 性能指标

**单核吞吐量**: 9.34M orders/sec
**16核并行预估**: 89.7M orders/sec
**延迟**: 11.74µs (100订单批量)
**架构**: Tick-based Array + FastBitmap硬件指令优化
**设计模式**: Hexagonal/Onion Architecture (五层架构)

## ✨ 核心特性

### 商业级功能
- **订单授权验证**: 用户身份验证，防止未授权订单操作
- **订单状态查询**: 实时查询订单状态、剩余数量和部分成交信息
- **崩溃恢复机制**: WAL持久化 + 快照，10K订单36ms快速恢复
- **数据完整性保证**: CRC32校验，零数据丢失

### 架构设计
- **Hexagonal/Onion Architecture**: 清晰的五层架构，关注点分离
- **依赖倒置原则**: 外层依赖内层，领域层无外部依赖
- **零成本抽象**: Rust trait单态化，无运行时开销
- **高可测试性**: 依赖注入设计，易于mock和单元测试

### 高性能技术
- **Tick-Based Array订单簿**: O(1)价格索引，针对期货tick特性优化
- **FastBitmap硬件指令**: 使用CPU的POPCNT/TZCNT指令实现O(n/64)最优价查找
- **零动态分配RingBuffer**: 预分配循环队列，消除运行时分配开销
- **符号池化**: Arc<str>缓存，避免重复字符串分配

### 企业级特性
- **分区架构**: 支持多核并行，每个品种独立线程
- **批量提交API**: 减少跨线程通信开销
- **Crossbeam无锁通道**: 高效的生产者-消费者通信
- **业务规则验证**: OrderValidator支持配置化验证规则

### 商业级生产就绪
- **100% Safe Rust**: 无unsafe代码，内存安全保证
- **完整测试覆盖**: 112个测试全通过 ✅ (包含10个生产级WAL和崩溃恢复测试)
- **订单授权验证**: 防止恶意订单取消的安全机制
- **订单状态查询**: 实时追踪订单状态和部分成交信息
- **崩溃恢复**: 10K订单36ms恢复，WAL持久化零数据丢失
- **混沌工程验证**: 高并发、内存压力、锁竞争等极端场景测试
- **详尽文档**: 架构设计 + 性能分析 + 部署指南 + 贡献指南

## 📊 性能对比

### 架构演进

| 版本 | 架构 | 吞吐量 | vs V1 | 关键优化 |
|------|------|--------|-------|---------|
| V1 | BTreeMap + 链表 | 2.71M/s | - | 基线实现 |
| V2 | BTreeMap + RingBuffer | 3.59M/s | +32% | 零分配队列 |
| V3 | **Array + FastBitmap** | **9.34M/s** | **+245%** 🔥 | 硬件指令优化 |

### 详细性能数据

| 场景 | V1 (链表) | V2 (RingBuffer) | V3 (FastBitmap) | 最终提升 |
|------|----------|----------------|----------------|---------|
| 100订单批量 | 138.06µs | 25.66µs | **11.74µs** | **11.8x** 🔥 |
| 500订单批量 | 239.16µs | 130.40µs | **53.44µs** | **4.5x** 🔥 |
| 1000订单批量 | 369.20µs | 278.40µs | **107.09µs** | **3.4x** 🔥 |
| 深度订单簿 | 357.90µs | 357.90µs | **113.11µs** | **3.2x** 🔥 |
| 真实期货盘口 | - | 156.91µs | **94.70µs** | **1.7x** ✅ |

详细性能分析见: [PERFORMANCE_FINAL_REPORT.md](PERFORMANCE_FINAL_REPORT.md)

## 🚀 快速开始

### 系统要求
- Rust 1.70+ ([安装指南](https://www.rust-lang.org/tools/install))
- Linux/macOS (推荐) 或 Windows
- 支持x86_64或ARM64 CPU

### 编译与运行

```bash
# 克隆项目
git clone <repository-url>
cd matching-engine

# 开发编译
cargo build

# 发布编译（启用所有优化）
cargo build --release

# 运行撮合引擎服务器
cargo run --release
# 服务器监听 127.0.0.1:8080

# 运行集成测试
cargo test --test basic_trade -- --nocapture

# 运行性能基准测试
cargo bench
```

### 性能基准测试

```bash
# 完整基准测试套件
cargo bench

# 单独测试Tick-based订单簿
cargo bench --bench tick_orderbook_benchmark

# 测试RingBuffer性能
cargo bench --bench ringbuffer_comparison

# 分区引擎测试
cargo bench --bench partitioned_engine_benchmark
```

## 📁 项目结构

采用**DDD分层架构**设计（Domain-Driven Design + Hexagonal Architecture）:

```
src/
├── lib.rs                    # 模块导出与向后兼容层
├── main.rs                   # 程序入口 (thin wrapper)
│
├── interfaces/               # 🔵 Layer 5: 接口层 (Interfaces)
│   ├── cli/                  #   - CLI命令行接口
│   └── tools/                #   - 负载生成器等工具
│
├── application/              # 🟢 Layer 4: 应用层 (Application)
│   ├── use_cases/            #   业务用例 (编排领域逻辑)
│   │   ├── match_order.rs    #   - 订单撮合用例
│   │   └── cancel_order.rs   #   - 订单取消用例
│   └── services/             #   技术服务 (处理并发/通信)
│       ├── matching_service.rs         # - 单线程撮合服务
│       ├── partitioned_service.rs      # - 多线程分区服务
│       └── high_performance_service.rs # - 高性能优化服务
│
├── domain/                   # ⭐ Layer 3: 领域层 (Domain) - 核心
│   ├── orderbook/            #   订单簿模块
│   │   ├── traits.rs         #   - OrderBook trait (抽象接口)
│   │   ├── tick_based.rs     #   - TickBasedOrderBook (9.34M ops/s)
│   │   └── mod.rs            #   - ContractSpec等领域模型
│   └── validation.rs         #   - OrderValidator (业务规则验证)
│
├── infrastructure/           # 🟠 Layer 2: 基础设施层 (Infrastructure)
│   ├── network/              #   网络适配器
│   │   ├── backends/         #   - Tokio/io_uring/DPDK后端
│   │   ├── codec.rs          #   - 消息编解码
│   │   └── metrics.rs        #   - 网络指标
│   ├── persistence/          #   持久化
│   │   ├── wal.rs            #   - Write-Ahead Log
│   │   ├── snapshot.rs       #   - 快照管理
│   │   └── recovery.rs       #   - 崩溃恢复
│   └── observability/        #   可观测性
│       ├── health.rs         #   - 健康检查
│       ├── metrics_collector.rs  #   - 指标收集器
│       └── http_server.rs    #   - HTTP监控服务
│
├── shared/                   # 🟡 Layer 1: 共享层 (Shared)
│   ├── protocol.rs           #   - 协议数据结构
│   ├── symbol_pool.rs        #   - 符号池化
│   ├── collections/          #   - 高性能数据结构
│   │   ├── ringbuffer.rs     #   - 零分配循环队列
│   │   └── fast_bitmap.rs    #   - FastBitmap硬件指令
│   ├── metrics.rs            #   - 指标定义
│   └── timestamp.rs          #   - 高性能时间戳
│
└── legacy/                   # ⚠️  向后兼容层 (Legacy - Deprecated)
    ├── engine.rs             #   - 旧引擎实现 [@deprecated]
    ├── orderbook.rs          #   - 旧订单簿 (BTreeMap) [@deprecated]
    ├── orderbook_v2.rs       #   - V2订单簿 [@deprecated]
    ├── partitioned_engine.rs #   - 旧分区引擎 [@deprecated]
    └── network.rs            #   - 旧网络实现 [@deprecated]

benches/                      # 性能基准测试
├── tick_orderbook_benchmark.rs
├── comprehensive_benchmark.rs
├── partitioned_engine_benchmark.rs
└── ...                       # 13个基准测试

tests/                        # 集成测试 + 混沌测试
├── basic_trade.rs            # 基础交易测试
├── chaos_engineering_test.rs # 混沌工程测试 (7个场景)
├── performance_benchmark.rs  # 性能测试
└── ...                       # 91个单元/集成测试

docs/                         # 文档
├── ARCHITECTURE.md           # 架构设计文档 (v4.0)
├── DEPLOYMENT.md             # 生产部署指南 (789行)
├── CONTRIBUTING.md           # 贡献者指南 (722行)
├── PERFORMANCE_FINAL_REPORT.md  # 性能分析报告
└── ...                       # 20+文档文件
```

**依赖规则**:
```
Interfaces → Application → Domain ← Infrastructure
                         ↓
                      Shared (所有层可用)
                         ↓
                      Legacy (仅用于向后兼容)
```

## 🏗️ 架构设计

### 五层架构 (Hexagonal/Onion Architecture)

本项目采用清晰的分层架构设计，实现了业务逻辑与技术实现的完全解耦：

```
┌─────────────────────────────────────────┐
│   Interfaces (接口层)                     │  CLI, REST API, gRPC
├─────────────────────────────────────────┤
│   Application (应用层)                    │  Use Cases + Services
│     - MatchOrderUseCase<OB>             │  业务流程编排
│     - CancelOrderUseCase<OB>            │  依赖注入
├─────────────────────────────────────────┤
│   Domain (领域层) ⭐                      │  核心业务逻辑
│     - OrderBook trait                   │  纯业务规则
│     - TickBasedOrderBook (9.34M ops/s) │  无外部依赖
│     - OrderValidator                    │
├─────────────────────────────────────────┤
│   Infrastructure (基础设施层)             │  技术实现
│     - Tokio/io_uring/DPDK 网络          │  可替换适配器
│     - Crossbeam 通道                     │
├─────────────────────────────────────────┤
│   Shared (共享层)                         │  数据结构 + 工具
│     - Protocol, RingBuffer, FastBitmap │  跨层共享
└─────────────────────────────────────────┘
```

**架构优势**:
- ✅ **高可测试性**: 通过依赖注入轻松mock任何组件
- ✅ **易于扩展**: 可轻松添加新的OrderBook实现或网络后端
- ✅ **零成本抽象**: Rust泛型编译期单态化，无运行时开销
- ✅ **清晰职责**: 每层职责明确，易于理解和维护

详细架构设计见: **[ARCHITECTURE.md](ARCHITECTURE.md)** (v4.0 - 50+页详细文档)

### 核心数据结构：Tick-Based Array订单簿

```rust
pub struct TickBasedOrderBook {
    spec: ContractSpec,                    // 合约规格 (tick size, 价格范围)
    bid_levels: Vec<Option<RingBuffer>>,   // 买单数组 (O(1)索引)
    ask_levels: Vec<Option<RingBuffer>>,   // 卖单数组
    bid_bitmap: FastBitmap,                // 买单位图 (硬件指令查找)
    ask_bitmap: FastBitmap,                // 卖单位图
}
```

**关键设计理念:**

1. **Array索引 (O(1))**
   ```rust
   let index = (price - min_price) / tick_size;  // 直接算术计算
   let queue = &mut bid_levels[index];           // 数组访问
   ```

2. **FastBitmap硬件指令**
   ```rust
   // 查找最优买价: O(n/64) + 硬件指令
   pub fn find_last_one(&self) -> Option<usize> {
       for (idx, &block) in self.blocks.iter().enumerate().rev() {
           if block != 0 {
               return Some(idx * 64 + (63 - block.leading_zeros()));
           }
       }
   }
   ```

3. **RingBuffer零分配**
   ```rust
   pub struct RingBuffer<T> {
       buffer: Box<[MaybeUninit<T>]>,  // 预分配
       head: usize,
       tail: usize,
   }
   ```

详细架构见: [ARCHITECTURE.md](ARCHITECTURE.md)

## 🔧 技术栈

- **语言**: Rust 2021 Edition
- **并发**: Crossbeam (无锁通道)
- **网络**: Tokio (异步运行时)
- **序列化**: Bincode
- **内存分配器**: jemalloc
- **基准测试**: Criterion

## 📈 性能优化技术

### Phase 1: 基础优化
- ✅ 符号池化 (Arc<str>缓存)
- ✅ SmallVec (栈分配小向量)
- ✅ 时间戳缓存 (thread_local)

### Phase 2: 数据结构优化
- ✅ RingBuffer替代链表 (零分配)
- ✅ Tick-based Array (O(1)索引)
- ✅ FastBitmap硬件指令 (O(n/64)查找)

### Phase 3: 并发优化
- ✅ 分区架构 (多核并行)
- ✅ 批量提交API (减少通信开销)
- ✅ CPU亲和性绑定 (可选)

### 未来优化方向
- SIMD批量价格匹配 (AVX2/AVX512)
- Lock-Free SkipMap (替代BTreeMap)
- DPDK零拷贝网络
- FPGA硬件加速

## 🧪 测试

### 单元测试和集成测试
```bash
# 运行所有测试 (91个测试)
cargo test --lib --release

# 查看测试详情
cargo test --lib --release -- --nocapture
```

### 混沌工程测试
```bash
# 运行混沌测试 (7个极端场景)
cargo test --release --test chaos_engineering_test -- --ignored --nocapture

# 测试场景包括:
# - 高并发订单提交 (10客户端 × 1000订单)
# - 随机订单取消
# - 极端价格范围测试
# - 内存压力测试 (10,000订单)
# - 快速订单流转
# - 单一价格层压力
# - 并发锁竞争
```

### 性能基准测试
```bash
# 所有基准测试
cargo bench

# 生成性能报告
cargo bench -- --save-baseline current

# Tick-based订单簿对比
cargo bench --bench tick_orderbook_benchmark
```

## 📖 文档

### 核心文档
- **[架构设计文档](ARCHITECTURE.md)** - 详细的架构设计和实现细节 (v4.0)
- **[部署指南](DEPLOYMENT.md)** - 完整的生产部署指南 (789行)
- **[贡献指南](CONTRIBUTING.md)** - 开发者贡献规范 (722行)
- **[性能分析报告](PERFORMANCE_FINAL_REPORT.md)** - 完整的性能测试和优化分析

### API文档
```bash
# 生成并查看API文档
cargo doc --open

# 包含私有项的完整文档
cargo doc --document-private-items --open
```

### 便捷命令
```bash
# 使用cargo别名（在.cargo/config.toml中定义）
cargo docs              # 生成并打开文档
cargo docs-all          # 生成完整文档（含私有项）
cargo build-small       # 体积优化构建
cargo bench-quick       # 快速基准测试
```

## 🎯 适用场景

### 推荐场景
- ✅ 期货交易所 (价格tick离散)
- ✅ 期权交易所 (价格规律分布)
- ✅ 高频交易系统 (低延迟要求)
- ✅ 大规模订单簿 (1000+价格层)

### 技术要求
- 价格必须是离散的 (有固定tick size)
- 价格范围有合理上下限
- 单一品种单一线程模型

## 🔐 安全性

- **100% Safe Rust**: 无unsafe代码，编译时内存安全保证
- **无数据竞争**: 所有并发访问通过通道同步
- **溢出检查**: Debug模式下启用整数溢出检查

## ⚡ 性能调优建议

### 编译优化
```toml
[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
```

### 运行时配置
```bash
# 启用CPU亲和性
cargo run --release --features cpu-affinity

# 设置jemalloc参数
MALLOC_CONF=dirty_decay_ms:1000 cargo run --release
```

### 系统调优
```bash
# 增加文件描述符限制
ulimit -n 65535

# 禁用CPU频率调节
sudo cpupower frequency-set -g performance
```

## 📊 基准测试结果

运行环境:
- CPU: x86_64 (支持BSR/BSF指令)
- 内存: 16GB
- 操作系统: Linux 4.4.0
- Rust: 1.x (release编译)

最新基准测试结果详见: [PERFORMANCE_FINAL_REPORT.md](PERFORMANCE_FINAL_REPORT.md)

## 🤝 贡献

欢迎贡献！我们欢迎各种形式的贡献，包括但不限于：

- 🐛 Bug报告和修复
- ✨ 新功能和性能优化
- 📝 文档改进
- 🧪 测试用例补充

### 快速开始
1. Fork本项目
2. 创建特性分支 (`git checkout -b feature/AmazingFeature`)
3. 遵循代码规范和测试要求
4. 提交更改 (`git commit -m 'feat: Add some AmazingFeature'`)
5. 推送到分支 (`git push origin feature/AmazingFeature`)
6. 开启Pull Request

### 详细指南
请查看 **[贡献者指南](CONTRIBUTING.md)** 了解：
- 代码风格规范
- 提交信息格式（Conventional Commits）
- 测试要求（>80%覆盖率）
- Code Review流程
- 开发环境设置

## 📝 许可证

本项目采用MIT许可证 - 详见LICENSE文件

## 🙏 致谢

- 感谢Rust社区提供的优秀工具和库
- 感谢Crossbeam项目的无锁数据结构
- 感谢Criterion项目的性能基准测试框架

## 📞 联系方式

如有问题或建议，请通过Issue反馈。

---

**注意**: 本项目仅用于学习和研究目的。生产环境使用请充分测试并进行必要的安全审计。
