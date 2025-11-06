/// 分区引擎性能基准测试
/// 验证并行架构的性能提升

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId, Throughput};
use matching_engine::partitioned_engine::{PartitionedEngine, PartitionConfig};
use matching_engine::protocol::{NewOrderRequest, OrderType};
use std::sync::Arc;

/// 基准测试：不同分区数量的性能对比
fn bench_partition_count(c: &mut Criterion) {
    let mut group = c.benchmark_group("Partitioned Engine - Partition Count");

    for partition_count in [1, 2, 4, 8, 16] {
        group.throughput(Throughput::Elements(1000));

        group.bench_with_input(
            BenchmarkId::from_parameter(partition_count),
            &partition_count,
            |b, &count| {
                let config = PartitionConfig {
                    partition_count: count,
                    queue_capacity: 10_000,
                    batch_size: 100,
                    enable_cpu_affinity: false, // 基准测试中禁用
                };

                let engine = PartitionedEngine::new(config);

                b.iter(|| {
                    // 提交1000个订单
                    for i in 0..1000 {
                        let order = NewOrderRequest {
                            user_id: i as u64,
                            symbol: Arc::from("BTC/USD"),
                            order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
                            price: 50000 + (i % 100) as u64,
                            quantity: 10,
                        };

                        engine.submit_order(black_box(order)).unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// 基准测试：批量大小的影响
fn bench_batch_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("Partitioned Engine - Batch Size");

    for batch_size in [1, 10, 50, 100, 200] {
        group.throughput(Throughput::Elements(1000));

        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            &batch_size,
            |b, &size| {
                let config = PartitionConfig {
                    partition_count: 4,
                    queue_capacity: 10_000,
                    batch_size: size,
                    enable_cpu_affinity: false,
                };

                let engine = PartitionedEngine::new(config);

                b.iter(|| {
                    for i in 0..1000 {
                        let order = NewOrderRequest {
                            user_id: i as u64,
                            symbol: Arc::from("BTC/USD"),
                            order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
                            price: 50000,
                            quantity: 10,
                        };

                        engine.submit_order(black_box(order)).unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// 基准测试：多交易对分布
fn bench_multi_symbol(c: &mut Criterion) {
    let mut group = c.benchmark_group("Partitioned Engine - Multi Symbol");
    group.throughput(Throughput::Elements(1000));

    let symbols = vec![
        "BTC/USD", "ETH/USD", "BNB/USD", "SOL/USD",
        "ADA/USD", "XRP/USD", "DOT/USD", "MATIC/USD",
    ];

    group.bench_function("8_symbols_distributed", |b| {
        let config = PartitionConfig {
            partition_count: 8,
            queue_capacity: 10_000,
            batch_size: 100,
            enable_cpu_affinity: false,
        };

        let engine = PartitionedEngine::new(config);

        b.iter(|| {
            for i in 0..1000 {
                let symbol = symbols[i % symbols.len()];
                let order = NewOrderRequest {
                    user_id: i as u64,
                    symbol: Arc::from(symbol),
                    order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
                    price: 50000,
                    quantity: 10,
                };

                engine.submit_order(black_box(order)).unwrap();
            }
        });
    });

    group.finish();
}

/// 基准测试：同步vs异步提交
fn bench_sync_vs_async(c: &mut Criterion) {
    let mut group = c.benchmark_group("Partitioned Engine - Sync vs Async");

    let config = PartitionConfig {
        partition_count: 4,
        queue_capacity: 10_000,
        batch_size: 100,
        enable_cpu_affinity: false,
    };

    let engine = PartitionedEngine::new(config.clone());

    group.bench_function("async_submit", |b| {
        b.iter(|| {
            let order = NewOrderRequest {
                user_id: 1,
                symbol: Arc::from("BTC/USD"),
                order_type: OrderType::Buy,
                price: 50000,
                quantity: 10,
            };

            engine.submit_order(black_box(order)).unwrap();
        });
    });

    let engine2 = PartitionedEngine::new(config);

    group.bench_function("sync_submit", |b| {
        b.iter(|| {
            let order = NewOrderRequest {
                user_id: 1,
                symbol: Arc::from("BTC/USD"),
                order_type: OrderType::Buy,
                price: 50000,
                quantity: 10,
            };

            let _ = engine2.submit_order_sync(black_box(order));
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_partition_count,
    bench_batch_size,
    bench_multi_symbol,
    bench_sync_vs_async,
);
criterion_main!(benches);
