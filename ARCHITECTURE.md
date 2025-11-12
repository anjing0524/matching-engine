# 撮合引擎架构设计文档

## 1. 系统架构概览

### 1.1 整体设计

本项目是一个**高性能期货交易撮合引擎**，采用100% Safe Rust实现，设计目标是单核处理900万+ orders/sec。系统采用**分层解耦的Actor模式**架构：

```
┌─────────────────────────────────────────────────────────────────┐
│                     异步网络层 (Tokio)                           │
│         多客户端并发连接 - Length-Delimited Codec                │
├─────────────────────────────────────────────────────────────────┤
│                   ↓ MPSC Unbounded Channels                     │
├─────────────────────────────────────────────────────────────────┤
│            分区撮合引擎 (Partitioned Engine)                      │
│              Crossbeam无锁通道 + 多核并行                         │
│                                                                 │
│  Partition 0    Partition 1    ...    Partition N              │
│  TickOrderBook  TickOrderBook  ...    TickOrderBook            │
│  (独立线程)     (独立线程)             (独立线程)                 │
├─────────────────────────────────────────────────────────────────┤
│                   ↓ Broadcast Channel                           │
├─────────────────────────────────────────────────────────────────┤
│               成交通知广播 (发送到所有客户端)                      │
└─────────────────────────────────────────────────────────────────┘
```

### 1.2 核心设计原则

1. **零拷贝**: 预分配内存，避免运行时动态分配
2. **无锁并发**: Crossbeam SPSC通道，分区隔离
3. **硬件加速**: 利用CPU硬件指令(leading_zeros/trailing_zeros)
4. **缓存友好**: 连续内存布局，提升CPU缓存命中率
5. **类型安全**: 100% Safe Rust，编译期内存安全保证

---

## 2. 订单簿架构演进

### 2.1 V1: BTreeMap + 链表 (Baseline)

```rust
pub struct OrderBook {
    bids: BTreeMap<u64, VecDeque<Order>>,  // 价格 → 订单队列
    asks: BTreeMap<u64, VecDeque<Order>>,
}
```

**性能**: 2.71M orders/sec
**问题**:
- VecDeque动态分配开销大
- 链表指针追踪导致缓存miss
- 大量malloc/free调用

### 2.2 V2: BTreeMap + RingBuffer

```rust
pub struct OrderBookV2 {
    bids: BTreeMap<u64, RingBuffer<OrderNode>>,
    asks: BTreeMap<u64, RingBuffer<OrderNode>>,
}

pub struct RingBuffer<T> {
    buffer: Box<[MaybeUninit<T>]>,  // 预分配
    capacity: usize,
    head: usize,
    tail: usize,
}
```

**性能**: 3.59M orders/sec (+32%)
**优势**:
- 零动态分配 (MaybeUninit避免初始化)
- 连续内存，缓存友好
- O(1) push/pop

**问题**:
- BTreeMap仍然是O(log n)查找

### 2.3 V3: Tick-Based Array + FastBitmap (最优方案) ⭐

```rust
pub struct TickBasedOrderBook {
    spec: ContractSpec,                          // 合约规格
    bid_levels: Vec<Option<RingBuffer<OrderNode>>>, // O(1)数组索引
    ask_levels: Vec<Option<RingBuffer<OrderNode>>>,
    bid_bitmap: FastBitmap,                      // 硬件指令查找
    ask_bitmap: FastBitmap,
    best_bid_idx: Option<usize>,                 // 缓存最优价
    best_ask_idx: Option<usize>,
}
```

**性能**: 9.34M orders/sec (+160% vs V2, +245% vs V1)
**核心优化**:
1. **Array O(1)索引**: `(price - min_price) / tick_size`
2. **硬件指令查找**: leading_zeros/trailing_zeros
3. **位图稀疏优化**: 6000价格层 = 94个u64块

---

## 3. FastBitmap硬件指令优化

### 3.1 数据结构

```rust
pub struct FastBitmap {
    blocks: Vec<u64>,  // 每块64个bit
    len: usize,
}
```

**内存布局**:
```
价格层0-63:   block[0] = 0b00...1001  (bit 0, 3设置)
价格层64-127: block[1] = 0b00...0010  (bit 1设置)
...
价格层5952-6015: block[93] = 0b10...0000 (bit 63设置)
```

### 3.2 硬件指令实现

**查找最优买价 (最高价)**:

```rust
#[inline]
pub fn find_last_one(&self) -> Option<usize> {
    // 从高到低遍历u64块
    for (block_idx, &block) in self.blocks.iter().enumerate().rev() {
        if block != 0 {
            // 使用硬件指令 - x86: BSR, ARM: CLZ
            let bit_offset = 63 - block.leading_zeros() as usize;
            return Some(block_idx * 64 + bit_offset);
        }
    }
    None
}
```

**查找最优卖价 (最低价)**:

