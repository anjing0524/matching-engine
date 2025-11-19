# 性能优化实施指南

> 基于HFT性能评估报告的具体优化实施步骤

---

## 目录

1. [P0优化: 序列化库替换 (rkyv)](#p0-1-rkyv)
2. [P0优化: Lockfree Queue](#p0-2-lockfree-queue)
3. [P0优化: 对象池](#p0-3-object-pool)
4. [P1优化: 订单取消优化](#p1-1-cancel-order)
5. [P1优化: SIMD批量处理](#p1-2-simd)
6. [性能测试与验证](#testing)

---

## <a name="p0-1-rkyv"></a>P0-1: 序列化库替换 (rkyv)

### 背景

**当前瓶颈**: bincode序列化占28%总延迟 (0.60μs/2.16μs)
**优化目标**: 使用rkyv零拷贝序列化，降低30%序列化开销

### 实施步骤

#### 1. 添加依赖

```toml
# Cargo.toml
[dependencies]
rkyv = { version = "0.7", features = ["validation"] }
```

#### 2. 修改WalEntry定义

```rust
// src/infrastructure/persistence/types.rs

use rkyv::{Archive, Serialize, Deserialize};

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
#[archive(check_bytes)]
pub enum WalEntry {
    #[archive(check_bytes)]
    NewOrder {
        seq: u64,
        timestamp: u64,
        order: ArchivedNewOrderRequest,  // 使用rkyv版本
    },

    #[archive(check_bytes)]
    Trade {
        seq: u64,
        timestamp: u64,
        trade: ArchivedTradeNotification,
    },

    #[archive(check_bytes)]
    Confirmation {
        seq: u64,
        timestamp: u64,
        confirmation: ArchivedOrderConfirmation,
    },

    #[archive(check_bytes)]
    CancelOrder {
        seq: u64,
        timestamp: u64,
        order_id: u64,
    },
}
```

#### 3. 修改序列化代码

```rust
// src/infrastructure/persistence/async_wal.rs

fn write_entry(&mut self, entry: WalEntry) -> Result<()> {
    // 旧代码 (bincode)
    // let data = bincode::serde::encode_to_vec(&entry, standard())?;

    // 新代码 (rkyv - 零拷贝)
    let bytes = rkyv::to_bytes::<_, 256>(&entry)
        .map_err(|e| PersistenceError::Serialization(e.to_string()))?;

    // 计算CRC32
    let crc = crc32fast::hash(&bytes);

    // 写入长度
    let len = bytes.len() as u32;
    self.file.write_all(&len.to_le_bytes())?;

    // 写入序列号和时间戳
    self.file.write_all(&entry.seq().to_le_bytes())?;
    self.file.write_all(&entry.timestamp().to_le_bytes())?;

    // 写入CRC32
    self.file.write_all(&crc.to_le_bytes())?;

    // 写入数据
    self.file.write_all(&bytes)?;

    Ok(())
}
```

#### 4. 修改反序列化代码

```rust
// src/infrastructure/persistence/recovery.rs

fn read_entry(&mut self) -> Result<Option<WalEntry>> {
    // 读取长度
    let len = u32::from_le_bytes(...);

    // 读取数据
    let mut buffer = vec![0u8; len as usize];
    self.file.read_exact(&mut buffer)?;

    // 旧代码 (bincode)
    // let entry = bincode::serde::decode_from_slice(&buffer, standard())?;

    // 新代码 (rkyv - 零拷贝反序列化)
    let archived = rkyv::check_archived_root::<WalEntry>(&buffer)
        .map_err(|e| PersistenceError::Deserialization(e.to_string()))?;

    // 直接使用archived版本，或者反序列化
    let entry: WalEntry = archived.deserialize(&mut rkyv::Infallible)
        .map_err(|_| PersistenceError::Deserialization("Failed".to_string()))?;

    Ok(Some(entry))
}
```

### 预期收益

| 指标 | 优化前 | 优化后 | 提升 |
|------|--------|--------|------|
| 序列化延迟 | 0.60μs | 0.42μs | -30% |
| 总延迟 | 2.16μs | 1.88μs | -13% |
| 反序列化 | 拷贝 | 零拷贝 | 内存带宽节省 |

---

## <a name="p0-2-lockfree-queue"></a>P0-2: Lockfree Queue

### 背景

**当前瓶颈**: Tokio MPSC通道占21%延迟 (0.45μs)
**优化目标**: 使用crossbeam ArrayQueue，降低20%通道开销

### 实施步骤

#### 1. 添加依赖

```toml
# Cargo.toml
[dependencies]
crossbeam = "0.8"
```

#### 2. 修改PartitionedService

```rust
// src/application/services/partitioned_service.rs

use crossbeam::queue::ArrayQueue;
use std::sync::Arc;

pub struct PartitionedService {
    // 旧代码
    // partitions: Vec<Sender<OrderRequest>>,

    // 新代码 - lockfree queue
    partitions: Vec<Arc<ArrayQueue<OrderRequest>>>,

    workers: Vec<thread::JoinHandle<()>>,
}

impl PartitionedService {
    pub fn new(config: PartitionConfig) -> Self {
        let mut partitions = Vec::new();
        let mut workers = Vec::new();

        for partition_id in 0..config.partition_count {
            // 创建lockfree队列
            let queue = Arc::new(ArrayQueue::new(config.queue_capacity));
            partitions.push(queue.clone());

            // 启动worker
            let handle = thread::spawn(move || {
                Self::worker_loop(queue, partition_id);
            });

            workers.push(handle);
        }

        Self { partitions, workers }
    }

    /// Worker循环
    fn worker_loop(queue: Arc<ArrayQueue<OrderRequest>>, partition_id: usize) {
        let mut orderbook = OrderBook::new();

        loop {
            // 尝试从队列取订单
            match queue.pop() {
                Some(order_req) => {
                    // 处理订单
                    let (trades, conf) = orderbook.match_order(order_req.request);

                    // 发送响应（如果需要）
                    if let Some(tx) = order_req.response_tx {
                        let _ = tx.send(OrderResponse::Matched { trades, conf });
                    }
                }
                None => {
                    // 队列空，自适应等待
                    std::hint::spin_loop();
                }
            }
        }
    }

    /// 提交订单
    pub fn submit_order(&self, request: NewOrderRequest) -> Result<(), String> {
        let partition_id = self.route_to_partition(&request.symbol);

        let order_req = OrderRequest {
            request,
            response_tx: None,
        };

        // 旧代码
        // self.partitions[partition_id].send(order_req)

        // 新代码 - lockfree push
        self.partitions[partition_id]
            .push(order_req)
            .map_err(|_| "Queue full".to_string())
    }
}
```

### 预期收益

| 指标 | 优化前 | 优化后 | 提升 |
|------|--------|--------|------|
| 通道延迟 | 0.45μs | 0.36μs | -20% |
| 总延迟 | 1.88μs | 1.79μs | -5% |
| 吞吐量 | 463K | 550K | +19% |

---

## <a name="p0-3-object-pool"></a>P0-3: 对象池

### 背景

**问题**: 频繁分配NewOrderRequest、TradeNotification导致内存分配开销
**优化目标**: 使用对象池复用，减少90%内存分配

### 实施步骤

#### 1. 添加依赖

```toml
# Cargo.toml
[dependencies]
sharded-slab = "0.1"
```

#### 2. 创建对象池

```rust
// src/shared/object_pool.rs

use sharded_slab::Pool;
use crate::shared::protocol::{NewOrderRequest, TradeNotification};

lazy_static::lazy_static! {
    /// 订单对象池
    pub static ref ORDER_POOL: Pool<NewOrderRequest> = Pool::new();

    /// 成交对象池
    pub static ref TRADE_POOL: Pool<TradeNotification> = Pool::new();
}

/// 订单池化Wrapper
pub struct PooledOrder {
    key: usize,
    order: &'static mut NewOrderRequest,
}

impl PooledOrder {
    /// 从池中获取订单对象
    pub fn new() -> Option<Self> {
        ORDER_POOL.create(|| NewOrderRequest::default())
            .map(|(key, order)| Self { key, order })
    }

    /// 获取可变引用
    pub fn get_mut(&mut self) -> &mut NewOrderRequest {
        self.order
    }
}

impl Drop for PooledOrder {
    fn drop(&mut self) {
        // 归还到池（自动清理）
        ORDER_POOL.clear(self.key);
    }
}
```

#### 3. 使用对象池

```rust
// 使用示例

// 旧代码 - 每次分配
let order = NewOrderRequest {
    user_id: 1,
    symbol: Arc::from("BTC"),
    order_type: OrderType::Buy,
    price: 50000,
    quantity: 10,
};

// 新代码 - 从池中获取
let mut pooled = PooledOrder::new().unwrap();
{
    let order = pooled.get_mut();
    order.user_id = 1;
    order.symbol = Arc::from("BTC");
    order.order_type = OrderType::Buy;
    order.price = 50000;
    order.quantity = 10;
}

// pooled在离开作用域时自动归还池
```

### 预期收益

| 指标 | 优化前 | 优化后 | 提升 |
|------|--------|--------|------|
| 内存分配 | 100% | 10% | -90% |
| 延迟降低 | - | ~0.2μs | - |
| GC压力 | 高 | 低 | - |

---

## <a name="p1-1-cancel-order"></a>P1-1: 订单取消优化

### 背景

**当前问题**: cancel_order需要重建整个队列 - O(n)
**优化目标**: 实现O(1)物理删除

### 方案1: Slot-based RingBuffer with Free List

```rust
// src/shared/collections/slotted_ringbuffer.rs

/// 支持随机删除的Slot-based RingBuffer
pub struct SlottedRingBuffer<T> {
    /// Slot数组
    slots: Box<[Slot<T>]>,

    /// 空闲列表头
    free_head: Option<usize>,

    /// 已用列表头
    used_head: Option<usize>,

    /// 已用列表尾
    used_tail: Option<usize>,

    /// 元素数量
    len: usize,
}

struct Slot<T> {
    /// 数据
    data: MaybeUninit<T>,

    /// 下一个slot索引
    next: Option<usize>,

    /// 上一个slot索引
    prev: Option<usize>,

    /// 是否已用
    occupied: bool,
}

impl<T> SlottedRingBuffer<T> {
    /// 创建新的SlottedRingBuffer
    pub fn with_capacity(capacity: usize) -> Self {
        let mut slots: Vec<Slot<T>> = (0..capacity)
            .map(|i| Slot {
                data: MaybeUninit::uninit(),
                next: if i + 1 < capacity { Some(i + 1) } else { None },
                prev: if i > 0 { Some(i - 1) } else { None },
                occupied: false,
            })
            .collect();

        Self {
            slots: slots.into_boxed_slice(),
            free_head: Some(0),
            used_head: None,
            used_tail: None,
            len: 0,
        }
    }

    /// 入队 - O(1)
    pub fn push(&mut self, value: T) -> Result<usize, T> {
        // 从free list取一个slot
        let slot_idx = match self.free_head {
            Some(idx) => idx,
            None => return Err(value), // 满了
        };

        // 更新free list
        self.free_head = self.slots[slot_idx].next;

        // 写入数据
        self.slots[slot_idx].data.write(value);
        self.slots[slot_idx].occupied = true;

        // 加入used list尾部
        self.slots[slot_idx].prev = self.used_tail;
        self.slots[slot_idx].next = None;

        if let Some(tail_idx) = self.used_tail {
            self.slots[tail_idx].next = Some(slot_idx);
        }

        self.used_tail = Some(slot_idx);

        if self.used_head.is_none() {
            self.used_head = Some(slot_idx);
        }

        self.len += 1;

        Ok(slot_idx) // 返回slot索引，用于后续删除
    }

    /// 出队 - O(1)
    pub fn pop(&mut self) -> Option<T> {
        let head_idx = self.used_head?;

        // 读取数据
        let value = unsafe {
            self.slots[head_idx].data.assume_init_read()
        };

        self.remove(head_idx);

        Some(value)
    }

    /// 随机删除 - O(1) ⭐核心优化点
    pub fn remove(&mut self, slot_idx: usize) -> bool {
        if !self.slots[slot_idx].occupied {
            return false;
        }

        let slot = &mut self.slots[slot_idx];

        // 从used list移除
        match (slot.prev, slot.next) {
            (Some(prev_idx), Some(next_idx)) => {
                // 中间节点
                self.slots[prev_idx].next = Some(next_idx);
                self.slots[next_idx].prev = Some(prev_idx);
            }
            (Some(prev_idx), None) => {
                // 尾节点
                self.slots[prev_idx].next = None;
                self.used_tail = Some(prev_idx);
            }
            (None, Some(next_idx)) => {
                // 头节点
                self.slots[next_idx].prev = None;
                self.used_head = Some(next_idx);
            }
            (None, None) => {
                // 唯一节点
                self.used_head = None;
                self.used_tail = None;
            }
        }

        // 加入free list
        slot.next = self.free_head;
        slot.prev = None;
        slot.occupied = false;

        self.free_head = Some(slot_idx);
        self.len -= 1;

        true
    }

    /// 访问slot - O(1)
    pub fn get(&self, slot_idx: usize) -> Option<&T> {
        if self.slots[slot_idx].occupied {
            Some(unsafe { self.slots[slot_idx].data.assume_init_ref() })
        } else {
            None
        }
    }
}
```

### 方案2: 修改TickBasedOrderBook使用SlottedRingBuffer

```rust
// src/domain/orderbook/tick_based.rs

use crate::shared::collections::slotted_ringbuffer::SlottedRingBuffer;

/// 订单位置信息（增加slot_idx）
#[derive(Clone, Debug)]
struct OrderLocation {
    price_idx: usize,
    is_bid: bool,
    slot_idx: usize,  // 新增：Slot索引
}

pub struct TickBasedOrderBook {
    // 替换RingBuffer为SlottedRingBuffer
    bid_levels: Vec<Option<SlottedRingBuffer<OrderNode>>>,
    ask_levels: Vec<Option<SlottedRingBuffer<OrderNode>>>,

    // ... 其他字段不变
}

impl TickBasedOrderBook {
    /// 添加订单（返回slot索引）
    fn add_bid_order(&mut self, idx: usize, user_id: u64, quantity: u64, order_id: u64) {
        let order = OrderNode { ... };

        let queue = self.bid_levels[idx]
            .get_or_insert_with(|| SlottedRingBuffer::with_capacity(self.spec.queue_capacity));

        // push返回slot索引
        if let Ok(slot_idx) = queue.push(order) {
            // 记录slot索引
            self.order_locations.insert(order_id, OrderLocation {
                price_idx: idx,
                is_bid: true,
                slot_idx,  // 保存slot索引
            });

            self.bid_bitmap.set(idx, true);

            if self.best_bid_idx.is_none() || idx > self.best_bid_idx.unwrap() {
                self.best_bid_idx = Some(idx);
            }
        }
    }
}

// 实现OrderBook trait
impl OrderBook for TickBasedOrderBook {
    /// 取消订单 - O(1)优化版本
    fn cancel_order(&mut self, order_id: u64) -> Result<(), String> {
        // Step 1: 查找订单位置
        let location = self.order_locations.get(&order_id).cloned()
            .ok_or_else(|| format!("Order {} not found", order_id))?;

        // Step 2: 获取队列
        let levels = if location.is_bid {
            &mut self.bid_levels
        } else {
            &mut self.ask_levels
        };

        let queue = levels[location.price_idx]
            .as_mut()
            .ok_or_else(|| format!("Price level not found"))?;

        // Step 3: O(1)删除 ⭐核心优化
        if !queue.remove(location.slot_idx) {
            return Err(format!("Failed to remove order"));
        }

        // Step 4: 如果队列空了，更新位图
        if queue.is_empty() {
            if location.is_bid {
                self.bid_bitmap.set(location.price_idx, false);
                if self.best_bid_idx == Some(location.price_idx) {
                    self.best_bid_idx = self.find_best_bid();
                }
            } else {
                self.ask_bitmap.set(location.price_idx, false);
                if self.best_ask_idx == Some(location.price_idx) {
                    self.best_ask_idx = self.find_best_ask();
                }
            }
        }

        // Step 5: 清理映射
        self.order_locations.remove(&order_id);

        Ok(())
    }
}
```

### 预期收益

| 场景 | 优化前 | 优化后 | 提升 |
|------|--------|--------|------|
| 取消订单(队列深度10) | ~5μs | ~0.5μs | 10倍 |
| 取消订单(队列深度100) | ~50μs | ~0.5μs | 100倍 |
| 取消订单(队列深度1000) | ~500μs | ~0.5μs | 1000倍 |

---

## <a name="p1-2-simd"></a>P1-2: SIMD批量处理

### 背景

**场景**: 批量价格比较、数量累加
**优化目标**: 使用AVX2/AVX512向量化处理，提升50%批量性能

### 实施步骤

#### 1. 启用SIMD特性

```toml
# Cargo.toml
[features]
simd = []

[build]
rustflags = ["-C", "target-cpu=native"]
```

#### 2. 实现SIMD批量匹配

```rust
// src/domain/orderbook/simd_match.rs

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

/// SIMD批量价格比较 (AVX2)
///
/// 一次性比较4个价格是否 <= 限价
#[cfg(all(target_arch = "x86_64", feature = "simd"))]
pub unsafe fn batch_price_compare_avx2(prices: &[u64; 4], limit_price: u64) -> [bool; 4] {
    // 加载4个价格到AVX2寄存器
    let prices_vec = _mm256_loadu_si256(prices.as_ptr() as *const __m256i);

    // 广播限价到AVX2寄存器
    let limit_vec = _mm256_set1_epi64x(limit_price as i64);

    // 向量化比较: prices <= limit
    let cmp = _mm256_cmpgt_epi64(limit_vec, prices_vec);

    // 提取比较结果
    let mut result = [false; 4];
    let mask = _mm256_movemask_pd(_mm256_castsi256_pd(cmp));

    for i in 0..4 {
        result[i] = (mask & (1 << i)) != 0;
    }

    result
}

/// SIMD批量数量累加 (AVX2)
#[cfg(all(target_arch = "x86_64", feature = "simd"))]
pub unsafe fn batch_quantity_sum_avx2(quantities: &[u64]) -> u64 {
    let mut sum = 0u64;

    // 每次处理4个u64
    for chunk in quantities.chunks_exact(4) {
        let vec = _mm256_loadu_si256(chunk.as_ptr() as *const __m256i);

        // 水平累加
        let sum_vec = _mm256_add_epi64(vec, _mm256_srli_si256(vec, 8));
        let sum_vec = _mm256_add_epi64(sum_vec, _mm256_permute2x128_si256(sum_vec, sum_vec, 1));

        sum += _mm256_extract_epi64(sum_vec, 0) as u64;
    }

    // 处理剩余元素
    for &qty in quantities.chunks_exact(4).remainder() {
        sum += qty;
    }

    sum
}

/// 批量匹配（SIMD优化版本）
pub fn batch_match_orders_simd(
    orderbook: &mut TickBasedOrderBook,
    orders: &[NewOrderRequest],
) -> Vec<(Vec<TradeNotification>, Option<OrderConfirmation>)> {
    let mut results = Vec::with_capacity(orders.len());

    // TODO: 使用SIMD预处理订单
    // - 批量价格转索引
    // - 批量验证价格合法性
    // - 批量路由到价格层

    for order in orders {
        let result = orderbook.match_order(order.clone());
        results.push(result);
    }

    results
}
```

#### 3. 基准测试

```rust
#[cfg(all(test, target_arch = "x86_64", feature = "simd"))]
mod bench_simd {
    use super::*;
    use test::Bencher;

    #[bench]
    fn bench_price_compare_scalar(b: &mut Bencher) {
        let prices = [50000, 50001, 50002, 50003];
        let limit = 50002;

        b.iter(|| {
            let mut result = [false; 4];
            for i in 0..4 {
                result[i] = prices[i] <= limit;
            }
            result
        });
    }

    #[bench]
    fn bench_price_compare_simd(b: &mut Bencher) {
        let prices = [50000, 50001, 50002, 50003];
        let limit = 50002;

        b.iter(|| unsafe {
            batch_price_compare_avx2(&prices, limit)
        });
    }
}
```

### 预期收益

| 操作 | 标量 | SIMD | 提升 |
|------|------|------|------|
| 4个价格比较 | ~4 cycles | ~1 cycle | 4倍 |
| 批量累加(100个) | ~200 cycles | ~30 cycles | 6.7倍 |
| 批量匹配 | 基准 | +50% | - |

---

## <a name="testing"></a>性能测试与验证

### 运行HFT基准测试

```bash
# 运行所有HFT测试
cargo bench --bench hft_benchmark

# 只运行延迟测试
cargo bench --bench hft_benchmark -- hft_latency

# 只运行吞吐量测试
cargo bench --bench hft_benchmark -- hft_throughput

# 生成火焰图
cargo flamegraph --bench hft_benchmark
```

### 性能指标验证清单

- [ ] P50延迟 < 2μs
- [ ] P99延迟 < 5μs
- [ ] P99.9延迟 < 10μs
- [ ] 单核吞吐量 > 9M ops/s
- [ ] 带WAL吞吐量 > 500K ops/s
- [ ] 取消订单延迟 < 1μs (队列深度1000)
- [ ] 16核吞吐量 > 80M ops/s

### 回归测试

```bash
# 运行所有测试
cargo test --release

# 运行集成测试
cargo test --release --test '*'

# 运行崩溃恢复测试
cargo test --release crash_recovery
```

---

## 总结

本优化指南提供了从P0到P1的具体实施步骤。按照优先级依次实现：

1. **P0优化** (1-2周): rkyv + lockfree queue + 对象池
   - 预期收益: 总延迟降低25%, 吞吐量提升40%

2. **P1优化** (1-3个月): 订单取消 + SIMD
   - 预期收益: 取消延迟降低100倍, 批量性能提升50%

完成所有优化后，系统性能指标：
- **延迟**: ~1.5μs (P50), ~3μs (P99)
- **吞吐量**: 650K ops/s (带WAL), 12M ops/s (纯内存)
- **取消性能**: <1μs (任意队列深度)

---

**最后更新**: 2025-11-17
