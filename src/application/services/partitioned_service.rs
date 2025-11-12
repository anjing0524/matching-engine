/// Partitioned Service - Multi-threaded Parallel Matching Engine
///
/// This service implements a partitioned architecture for high-throughput order matching.
/// Orders are routed to worker threads based on symbol hash, ensuring lock-free processing.
///
/// ## Architecture
/// - **Partitioning**: Each symbol is consistently routed to the same partition
/// - **Lock-free**: No shared state between partitions, uses crossbeam channels
/// - **CPU Affinity**: Optional core pinning for reduced cache misses
/// - **Batch Processing**: Batches orders for improved throughput
///
/// ## Performance
/// - Target: 1M+ orders/sec
/// - Horizontal scaling: Performance scales linearly with CPU cores
/// - Zero-lock contention: Each partition is independent
///
/// ## Usage
/// ```rust
/// use matching_engine::application::services::{PartitionedService, PartitionConfig};
///
/// let config = PartitionConfig::default(); // Uses CPU count
/// let service = PartitionedService::new(config);
///
/// service.submit_order(order).unwrap();
/// ```

use crate::orderbook::OrderBook; // TODO: Replace with domain layer trait
use crate::shared::protocol::{NewOrderRequest, OrderConfirmation, TradeNotification};
use crate::shared::symbol_pool::SymbolPool;
use crossbeam::channel::{bounded, Sender, Receiver};
use smallvec::SmallVec;
use std::sync::Arc;
use std::thread;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Configuration for the partitioned service
#[derive(Clone, Debug)]
pub struct PartitionConfig {
    /// Number of partitions (recommended: number of CPU cores)
    pub partition_count: usize,

    /// Queue capacity per partition
    pub queue_capacity: usize,

    /// Batch size for processing
    pub batch_size: usize,

    /// Enable CPU core affinity binding
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

/// Internal representation of an order request
pub struct OrderRequest {
    pub request: NewOrderRequest,
    /// Optional response channel for synchronous operation
    pub response_tx: Option<Sender<OrderResponse>>,
}

/// Response from order processing
#[derive(Debug)]
pub enum OrderResponse {
    Matched {
        trades: SmallVec<[TradeNotification; 8]>,
        confirmation: Option<OrderConfirmation>,
    },
    Error(String),
}

/// Statistics for a partition
#[derive(Debug, Clone, Default)]
pub struct PartitionStats {
    pub orders_processed: u64,
    pub trades_generated: u64,
    pub queue_depth: usize,
}

/// Worker for a single partition
struct PartitionWorker {
    #[allow(dead_code)] // Used in cpu-affinity feature
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

    /// Main processing loop for the partition
    fn run(mut self) {
        // Bind to CPU core if enabled
        if self.config.enable_cpu_affinity {
            #[cfg(feature = "cpu-affinity")]
            {
                use core_affinity::CoreId;
                if let Some(core_ids) = core_affinity::get_core_ids() {
                    if self.partition_id < core_ids.len() {
                        core_affinity::set_for_current(core_ids[self.partition_id]);
                    }
                }
            }
        }

        loop {
            // Batch receive orders to reduce context switching
            let batch: Vec<OrderRequest> = self.rx
                .try_iter()
                .take(self.config.batch_size)
                .collect();

            if batch.is_empty() {
                // Adaptive wait strategy
                if self.stats.queue_depth > 0 {
                    // Backlog exists, keep spinning
                    std::hint::spin_loop();
                } else {
                    // Queue empty, yield briefly
                    std::thread::yield_now();
                }
                continue;
            }

            // Process orders in batch
            for order in batch {
                self.process_order(order);
            }

            // Update queue depth
            self.stats.queue_depth = self.rx.len();
        }
    }

