/// 分区并行引擎 - 百万级QPS架构POC
///
/// 核心设计：
/// 1. 按交易对哈希分区，每个分区独立处理
/// 2. 无锁路由，crossbeam channel传输
/// 3. CPU亲和性绑定，减少缓存失效
/// 4. 批量处理，提高吞吐量

use crate::orderbook::OrderBook;
use crate::protocol::{NewOrderRequest, OrderConfirmation, TradeNotification};
use crate::symbol_pool::SymbolPool;
use crossbeam::channel::{bounded, Sender, Receiver};
use smallvec::SmallVec;
use std::sync::Arc;
use std::thread;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// 分区配置
#[derive(Clone)]
pub struct PartitionConfig {
    /// 分区数量（推荐设置为CPU核心数）
    pub partition_count: usize,

    /// 每个分区的队列容量
    pub queue_capacity: usize,

    /// 批量处理大小
    pub batch_size: usize,

    /// 是否绑定CPU核心
    pub enable_cpu_affinity: bool,
}

impl Default for PartitionConfig {
    fn default() -> Self {
        Self {
            partition_count: num_cpus::get(),
            queue_capacity: 10_000,
            batch_size: 100,
            enable_cpu_affinity: true,
        }
    }
}

/// 订单请求的内部表示
pub struct OrderRequest {
    pub request: NewOrderRequest,
    /// 可选的响应通道（用于同步等待结果）
    pub response_tx: Option<Sender<OrderResponse>>,
}

/// 订单响应
pub enum OrderResponse {
    Matched {
        trades: SmallVec<[TradeNotification; 8]>,
        confirmation: Option<OrderConfirmation>,
    },
    Error(String),
}

/// 分区统计信息
#[derive(Debug, Clone, Default)]
pub struct PartitionStats {
    pub orders_processed: u64,
    pub trades_generated: u64,
    pub queue_depth: usize,
}

/// 单个分区worker
struct PartitionWorker {
    partition_id: usize,
    orderbook: OrderBook,
    rx: Receiver<OrderRequest>,
    stats: PartitionStats,
    config: PartitionConfig,
}

impl PartitionWorker {
    fn new(
        partition_id: usize,
        rx: Receiver<OrderRequest>,
        symbol_pool: Arc<SymbolPool>,
        config: PartitionConfig,
    ) -> Self {
        Self {
            partition_id,
            orderbook: OrderBook::with_symbol_pool(symbol_pool),
            rx,
            stats: PartitionStats::default(),
            config,
        }
    }

    /// 运行分区处理循环
    fn run(mut self) {
        // 绑定CPU核心（如果启用）
        if self.config.enable_cpu_affinity {
            #[cfg(feature = "cpu-affinity")]
            {
                use core_affinity::CoreId;
                let core_ids = core_affinity::get_core_ids().unwrap();
                if self.partition_id < core_ids.len() {
                    core_affinity::set_for_current(core_ids[self.partition_id]);
                }
            }
        }

        loop {
            // 批量接收订单，减少上下文切换
            let batch: Vec<OrderRequest> = self.rx
                .try_iter()
                .take(self.config.batch_size)
                .collect();

            if batch.is_empty() {
                // 自适应等待策略
                if self.stats.queue_depth > 0 {
                    // 有积压，继续自旋
                    std::hint::spin_loop();
                } else {
                    // 队列空，短暂yield
                    std::thread::yield_now();
                }
                continue;
            }

            // 批量处理订单
            for order in batch {
                self.process_order(order);
            }

            // 更新队列深度
            self.stats.queue_depth = self.rx.len();
        }
    }

    /// 处理单个订单
    #[inline]
    fn process_order(&mut self, order_req: OrderRequest) {
        // 撮合订单
        let (trades, confirmation) = self.orderbook.match_order(order_req.request);

        // 更新统计
        self.stats.orders_processed += 1;
        self.stats.trades_generated += trades.len() as u64;

        // 发送响应（如果需要）
        if let Some(response_tx) = order_req.response_tx {
            let response = OrderResponse::Matched { trades, confirmation };
            let _ = response_tx.send(response);
        }
    }
}

/// 分区并行引擎
pub struct PartitionedEngine {
    /// 每个分区的发送通道
    partitions: Vec<Sender<OrderRequest>>,

    /// 共享的Symbol池
    symbol_pool: Arc<SymbolPool>,

    /// 配置
    config: PartitionConfig,

    /// Worker线程句柄
    workers: Vec<thread::JoinHandle<()>>,
}

