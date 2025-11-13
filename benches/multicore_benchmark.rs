//! Multi-core Performance Benchmark
//!
//! 测试16核并行处理性能
//!
//! ## 测试场景
//! 1. 单核基准
//! 2. 4核并行
//! 3. 8核并行
//! 4. 16核并行
//!
//! ## 运行
//! ```bash
//! cargo bench --bench multicore_benchmark
//! ```

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId, Throughput};
use matching_engine::domain::orderbook::{TickBasedOrderBook, ContractSpec};
use matching_engine::shared::protocol::{NewOrderRequest, OrderType};
use matching_engine::shared::metrics::METRICS;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;
use rand::Rng;

/// 生成随机订单
fn generate_random_order(symbol: &str, rng: &mut impl Rng) -> NewOrderRequest {
    let order_type = if rng.gen_bool(0.5) {
        OrderType::Buy
    } else {
        OrderType::Sell
    };

    // Generate price aligned to tick size (10) within range
    // Range: 49000-51000 with tick size 10 (e.g., 49000, 49010, 49020...)
    let price_ticks = rng.gen_range(4900..5100); // Tick count
    let price = price_ticks * 10; // Actual price
    let quantity = rng.gen_range(1..100);

    NewOrderRequest {
        user_id: rng.gen(),
        symbol: Arc::from(symbol),
        order_type,
        price,
        quantity,
    }
}

/// 单核基准测试
fn single_core_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_core");
    group.throughput(Throughput::Elements(10000));

    group.bench_function("tick_orderbook", |b| {
        let spec = ContractSpec::new("BTC/USD", 10, 40000, 60000);
        let mut orderbook = TickBasedOrderBook::new(spec);
        let mut rng = rand::thread_rng();

        b.iter(|| {
            for _ in 0..10000 {
                let order = generate_random_order("BTC/USD", &mut rng);
                let start = std::time::Instant::now();
                let _result = orderbook.match_order(order);
                let duration = start.elapsed();

                // 记录metrics
                METRICS.matching_duration
                    .with_label_values(&["BTC/USD"])
                    .observe(duration.as_micros() as f64);
            }
        });
    });

    group.finish();
}

/// 多核基准测试
fn multicore_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("multicore");

    // 测试不同核心数
    for cores in &[1, 2, 4, 8, 16] {
        let cores = *cores;
        if cores > num_cpus::get() {
            eprintln!("跳过{}核测试（系统只有{}个核心）", cores, num_cpus::get());
            continue;
        }

        group.throughput(Throughput::Elements(10000 * cores as u64));

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}cores", cores)),
            &cores,
            |b, &cores| {
                b.iter(|| {
                    let barrier = Arc::new(Barrier::new(cores));
                    let mut handles = vec![];

                    for thread_id in 0..cores {
                        let barrier = barrier.clone();

                        let handle = thread::spawn(move || {
                            // 每个线程创建自己的订单簿
                            let symbol = format!("SYM{}", thread_id);
                            let spec = ContractSpec::new(&symbol, 10, 40000, 60000);
                            let mut orderbook = TickBasedOrderBook::new(spec);
                            let mut rng = rand::thread_rng();

                            // 等待所有线程就绪
                            barrier.wait();

                            // 执行订单处理
                            for _ in 0..10000 {
                                let order = generate_random_order(&symbol, &mut rng);
                                black_box(orderbook.match_order(order));
                            }
                        });

                        handles.push(handle);
                    }

                    // 等待所有线程完成
                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// 分区引擎基准测试（实际场景）
fn partitioned_engine_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("partitioned_engine");
    let num_partitions = num_cpus::get().min(16);

    group.throughput(Throughput::Elements(100000));

    group.bench_function(format!("{}_partitions", num_partitions), |b| {
        b.iter(|| {
            let barrier = Arc::new(Barrier::new(num_partitions));
            let mut handles = vec![];

            for partition_id in 0..num_partitions {
                let barrier = barrier.clone();

                let handle = thread::spawn(move || {
                    // 每个分区处理多个品种
                    let symbols = vec![
                        format!("BTC/USD-P{}", partition_id),
                        format!("ETH/USD-P{}", partition_id),
                        format!("SOL/USD-P{}", partition_id),
                    ];

                    let mut orderbooks: Vec<_> = symbols
                        .iter()
                        .map(|s| {
                            let spec = ContractSpec::new(s, 10, 10000, 100000);
                            TickBasedOrderBook::new(spec)
                        })
                        .collect();

                    let mut rng = rand::thread_rng();

                    // 等待所有分区就绪
                    barrier.wait();

                    // 处理订单
                    for _ in 0..(100000 / num_partitions) {
                        let symbol_idx = rng.gen_range(0..symbols.len());
                        let order = generate_random_order(&symbols[symbol_idx], &mut rng);
                        black_box(orderbooks[symbol_idx].match_order(order));
                    }
                });

                handles.push(handle);
            }

            // 等待所有分区完成
            for handle in handles {
                handle.join().unwrap();
            }
        });
    });

    group.finish();
}

/// 吞吐量测试（ops/s）
fn throughput_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("throughput");
    group.measurement_time(Duration::from_secs(10));

    let num_cores = num_cpus::get().min(16);

    group.bench_function(format!("{}_cores_max_throughput", num_cores), |b| {
        b.iter(|| {
            let barrier = Arc::new(Barrier::new(num_cores));
            let mut handles = vec![];

            for thread_id in 0..num_cores {
                let barrier = barrier.clone();

                let handle = thread::spawn(move || {
                    let symbol = format!("PERF{}", thread_id);
                    let spec = ContractSpec::new(&symbol, 1, 10000, 100000);
                    let mut orderbook = TickBasedOrderBook::new(spec);
                    let mut rng = rand::thread_rng();

                    barrier.wait();

                    let mut count = 0u64;
                    let start = std::time::Instant::now();
                    let duration = Duration::from_secs(1);

                    while start.elapsed() < duration {
                        let order = generate_random_order(&symbol, &mut rng);
                        black_box(orderbook.match_order(order));
                        count += 1;
                    }

                    count
                });

                handles.push(handle);
            }

            let total: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
            eprintln!("总吞吐量: {} ops/s ({} cores)", total, num_cores);
            eprintln!("单核平均: {} ops/s", total / num_cores as u64);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    single_core_benchmark,
    multicore_benchmark,
    partitioned_engine_benchmark,
    throughput_benchmark
);
criterion_main!(benches);
