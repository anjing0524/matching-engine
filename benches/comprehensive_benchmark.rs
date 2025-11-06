/// Comprehensive Performance Benchmark Suite
/// Tests all key components of the matching engine based on system design principles:
/// 1. OrderBook matching (core latency)
/// 2. Memory allocation patterns (free list efficiency)
/// 3. Network framing overhead (LengthDelimitedCodec)
/// 4. Serialization costs (serde_json)
/// 5. Price level lookups (BTreeMap performance)

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion, Throughput, BenchmarkId};
use matching_engine::orderbook::OrderBook;
use matching_engine::protocol::{NewOrderRequest, OrderType, TradeNotification, OrderConfirmation};
use std::sync::Arc;

/// ============================================================================
/// 1. CORE MATCHING PERFORMANCE
/// ============================================================================

/// Benchmark: Single order add (no matching)
/// Tests: OrderNode allocation + Vec.push + BTreeMap insertion
fn bench_order_add_no_match(c: &mut Criterion) {
    let mut group = c.benchmark_group("OrderBook - Add Order (No Match)");
    group.throughput(Throughput::Elements(1));

    group.bench_function("single_order_add", |b| {
        b.iter_batched(
            || OrderBook::new(),
            |mut book| {
                let order = NewOrderRequest {
                    user_id: 1,
                    symbol: Arc::from("BTC/USD"),
                    order_type: OrderType::Buy,
                    price: black_box(50000),
                    quantity: black_box(100),
                };
                book.match_order(order);
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Benchmark: Full match (buyer meets seller)
/// Tests: BTreeMap lookup + linked list traversal + Trade generation
fn bench_full_match(c: &mut Criterion) {
    let mut group = c.benchmark_group("OrderBook - Full Match");
    group.throughput(Throughput::Elements(1));

    group.bench_function("buyer_seller_full_match", |b| {
        b.iter_batched(
            || {
                let mut book = OrderBook::new();
                // Pre-populate with a sell order
                book.match_order(NewOrderRequest {
                    user_id: 2,
                    symbol: Arc::from("BTC/USD"),
                    order_type: OrderType::Sell,
                    price: 50000,
                    quantity: 100,
                });
                book
            },
            |mut book| {
                let buy_order = NewOrderRequest {
                    user_id: 1,
                    symbol: Arc::from("BTC/USD"),
                    order_type: OrderType::Buy,
                    price: black_box(50000),
                    quantity: black_box(100),
                };
                book.match_order(buy_order);
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Benchmark: Partial match
/// Tests: Quantity reduction + remaining order insertion
fn bench_partial_match(c: &mut Criterion) {
    let mut group = c.benchmark_group("OrderBook - Partial Match");
    group.throughput(Throughput::Elements(1));

    group.bench_function("partial_match_50pct", |b| {
        b.iter_batched(
            || {
                let mut book = OrderBook::new();
                let (_, _) = book.match_order(NewOrderRequest {
                    user_id: 2,
                    symbol: Arc::from("BTC/USD"),
                    order_type: OrderType::Sell,
                    price: 50000,
                    quantity: 100,
                });
                book
            },
            |mut book| {
                let buy_order = NewOrderRequest {
                    user_id: 1,
                    symbol: Arc::from("BTC/USD"),
                    order_type: OrderType::Buy,
                    price: black_box(50000),
                    quantity: black_box(50), // Partial
                };
                book.match_order(buy_order);
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// ============================================================================
/// 2. MEMORY POOL EFFICIENCY (Free List)
/// ============================================================================

/// Benchmark: Order lifecycle - add → remove → reuse
/// Tests: free list effectiveness
fn bench_memory_pool_reuse(c: &mut Criterion) {
    let mut group = c.benchmark_group("OrderBook - Memory Pool Reuse");

    group.bench_function("add_remove_add_sequence", |b| {
        b.iter_batched(
            || OrderBook::new(),
            |mut book| {
                // Add order 1
                let order1 = NewOrderRequest {
                    user_id: 1,
                    symbol: Arc::from("BTC/USD"),
                    order_type: OrderType::Buy,
                    price: 50000,
                    quantity: 100,
                };
                let (_trades1, _) = book.match_order(order1);

                // Remove order (via complete match)
                let order2 = NewOrderRequest {
                    user_id: 2,
                    symbol: Arc::from("BTC/USD"),
                    order_type: OrderType::Sell,
                    price: 49999,
                    quantity: 100,
                };
                let (_trades2, _) = book.match_order(order2);

                // Add order 3 - should reuse freed slot
                let order3 = NewOrderRequest {
                    user_id: 3,
                    symbol: Arc::from("BTC/USD"),
                    order_type: OrderType::Buy,
                    price: 51000,
                    quantity: 50,
                };
                book.match_order(order3);
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// ============================================================================
/// 3. PRICE LEVEL LOOKUP PERFORMANCE (BTreeMap)
/// ============================================================================

/// Benchmark: Lookup time with varying depth
fn bench_price_level_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("OrderBook - Price Level Lookup");

    for num_levels in [10, 100, 1000, 10000].iter() {
        group.throughput(Throughput::Elements(*num_levels as u64));

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_levels", num_levels)),
            num_levels,
            |b, &num_levels| {
                b.iter_batched(
                    || {
                        let mut book = OrderBook::new();
                        // Pre-populate with sell orders at different price levels
                        for i in 0..num_levels {
                            book.match_order(NewOrderRequest {
                                user_id: 100 + i as u64,
                                symbol: Arc::from("BTC/USD"),
                                order_type: OrderType::Sell,
                                price: 50000 + (i as u64),
                                quantity: 100,
                            });
                        }
                        book
                    },
                    |mut book| {
                        // Issue a buy order that will scan all levels
                        let buy_order = NewOrderRequest {
                            user_id: 1,
                            symbol: Arc::from("BTC/USD"),
                            order_type: OrderType::Buy,
                            price: black_box(50000 + num_levels as u64),
                            quantity: black_box(1000),
                        };
                        book.match_order(buy_order);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

/// ============================================================================
/// 4. MULTIPLE ORDERS AT SAME PRICE (Linked List Traversal)
/// ============================================================================

/// Benchmark: FIFO matching at single price level
fn bench_fifo_order_queue(c: &mut Criterion) {
    let mut group = c.benchmark_group("OrderBook - FIFO Queue");

    for queue_depth in [1, 10, 100, 1000].iter() {
        group.throughput(Throughput::Elements(*queue_depth as u64));

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("depth_{}", queue_depth)),
            queue_depth,
            |b, &queue_depth| {
                b.iter_batched(
                    || {
                        let mut book = OrderBook::new();
                        // Add multiple sell orders at same price
                        for i in 0..queue_depth {
                            book.match_order(NewOrderRequest {
                                user_id: 100 + i as u64,
                                symbol: Arc::from("BTC/USD"),
                                order_type: OrderType::Sell,
                                price: 50000,
                                quantity: 100,
                            });
                        }
                        book
                    },
                    |mut book| {
                        // Single large buy that matches all orders in queue
                        let buy_order = NewOrderRequest {
                            user_id: 1,
                            symbol: Arc::from("BTC/USD"),
                            order_type: OrderType::Buy,
                            price: 50000,
                            quantity: black_box((queue_depth * 100) as u64),
                        };
                        book.match_order(buy_order);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

/// ============================================================================
/// 5. ALLOCATION & DEALLOCATION COST
/// ============================================================================

/// Benchmark: TradeNotification allocation
/// Tests: Vec<TradeNotification> growth cost
fn bench_trade_allocation(c: &mut Criterion) {
    let mut group = c.benchmark_group("Performance - Trade Allocation");

    for num_trades in [1, 10, 100, 1000].iter() {
        group.throughput(Throughput::Elements(*num_trades as u64));

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_trades", num_trades)),
            num_trades,
            |b, &num_trades| {
                b.iter(|| {
                    let mut trades = Vec::with_capacity(num_trades);
                    for i in 0..num_trades {
                        trades.push(TradeNotification {
                            trade_id: i as u64,
                            symbol: Arc::from("BTC/USD"),
                            matched_price: 50000,
                            matched_quantity: 100,
                            buyer_user_id: 1,
                            buyer_order_id: 1,
                            seller_user_id: 2,
                            seller_order_id: 2,
                            timestamp: 0,
                        });
                    }
                    black_box(trades);
                });
            },
        );
    }

    group.finish();
}

/// ============================================================================
/// 6. SERIALIZATION COST (serde_json)
/// ============================================================================

/// Benchmark: JSON serialization of messages
fn bench_json_serialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("Performance - JSON Serialization");
    group.throughput(Throughput::Bytes(200)); // Approx size of one message

    group.bench_function("trade_notification_serialize", |b| {
        let trade = TradeNotification {
            trade_id: 1,
            symbol: Arc::from("BTC/USD"),
            matched_price: 50000,
            matched_quantity: 100,
            buyer_user_id: 1,
            buyer_order_id: 1,
            seller_user_id: 2,
            seller_order_id: 2,
            timestamp: 1234567890,
        };

        b.iter(|| {
            serde_json::to_string(&black_box(&trade)).unwrap()
        });
    });

    group.bench_function("order_confirmation_serialize", |b| {
        let confirmation = OrderConfirmation {
            order_id: 1,
            user_id: 1,
        };

        b.iter(|| {
            serde_json::to_string(&black_box(&confirmation)).unwrap()
        });
    });

    group.finish();
}

/// ============================================================================
/// 7. WORST-CASE SCENARIOS
/// ============================================================================

/// Benchmark: Worst-case price crossing (many matches)
fn bench_worst_case_crossing(c: &mut Criterion) {
    let mut group = c.benchmark_group("OrderBook - Worst Case");
    group.throughput(Throughput::Elements(1000));

    group.bench_function("1000_price_levels_fully_crossed", |b| {
        b.iter_batched(
            || {
                let mut book = OrderBook::new();
                // Create asks from 50000 to 51000
                for i in 0..1000 {
                    book.match_order(NewOrderRequest {
                        user_id: 100 + i as u64,
                        symbol: Arc::from("BTC/USD"),
                        order_type: OrderType::Sell,
                        price: 50000 + i as u64,
                        quantity: 10,
                    });
                }
                book
            },
            |mut book| {
                // Massive buy order crossing all levels
                let big_buy = NewOrderRequest {
                    user_id: 1,
                    symbol: Arc::from("BTC/USD"),
                    order_type: OrderType::Buy,
                    price: black_box(51000),
                    quantity: black_box(10000),
                };
                book.match_order(big_buy);
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

// Criterion Setup

criterion_group!(
    name = benches;
    config = Criterion::default().sample_size(100).measurement_time(std::time::Duration::from_secs(10));
    targets =
        bench_order_add_no_match,
        bench_full_match,
        bench_partial_match,
        bench_memory_pool_reuse,
        bench_price_level_lookup,
        bench_fifo_order_queue,
        bench_trade_allocation,
        bench_json_serialization,
        bench_worst_case_crossing
);

criterion_main!(benches);