    /// Process a single order
    #[inline]
    fn process_order(&mut self, order_req: OrderRequest) {
        // Match order
        let (trades, confirmation) = self.orderbook.match_order(order_req.request);

        // Update statistics
        self.stats.orders_processed += 1;
        self.stats.trades_generated += trades.len() as u64;

        // Send response if requested
        if let Some(response_tx) = order_req.response_tx {
            let response = OrderResponse::Matched { trades, confirmation };
            let _ = response_tx.send(response);
        }
    }
}

/// Partitioned Matching Service
///
/// Distributes order processing across multiple worker threads, each with its own
/// orderbook instance. Orders for the same symbol always go to the same partition,
/// ensuring consistency without locks.
pub struct PartitionedService {
    /// Sender channels for each partition
    partitions: Vec<Sender<OrderRequest>>,

    /// Shared symbol pool
    symbol_pool: Arc<SymbolPool>,

    /// Configuration
    config: PartitionConfig,

    /// Worker thread handles - must be kept alive
    #[allow(dead_code)]
    workers: Vec<thread::JoinHandle<()>>,
}

impl PartitionedService {
    /// Creates a new partitioned service
    ///
    /// # Arguments
    /// * `config` - Configuration for partitioning and performance tuning
    pub fn new(config: PartitionConfig) -> Self {
        // Use global symbol pool, shared across all partitions
        let symbol_pool = Arc::clone(crate::shared::symbol_pool::global_symbol_pool());

        // Preload common trading pairs
        symbol_pool.preload(&[
            "BTC/USD", "ETH/USD", "BNB/USD", "SOL/USD",
            "ADA/USD", "XRP/USD", "DOT/USD", "MATIC/USD",
        ]);

        let mut partitions = Vec::with_capacity(config.partition_count);
        let mut workers = Vec::with_capacity(config.partition_count);

        // Create workers for each partition
        for partition_id in 0..config.partition_count {
            let (tx, rx) = bounded(config.queue_capacity);
            partitions.push(tx);

            let worker = PartitionWorker::new(
                partition_id,
                rx,
                symbol_pool.clone(),
                config.clone(),
            );

            // Spawn worker thread
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

    /// Submits an order asynchronously (fire-and-forget)
    ///
    /// This is the fastest path, suitable for scenarios where response is not needed.
    ///
    /// # Arguments
    /// * `request` - The order request to submit
    ///
    /// # Returns
    /// * `Ok(())` if order was queued successfully
    /// * `Err(String)` if the partition queue is full or closed
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

    /// Submits an order and waits for response (synchronous)
    ///
    /// Suitable for scenarios requiring confirmation, but adds latency.
    ///
    /// # Arguments
    /// * `request` - The order request to submit
    ///
    /// # Returns
    /// * `Ok(OrderResponse)` with trades and confirmation
    /// * `Err(String)` on failure
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

    /// Submits a batch of orders asynchronously (high-performance API)
    ///
    /// Most efficient submission method compared to calling submit_order() in a loop:
    /// - Reduces function call overhead (N orders → 1 call)
    /// - Pre-groups orders by partition, reducing routing computation
    /// - Better CPU cache locality
    /// - Expected improvement: 20-40%
    ///
    /// # Arguments
    /// * `requests` - Vector of order requests to submit
    ///
    /// # Returns
    /// * `Ok(())` if all orders were queued successfully
    /// * `Err(String)` on failure
    pub fn submit_order_batch(&self, requests: Vec<NewOrderRequest>) -> Result<(), String> {
        if requests.is_empty() {
            return Ok(());
        }

        // Pre-allocate partition vectors
        let mut partitioned: Vec<Vec<OrderRequest>> = (0..self.config.partition_count)
            .map(|_| Vec::with_capacity(requests.len() / self.config.partition_count + 1))
            .collect();

        // Group by partition
        for request in requests {
            let partition_id = self.route_to_partition(&request.symbol);
            partitioned[partition_id].push(OrderRequest {
                request,
                response_tx: None,
            });
        }

        // Batch send to each partition
        for (partition_id, orders) in partitioned.into_iter().enumerate() {
            if orders.is_empty() {
                continue;
            }
            for order in orders {
                self.partitions[partition_id]
                    .send(order)
                    .map_err(|e| format!("Partition {} send failed: {}", partition_id, e))?;
            }
        }

        Ok(())
    }

    /// Routes an order to the appropriate partition
    ///
    /// Uses fast hashing to ensure the same symbol always routes to the same partition.
    #[inline]
    fn route_to_partition(&self, symbol: &Arc<str>) -> usize {
        // Use DefaultHasher (fast enough for routing)
        let mut hasher = DefaultHasher::new();
        symbol.hash(&mut hasher);
        let hash = hasher.finish();

        (hash as usize) % self.config.partition_count
    }

    /// Gets a reference to the symbol pool
    pub fn symbol_pool(&self) -> &Arc<SymbolPool> {
        &self.symbol_pool
    }

    /// Gets the number of partitions
    pub fn partition_count(&self) -> usize {
        self.config.partition_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::protocol::OrderType;

    #[test]
    fn test_partitioned_service_creation() {
        let config = PartitionConfig {
            partition_count: 4,
            queue_capacity: 100,
            batch_size: 10,
            enable_cpu_affinity: false,
        };

        let service = PartitionedService::new(config);
        assert_eq!(service.partition_count(), 4);
    }

    #[test]
    fn test_partitioned_service_basic_match() {
        let config = PartitionConfig {
            partition_count: 4,
            queue_capacity: 100,
            batch_size: 10,
            enable_cpu_affinity: false,
        };

        let service = PartitionedService::new(config);

        // Submit buy order
        let buy_order = NewOrderRequest {
            user_id: 1,
            symbol: Arc::from("BTC/USD"),
            order_type: OrderType::Buy,
            price: 50000,
            quantity: 10,
        };

        service.submit_order(buy_order).unwrap();

        // Submit sell order that matches
        let sell_order = NewOrderRequest {
            user_id: 2,
            symbol: Arc::from("BTC/USD"),
            order_type: OrderType::Sell,
            price: 50000,
            quantity: 10,
        };

        let response = service.submit_order_sync(sell_order).unwrap();

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
        let service = PartitionedService::new(config);

        let symbol = Arc::from("BTC/USD");

        // Same symbol should always route to same partition
        let partition1 = service.route_to_partition(&symbol);
        let partition2 = service.route_to_partition(&symbol);
        let partition3 = service.route_to_partition(&symbol);

        assert_eq!(partition1, partition2);
        assert_eq!(partition2, partition3);
    }

    #[test]
    fn test_different_symbols_distribution() {
        let config = PartitionConfig {
            partition_count: 4,
            ..Default::default()
        };
        let service = PartitionedService::new(config);

        let symbols = vec![
            "BTC/USD", "ETH/USD", "BNB/USD", "SOL/USD",
            "ADA/USD", "XRP/USD", "DOT/USD", "MATIC/USD",
        ];

        let mut partition_counts = vec![0; 4];

        for symbol in symbols {
            let partition = service.route_to_partition(&Arc::from(symbol));
            partition_counts[partition] += 1;
        }

        // Verify relatively even distribution (at least one per partition)
        for count in partition_counts {
            assert!(count > 0, "Partition should have at least one symbol");
        }
    }

    #[test]
    fn test_batch_submission() {
        let config = PartitionConfig {
            partition_count: 2,
            queue_capacity: 1000,
            batch_size: 10,
            enable_cpu_affinity: false,
        };

        let service = PartitionedService::new(config);

        // Create a batch of orders
        let orders: Vec<NewOrderRequest> = (0..100)
            .map(|i| NewOrderRequest {
                user_id: i,
                symbol: Arc::from("BTC/USD"),
                order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
                price: 50000,
                quantity: 1,
            })
            .collect();

        // Submit batch
        service.submit_order_batch(orders).unwrap();

        // Give workers time to process
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