```rust
#[inline]
pub fn find_first_one(&self) -> Option<usize> {
    // 从低到高遍历u64块
    for (block_idx, &block) in self.blocks.iter().enumerate() {
        if block != 0 {
            // 使用硬件指令 - x86: BSF, ARM: CTZ
            let bit_offset = block.trailing_zeros() as usize;
            return Some(block_idx * 64 + bit_offset);
        }
    }
    None
}
```

**复杂度分析**:
- 6000价格层 = 94个u64块
- 最坏情况: 94次比较 + 1次硬件指令
- 时间: ~100-300 CPU周期 vs BitVec的 ~60K周期
- **提升: 200-600倍**

### 3.3 CPU硬件指令映射

| 操作 | x86指令 | ARM指令 | 延迟 |
|------|---------|---------|------|
| leading_zeros | BSR (Bit Scan Reverse) | CLZ (Count Leading Zeros) | 1-3 cycles |
| trailing_zeros | BSF (Bit Scan Forward) | CTZ (Count Trailing Zeros) | 1-3 cycles |

---

## 4. 撮合算法

### 4.1 价格-时间优先

**核心规则**:
1. 买单按价格**从高到低**排序
2. 卖单按价格**从低到高**排序
3. 同价格按**时间优先** (FIFO)

### 4.2 撮合流程

```rust
pub fn match_order(&mut self, request: NewOrderRequest)
    -> (SmallVec<[TradeNotification; 8]>, Option<OrderConfirmation>)
{
    let mut trades = SmallVec::new();
    let mut remaining = request.quantity;

    match request.order_type {
        OrderType::Buy => {
            // 1. 从最优卖价开始匹配
            while let Some(ask_idx) = self.best_ask_idx {
                let ask_price = self.index_to_price(ask_idx);

                // 2. 价格检查
                if ask_price > request.price {
                    break;  // 无法成交
                }

                // 3. 从队列头部取订单
                if let Some(queue) = &mut self.ask_levels[ask_idx] {
                    while let Some(counter_order) = queue.front_mut() {
                        let trade_qty = min(remaining, counter_order.quantity);

                        // 4. 生成成交通知
                        trades.push(TradeNotification { ... });

                        // 5. 更新数量
                        remaining -= trade_qty;
                        counter_order.quantity -= trade_qty;

                        if counter_order.quantity == 0 {
                            queue.pop();  // 完全成交，移除
                        }

                        if remaining == 0 {
                            return (trades, None);  // 完全成交
                        }
                    }
                }

                // 6. 更新最优价
                self.best_ask_idx = self.find_best_ask();
            }

            // 7. 未完全成交，挂单
            if remaining > 0 {
                self.add_bid_order(request_idx, user_id, remaining);
            }
        }
        OrderType::Sell => { /* 对称逻辑 */ }
    }

    (trades, confirmation)
}
```

### 4.3 关键优化点

1. **最优价缓存**: `best_bid_idx/best_ask_idx` 避免重复查找
2. **SmallVec**: 栈分配成交通知数组 (8个内联)
3. **前置检查**: 价格检查在队列遍历之前
4. **批量更新**: 位图标记延迟到队列为空时

---

## 5. 分区引擎架构

### 5.1 分区策略

```rust
pub struct PartitionedEngine {
    partitions: Vec<Sender<OrderRequest>>,
    partition_count: usize,
}

impl PartitionedEngine {
    fn route_to_partition(&self, symbol: &str) -> usize {
        // 基于符号哈希的一致性路由
        let mut hasher = DefaultHasher::new();
        symbol.hash(&mut hasher);
        (hasher.finish() as usize) % self.partition_count
    }
}
```

**优势**:
- 每个品种固定分配到一个分区
- 分区内单线程，无锁
- 品种间并行处理

### 5.2 批量提交API

```rust
pub fn submit_order_batch(&self, requests: Vec<NewOrderRequest>)
    -> Result<(), String>
{
    // 1. 预分配per-partition向量
    let mut partitioned: Vec<Vec<OrderRequest>> =
        (0..self.partition_count)
            .map(|_| Vec::with_capacity(requests.len() / self.partition_count))
            .collect();

    // 2. 按分区分组
    for request in requests {
        let partition_id = self.route_to_partition(&request.symbol);
        partitioned[partition_id].push(OrderRequest { ... });
    }

    // 3. 批量发送
    for (partition_id, orders) in partitioned.into_iter().enumerate() {
        for order in orders {
            self.partitions[partition_id].send(order)?;
        }
    }

    Ok(())
}
```

---

## 6. 性能优化技术

### 6.1 内存分配优化

| 技术 | 实现 | 收益 |
|------|------|------|
| RingBuffer预分配 | `Box<[MaybeUninit<T>]>` | 零运行时分配 |
| SmallVec | 栈分配8个元素 | 避免堆分配 |
| 符号池化 | `Arc<str>` 缓存 | 字符串零拷贝 |
| 位图索引 | `Vec<u64>` | 固定内存占用 |

