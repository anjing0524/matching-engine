/// HFT (High-Frequency Trading) 性能基准测试
///
/// 基于期货行业特性的专项性能测试：
/// 1. 纳秒级延迟测量 (P50/P95/P99/P99.9)
/// 2. 极端吞吐量压测 (百万级TPS)
/// 3. 订单簿深度压力测试
/// 4. 市场微观结构模拟 (真实交易模式)

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use matching_engine::domain::orderbook::{ContractSpec, TickBasedOrderBook};
use matching_engine::domain::orderbook::traits::OrderBook;
use matching_engine::shared::protocol::{NewOrderRequest, OrderType};
use std::sync::Arc;
use std::time::Duration;

// ============================================================================
// 场景1: 纳秒级延迟测量 (P50/P95/P99/P99.9)
// ============================================================================

/// 测试单订单延迟（冷启动 - 空订单簿）
fn bench_single_order_latency_cold(c: &mut Criterion) {
    let mut group = c.benchmark_group("hft_latency_cold");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(10000);

    group.bench_function("match_order_cold_start", |b| {
        b.iter_batched(
            || {
                // Setup: 每次创建新订单簿
                let spec = ContractSpec::new("BTC2501", 1, 40000, 70000);
                let orderbook = TickBasedOrderBook::new(spec);
                orderbook
            },
            |mut orderbook| {
                // Measure: 冷启动延迟
                let order = NewOrderRequest {
                    user_id: 1,
                    symbol: Arc::from("BTC2501"),
                    order_type: OrderType::Buy,
                    price: 50000,
                    quantity: 10,
                };
                orderbook.match_order(order)
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// 测试单订单延迟（热路径 - 预热订单簿）
fn bench_single_order_latency_hot(c: &mut Criterion) {
    let mut group = c.benchmark_group("hft_latency_hot");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(10000);

    // 预热订单簿：添加100个买卖单
    let spec = ContractSpec::new("BTC2501", 1, 40000, 70000);
    let mut orderbook = TickBasedOrderBook::new(spec);

    for i in 0..50 {
        let buy = NewOrderRequest {
            user_id: i,
            symbol: Arc::from("BTC2501"),
            order_type: OrderType::Buy,
            price: 49000 + i * 10,
            quantity: 10,
        };
        orderbook.match_order(buy);

        let sell = NewOrderRequest {
            user_id: i + 100,
            symbol: Arc::from("BTC2501"),
            order_type: OrderType::Sell,
            price: 51000 + i * 10,
            quantity: 10,
        };
        orderbook.match_order(sell);
    }

    group.bench_function("match_order_hot_path", |b| {
        let mut price_counter = 50000u64;

        b.iter(|| {
            // 测试热路径：订单簿已预热
            let order = NewOrderRequest {
                user_id: 999,
                symbol: Arc::from("BTC2501"),
                order_type: OrderType::Buy,
                price: price_counter,
                quantity: 1,
            };

            price_counter = (price_counter % 100) + 50000; // 循环价格
            orderbook.match_order(order)
        });
    });

    group.finish();
}

/// 测试成交延迟（订单立即成交）
fn bench_immediate_match_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("hft_latency_match");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(10000);

    group.bench_function("immediate_match", |b| {
        b.iter_batched(
            || {
                // Setup: 每次创建新订单簿并添加对手单
                let spec = ContractSpec::new("BTC2501", 1, 40000, 70000);
                let mut orderbook = TickBasedOrderBook::new(spec);

                // 添加卖单
                let sell = NewOrderRequest {
                    user_id: 1,
                    symbol: Arc::from("BTC2501"),
                    order_type: OrderType::Sell,
                    price: 50000,
                    quantity: 100,
                };
                orderbook.match_order(sell);

                orderbook
            },
            |mut orderbook| {
                // Measure: 成交延迟
                let buy = NewOrderRequest {
                    user_id: 2,
                    symbol: Arc::from("BTC2501"),
                    order_type: OrderType::Buy,
                    price: 50000,
                    quantity: 10,
                };
                orderbook.match_order(buy)
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ============================================================================
// 场景2: 极端吞吐量压测 (百万级TPS)
// ============================================================================

/// 测试极端吞吐量（连续订单）
fn bench_extreme_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("hft_throughput");
    group.measurement_time(Duration::from_secs(20));

    for &order_count in &[10_000, 100_000, 1_000_000] {
        group.throughput(Throughput::Elements(order_count as u64));

        group.bench_with_input(
            BenchmarkId::from_parameter(order_count),
            &order_count,
            |b, &count| {
                b.iter(|| {
                    let spec = ContractSpec::new("BTC2501", 1, 40000, 70000);
                    let mut orderbook = TickBasedOrderBook::new(spec);

                    for i in 0..count {
                        let order_type = if i % 2 == 0 {
                            OrderType::Buy
                        } else {
                            OrderType::Sell
                        };

                        let order = NewOrderRequest {
                            user_id: i as u64,
                            symbol: Arc::from("BTC2501"),
                            order_type,
                            price: 50000 + (i % 100) as u64,
                            quantity: 10,
                        };

                        orderbook.match_order(order);
                    }
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// 场景3: 订单簿深度压力测试
// ============================================================================

/// 测试不同订单簿深度下的性能
fn bench_orderbook_depth_stress(c: &mut Criterion) {
    let mut group = c.benchmark_group("hft_depth_stress");
    group.measurement_time(Duration::from_secs(15));

    for &depth in &[100, 1000, 10000, 100000] {
        group.bench_with_input(
            BenchmarkId::new("orderbook_depth", depth),
            &depth,
            |b, &depth| {
                // 预填充订单簿
                let spec = ContractSpec::new("BTC2501", 1, 40000, 70000);
                let mut orderbook = TickBasedOrderBook::new(spec);

                // 添加买单（深度一半）
                for i in 0..depth / 2 {
                    let buy = NewOrderRequest {
                        user_id: i as u64,
                        symbol: Arc::from("BTC2501"),
                        order_type: OrderType::Buy,
                        price: 49000 - (i % 1000) as u64,
                        quantity: 10,
                    };
                    orderbook.match_order(buy);
                }

                // 添加卖单（深度一半）
                for i in 0..depth / 2 {
                    let sell = NewOrderRequest {
                        user_id: (i + depth / 2) as u64,
                        symbol: Arc::from("BTC2501"),
                        order_type: OrderType::Sell,
                        price: 51000 + (i % 1000) as u64,
                        quantity: 10,
                    };
                    orderbook.match_order(sell);
                }

                // 测试在深度订单簿下的匹配性能
                b.iter(|| {
                    let order = NewOrderRequest {
                        user_id: 999999,
                        symbol: Arc::from("BTC2501"),
                        order_type: OrderType::Buy,
                        price: 50000,
                        quantity: 1,
                    };
                    orderbook.match_order(order)
                });
            },
        );
    }

    group.finish();
}

/// 测试订单簿深度对查找最优价的影响
fn bench_best_price_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("hft_best_price_lookup");
    group.measurement_time(Duration::from_secs(10));

    for &active_levels in &[10, 100, 1000, 6000] {
        group.bench_with_input(
            BenchmarkId::new("active_price_levels", active_levels),
            &active_levels,
            |b, &levels| {
                let spec = ContractSpec::new("BTC2501", 1, 40000, 70000);
                let mut orderbook = TickBasedOrderBook::new(spec);

                // 创建稀疏订单簿：每隔10个价格放一单
                for i in 0..levels / 2 {
                    let buy = NewOrderRequest {
                        user_id: i as u64,
                        symbol: Arc::from("BTC2501"),
                        order_type: OrderType::Buy,
                        price: 49000 - (i as u64) * 10,
                        quantity: 10,
                    };
                    orderbook.match_order(buy);

                    let sell = NewOrderRequest {
                        user_id: (i + 10000) as u64,
                        symbol: Arc::from("BTC2501"),
                        order_type: OrderType::Sell,
                        price: 51000 + (i as u64) * 10,
                        quantity: 10,
                    };
                    orderbook.match_order(sell);
                }

                // 测试查找最优价性能
                b.iter(|| {
                    let bid = orderbook.get_best_bid();
                    let ask = orderbook.get_best_ask();
                    (bid, ask)
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// 场景4: 市场微观结构模拟 (真实交易模式)
// ============================================================================

/// 模拟做市商行为（高频报撤单）
fn bench_market_maker_pattern(c: &mut Criterion) {
    let mut group = c.benchmark_group("hft_market_maker");
    group.measurement_time(Duration::from_secs(15));

    group.bench_function("market_maker_quote_update", |b| {
        let spec = ContractSpec::new("BTC2501", 1, 40000, 70000);
        let mut orderbook = TickBasedOrderBook::new(spec);

        let mut order_id_counter = 1u64;

        b.iter(|| {
            // 模拟做市商：双边报价
            let bid = NewOrderRequest {
                user_id: 1,
                symbol: Arc::from("BTC2501"),
                order_type: OrderType::Buy,
                price: 49995,
                quantity: 100,
            };
            let (_, conf_bid) = orderbook.match_order(bid);

            let ask = NewOrderRequest {
                user_id: 1,
                symbol: Arc::from("BTC2501"),
                order_type: OrderType::Sell,
                price: 50005,
                quantity: 100,
            };
            let (_, conf_ask) = orderbook.match_order(ask);

            // 取消订单（模拟报价更新）
            if let Some(conf) = conf_bid {
                let _ = orderbook.cancel_order(conf.order_id);
            }
            if let Some(conf) = conf_ask {
                let _ = orderbook.cancel_order(conf.order_id);
            }

            order_id_counter += 2;
        });
    });

    group.finish();
}

/// 模拟冲击成本（大单吃盘口）
fn bench_market_impact(c: &mut Criterion) {
    let mut group = c.benchmark_group("hft_market_impact");
    group.measurement_time(Duration::from_secs(15));

    for &layers in &[5, 10, 20, 50] {
        group.bench_with_input(
            BenchmarkId::new("eat_orderbook_layers", layers),
            &layers,
            |b, &layers| {
                b.iter_batched(
                    || {
                        // Setup: 创建深度订单簿
                        let spec = ContractSpec::new("BTC2501", 1, 40000, 70000);
                        let mut orderbook = TickBasedOrderBook::new(spec);

                        // 每层100单，每单10量
                        for i in 0..layers {
                            let sell = NewOrderRequest {
                                user_id: i as u64,
                                symbol: Arc::from("BTC2501"),
                                order_type: OrderType::Sell,
                                price: 50000 + i as u64,
                                quantity: 10,
                            };
                            orderbook.match_order(sell);
                        }

                        orderbook
                    },
                    |mut orderbook| {
                        // Measure: 大单吃穿多个价格层
                        let aggressive_buy = NewOrderRequest {
                            user_id: 999,
                            symbol: Arc::from("BTC2501"),
                            order_type: OrderType::Buy,
                            price: 50100,
                            quantity: layers as u64 * 10, // 吃穿所有层
                        };
                        orderbook.match_order(aggressive_buy)
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

/// 模拟高频交易策略（低延迟套利）
fn bench_hft_arbitrage_strategy(c: &mut Criterion) {
    let mut group = c.benchmark_group("hft_arbitrage");
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("latency_arbitrage", |b| {
        let spec = ContractSpec::new("BTC2501", 1, 40000, 70000);
        let mut orderbook = TickBasedOrderBook::new(spec);

        // 预填充订单簿（模拟市场状态）
        for i in 0..10 {
            let buy = NewOrderRequest {
                user_id: i,
                symbol: Arc::from("BTC2501"),
                order_type: OrderType::Buy,
                price: 49900 + i * 10,
                quantity: 100,
            };
            orderbook.match_order(buy);

            let sell = NewOrderRequest {
                user_id: i + 100,
                symbol: Arc::from("BTC2501"),
                order_type: OrderType::Sell,
                price: 50100 - i * 10,
                quantity: 100,
            };
            orderbook.match_order(sell);
        }

        b.iter(|| {
            // HFT策略：检测到套利机会 → 立即成交
            let best_ask = orderbook.get_best_ask().unwrap();

            // 买入
            let buy = NewOrderRequest {
                user_id: 999,
                symbol: Arc::from("BTC2501"),
                order_type: OrderType::Buy,
                price: best_ask,
                quantity: 10,
            };
            let (trades, _) = orderbook.match_order(buy);

            // 验证成交
            assert!(!trades.is_empty(), "Should have trades");
        });
    });

    group.finish();
}

// ============================================================================
// 场景5: 取消订单性能测试
// ============================================================================

/// 测试订单取消性能（不同队列深度）
fn bench_order_cancel_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("hft_cancel_order");
    group.measurement_time(Duration::from_secs(15));

    for &queue_depth in &[10, 100, 1000] {
        group.bench_with_input(
            BenchmarkId::new("cancel_from_queue", queue_depth),
            &queue_depth,
            |b, &depth| {
                b.iter_batched(
                    || {
                        // Setup: 在同一价格创建深度队列
                        let spec = ContractSpec::new("BTC2501", 1, 40000, 70000);
                        let mut orderbook = TickBasedOrderBook::new(spec);

                        let mut order_ids = Vec::new();

                        for i in 0..depth {
                            let order = NewOrderRequest {
                                user_id: i as u64,
                                symbol: Arc::from("BTC2501"),
                                order_type: OrderType::Buy,
                                price: 50000, // 同一价格
                                quantity: 10,
                            };

                            let (_, conf) = orderbook.match_order(order);
                            if let Some(c) = conf {
                                order_ids.push(c.order_id);
                            }
                        }

                        (orderbook, order_ids)
                    },
                    |(mut orderbook, order_ids)| {
                        // Measure: 取消中间订单
                        let mid_order_id = order_ids[depth / 2];
                        orderbook.cancel_order(mid_order_id)
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

// ============================================================================
// Criterion配置
// ============================================================================

criterion_group!(
    name = hft_latency_benches;
    config = Criterion::default()
        .significance_level(0.01)
        .noise_threshold(0.02)
        .warm_up_time(Duration::from_secs(3));
    targets =
        bench_single_order_latency_cold,
        bench_single_order_latency_hot,
        bench_immediate_match_latency
);

criterion_group!(
    name = hft_throughput_benches;
    config = Criterion::default()
        .sample_size(50)
        .warm_up_time(Duration::from_secs(5));
    targets = bench_extreme_throughput
);

criterion_group!(
    name = hft_depth_benches;
    config = Criterion::default();
    targets =
        bench_orderbook_depth_stress,
        bench_best_price_lookup
);

criterion_group!(
    name = hft_pattern_benches;
    config = Criterion::default();
    targets =
        bench_market_maker_pattern,
        bench_market_impact,
        bench_hft_arbitrage_strategy
);

criterion_group!(
    name = hft_cancel_benches;
    config = Criterion::default();
    targets = bench_order_cancel_performance
);

criterion_main!(
    hft_latency_benches,
    hft_throughput_benches,
    hft_depth_benches,
    hft_pattern_benches,
    hft_cancel_benches
);
