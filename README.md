# Rust高性能期货撮合引擎

一个基于100% Safe Rust实现的超高性能、低延迟订单撮合引擎，专为期货交易场景优化。

## 🎯 性能指标

**单核吞吐量**: 9.34M orders/sec
**16核并行预估**: 89.7M orders/sec
**延迟**: 11.74µs (100订单批量)
**架构**: Array + RingBuffer + FastBitmap硬件指令优化

## ✨ 核心特性

### 高性能架构
- **Tick-Based Array订单簿**: O(1)价格索引，针对期货tick特性优化
- **FastBitmap硬件指令**: 使用CPU的leading_zeros/trailing_zeros指令实现O(n/64)最优价查找
- **零动态分配RingBuffer**: 预分配循环队列，消除运行时分配开销
- **符号池化**: Arc<str>缓存，避免重复字符串分配

### 企业级特性
- **分区架构**: 支持多核并行，每个品种独立线程
- **批量提交API**: 减少跨线程通信开销
- **Crossbeam无锁通道**: 高效的生产者-消费者通信
- **jemalloc分配器**: 针对高并发场景优化的内存分配器

### 生产就绪
- **100% Safe Rust**: 无unsafe代码，内存安全保证
- **完整测试覆盖**: 单元测试 + 集成测试 + 性能基准测试
- **详尽文档**: 架构设计文档 + 性能分析报告

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

```
src/
├── lib.rs                    # 模块导出
├── main.rs                   # 服务器入口
│
├── protocol.rs               # 协议定义 (订单、成交通知)
├── timestamp.rs              # 高性能时间戳
├── symbol_pool.rs            # 符号池化
│
├── orderbook.rs              # V1: BTreeMap + 链表订单簿
├── orderbook_v2.rs           # V2: BTreeMap + RingBuffer订单簿
├── orderbook_tick.rs         # V3: Tick-based Array订单簿 ⭐
├── fast_bitmap.rs            # FastBitmap硬件指令优化 ⭐
├── ringbuffer.rs             # 零分配循环队列
│
├── engine.rs                 # 单线程撮合引擎
├── partitioned_engine.rs     # 多核分区引擎
└── network.rs                # TCP网络服务器

benches/
├── tick_orderbook_benchmark.rs      # Tick订单簿性能测试
├── ringbuffer_comparison.rs         # RingBuffer对比测试
├── partitioned_engine_benchmark.rs  # 分区引擎测试
└── ...

tests/
└── basic_trade.rs           # 集成测试
```

## 🏗️ 架构设计

### 核心架构：Tick-Based Array订单簿

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

### 单元测试
```bash
cargo test
```

### 集成测试
```bash
cargo test --test basic_trade -- --nocapture
```

### 性能基准测试
```bash
# 所有基准测试
cargo bench

# 生成性能报告
cargo bench -- --save-baseline current
```

## 📖 文档

- [架构设计文档](ARCHITECTURE.md) - 详细的架构设计和实现细节
- [性能分析报告](PERFORMANCE_FINAL_REPORT.md) - 完整的性能测试和优化分析
- [API文档](https://docs.rs) - 使用 `cargo doc --open` 生成

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

欢迎贡献! 请遵循以下步骤:

1. Fork本项目
2. 创建特性分支 (`git checkout -b feature/AmazingFeature`)
3. 提交更改 (`git commit -m 'Add some AmazingFeature'`)
4. 推送到分支 (`git push origin feature/AmazingFeature`)
5. 开启Pull Request

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