### 6.2 CPU优化

| 技术 | 原理 | 收益 |
|------|------|------|
| 硬件指令 | BSR/BSF/CLZ/CTZ | 200-600x |
| 缓存局部性 | 连续数组布局 | 减少cache miss |
| 分支预测 | 小循环 + 可预测分支 | 提升流水线效率 |
| SIMD潜力 | 连续内存 | 未来可批量处理 |

### 6.3 并发优化

| 技术 | 实现 | 收益 |
|------|------|------|
| SPSC通道 | Crossbeam | 无锁通信 |
| 分区隔离 | 品种级别 | 零竞争 |
| CPU亲和性 | core_affinity | 减少上下文切换 |

---

## 7. 代码组织

### 7.1 核心模块

```
src/
├── orderbook.rs          # V1: BTreeMap + 链表
├── orderbook_v2.rs       # V2: BTreeMap + RingBuffer
├── orderbook_tick.rs     # V3: Tick-based Array ⭐
├── fast_bitmap.rs        # FastBitmap硬件指令 ⭐
├── ringbuffer.rs         # 零分配循环队列
├── symbol_pool.rs        # 符号池化
├── timestamp.rs          # 高性能时间戳
├── partitioned_engine.rs # 分区引擎
└── engine.rs             # 单线程引擎
```

### 7.2 基准测试

```
benches/
├── tick_orderbook_benchmark.rs       # V1/V2/V3对比
├── ringbuffer_comparison.rs          # RingBuffer vs VecDeque
├── partitioned_engine_benchmark.rs   # 多核性能
└── ...
```

---

## 8. 性能基准测试

### 8.1 测试环境

- **CPU**: x86_64 (支持BSR/BSF指令)
- **内存**: 16GB
- **OS**: Linux 4.4.0
- **编译**: `cargo build --release` (opt-level=3, lto=fat)

### 8.2 单核性能

| 场景 | V1 | V2 | V3 | 提升 |
|------|----|----|----|----|
| 100订单 | 138µs | 26µs | **12µs** | **11.8x** |
| 1000订单 | 369µs | 278µs | **107µs** | **3.4x** |
| 深度簿 | 358µs | 358µs | **113µs** | **3.2x** |

**吞吐量**: 9.34M orders/sec

### 8.3 多核扩展

**理论计算**:
```
单核: 9.34M
16核: 9.34M × 16 × 0.6 (效率) = 89.7M orders/sec
```

**实际测试**: 待补充16核完整压测数据

---

## 9. 适用场景

### 9.1 ✅ 推荐场景

- **期货交易所**: 价格有固定tick size
- **期权交易所**: 行权价离散分布
- **高频交易**: 延迟敏感型应用
- **大规模订单簿**: 1000+活跃价格层

### 9.2 ⚠️ 限制

- 价格必须是离散的 (tick_size已知)
- 价格范围需要合理边界 (避免数组过大)
- 单品种单线程模型 (跨品种通过分区并行)

### 9.3 ❌ 不推荐场景

- 股票交易 (价格连续，无固定tick)
- 价格范围未知/动态扩展场景
- 需要跨品种原子操作的场景

---

## 10. 未来优化方向

### 10.1 P0 - 生产就绪

- [x] Tick-based Array订单簿
- [x] FastBitmap硬件指令
- [ ] 16核完整性能测试
- [ ] 生产环境压测

### 10.2 P1 - 性能提升

- [ ] SIMD批量价格匹配 (AVX2/AVX512)
- [ ] Lock-Free SkipMap (替代分区内BTreeMap)
- [ ] 每品种CPU核心绑定
- [ ] 零拷贝网络 (DPDK)

### 10.3 P2 - 探索性

- [ ] FPGA硬件加速
- [ ] GPU批量撮合
- [ ] 机器学习订单预测

---

## 11. 参考资料

### 11.1 关键算法

- **Tick-based Array**: 利用期货价格离散特性
- **FastBitmap**: x86 BSR/BSF指令，ARM CLZ/CTZ指令
- **SPSC RingBuffer**: 单生产者单消费者无锁队列

### 11.2 性能优化技巧

1. **避免分配**: MaybeUninit + 预分配
2. **缓存友好**: 连续内存 + 小数据结构
3. **硬件加速**: 利用CPU指令
4. **减少分支**: 提升流水线效率
5. **分区并行**: 无锁架构

### 11.3 Rust特性利用

- **零成本抽象**: inline + 单态化
- **所有权系统**: 编译期内存安全
- **类型系统**: 防止数据竞争
- **unsafe零使用**: 100% Safe Rust

---

**文档版本**: v3.0
**最后更新**: 2025-11-12
**维护者**: Matching Engine Team