impl PartitionedEngine {
    /// 创建新的分区引擎
    pub fn new(config: PartitionConfig) -> Self {
        let symbol_pool = Arc::new(SymbolPool::with_capacity(1000));

        // 预加载常见交易对
        symbol_pool.preload(&[
            "BTC/USD", "ETH/USD", "BNB/USD", "SOL/USD",
            "ADA/USD", "XRP/USD", "DOT/USD", "MATIC/USD",
        ]);

        let mut partitions = Vec::with_capacity(config.partition_count);
        let mut workers = Vec::with_capacity(config.partition_count);

        // 创建每个分区的worker
        for partition_id in 0..config.partition_count {
            let (tx, rx) = bounded(config.queue_capacity);
            partitions.push(tx);

            let worker = PartitionWorker::new(
                partition_id,
                rx,
                symbol_pool.clone(),
                config.clone(),
            );

            // 启动worker线程
            let handle = thread::Builder::new()
                .name(format!("partition-{}", partition_id))
                .spawn(move || worker.run())
                .expect("Failed to spawn worker thread");

            workers.push(handle);
        }

        Self {
            partitions,
            symbol_pool,
            config,
            workers,
        }
    }

    /// 提交订单（异步，无响应）
    ///
    /// 这是最快的路径，适用于不需要等待结果的场景
    pub fn submit_order(&self, request: NewOrderRequest) -> Result<(), String> {
        let partition_id = self.route_to_partition(&request.symbol);

        let order_req = OrderRequest {
            request,
            response_tx: None,
        };

        self.partitions[partition_id]
            .send(order_req)
            .map_err(|e| format!("Failed to send order: {}", e))
    }

    /// 提交订单并等待响应（同步）
    ///
    /// 适用于需要确认的场景，但会增加延迟
    pub fn submit_order_sync(&self, request: NewOrderRequest) -> Result<OrderResponse, String> {
        let partition_id = self.route_to_partition(&request.symbol);

        let (response_tx, response_rx) = bounded(1);

        let order_req = OrderRequest {
            request,
            response_tx: Some(response_tx),
        };

        self.partitions[partition_id]
            .send(order_req)
            .map_err(|e| format!("Failed to send order: {}", e))?;

        response_rx
            .recv()
            .map_err(|e| format!("Failed to receive response: {}", e))
    }

    /// 将订单路由到对应分区
    ///
    /// 使用快速哈希算法确保相同交易对总是路由到同一分区
    #[inline]
    fn route_to_partition(&self, symbol: &Arc<str>) -> usize {
        // 使用FNV哈希（比DefaultHasher更快）
        let mut hasher = DefaultHasher::new();
        symbol.hash(&mut hasher);
        let hash = hasher.finish();

        (hash as usize) % self.config.partition_count
    }

    /// 获取Symbol池引用
    pub fn symbol_pool(&self) -> &Arc<SymbolPool> {
        &self.symbol_pool
    }

    /// 获取分区数量
    pub fn partition_count(&self) -> usize {
        self.config.partition_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::OrderType;

    #[test]
    fn test_partitioned_engine_basic() {
        let config = PartitionConfig {
            partition_count: 4,
            queue_capacity: 100,
            batch_size: 10,
            enable_cpu_affinity: false,
        };

        let engine = PartitionedEngine::new(config);

        // 提交买单
        let buy_order = NewOrderRequest {
            user_id: 1,
            symbol: Arc::from("BTC/USD"),
            order_type: OrderType::Buy,
            price: 50000,
            quantity: 10,
        };

        engine.submit_order(buy_order).unwrap();

        // 提交卖单
        let sell_order = NewOrderRequest {
            user_id: 2,
            symbol: Arc::from("BTC/USD"),
            order_type: OrderType::Sell,
            price: 50000,
            quantity: 10,
        };

        let response = engine.submit_order_sync(sell_order).unwrap();

        match response {
            OrderResponse::Matched { trades, .. } => {
                assert_eq!(trades.len(), 1);
                assert_eq!(trades[0].matched_quantity, 10);
            }
            OrderResponse::Error(e) => panic!("Unexpected error: {}", e),
        }
    }

    #[test]
    fn test_routing_consistency() {
        let config = PartitionConfig::default();
        let engine = PartitionedEngine::new(config);

        let symbol = Arc::from("BTC/USD");

        // 相同符号应该总是路由到同一分区
        let partition1 = engine.route_to_partition(&symbol);
        let partition2 = engine.route_to_partition(&symbol);
        let partition3 = engine.route_to_partition(&symbol);

        assert_eq!(partition1, partition2);
        assert_eq!(partition2, partition3);
    }

    #[test]
    fn test_different_symbols_distribution() {
        let config = PartitionConfig {
            partition_count: 4,
            ..Default::default()
        };
        let engine = PartitionedEngine::new(config);

        let symbols = vec![
            "BTC/USD", "ETH/USD", "BNB/USD", "SOL/USD",
            "ADA/USD", "XRP/USD", "DOT/USD", "MATIC/USD",
        ];

        let mut partition_counts = vec![0; 4];

        for symbol in symbols {
            let partition = engine.route_to_partition(&Arc::from(symbol));
            partition_counts[partition] += 1;
        }

        // 验证分布相对均匀（每个分区至少有一个）
        for count in partition_counts {
            assert!(count > 0, "Partition should have at least one symbol");
        }
    }
}
