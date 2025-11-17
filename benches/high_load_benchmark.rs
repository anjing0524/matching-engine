//! 高负载基准测试 - 验证批量提交和多核扩展性

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput, BenchmarkId};
use matching_engine::partitioned_engine::{PartitionedEngine, PartitionConfig};
use matching_engine::protocol::{NewOrderRequest, OrderType};
use std::sync::Arc;
use std::time::Duration;

fn generate_orders(count: usize, symbol_count: usize) -> Vec<NewOrderRequest> {
    let symbols: Vec<Arc<str>> = (0..symbol_count)
        .map(|i| Arc::from(format!("SYM{:03}/USD", i)))
        .collect();

    (0..count)
        .map(|i| NewOrderRequest {
            user_id: (i as u64) % 10000,
            symbol: symbols[i % symbols.len()].clone(),
            order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
            price: 50000 + ((i % 100) as u64) * 10,
            quantity: 10,
        })
        .collect()
}

fn bench_batch_vs_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("Batch vs Single");
    group.measurement_time(Duration::from_secs(10));

    for order_count in [1000, 5000, 10000] {
        let orders = generate_orders(order_count, 20);
        group.throughput(Throughput::Elements(order_count as u64));

        // 单个提交
        group.bench_with_input(BenchmarkId::new("single", order_count), &order_count, |b, _| {
            let config = PartitionConfig {
                partition_count: 4,
                queue_capacity: 100_000,
                batch_size: 100,
                enable_cpu_affinity: false,
            };
            let engine = PartitionedEngine::new(config);
            b.iter(|| {
                for order in &orders {
                    let _ = engine.submit_order(black_box(order.clone()));
                }
                std::thread::sleep(Duration::from_micros(50));
            });
        });

        // 批量提交
        group.bench_with_input(BenchmarkId::new("batch", order_count), &order_count, |b, _| {
            let config = PartitionConfig {
                partition_count: 4,
                queue_capacity: 100_000,
                batch_size: 100,
                enable_cpu_affinity: false,
            };
            let engine = PartitionedEngine::new(config);
            b.iter(|| {
                let _ = engine.submit_order_batch(black_box(orders.clone()));
                std::thread::sleep(Duration::from_micros(50));
            });
        });
    }
    group.finish();
}

fn bench_partition_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("Partition Scaling");
    group.measurement_time(Duration::from_secs(15));
    
    let orders = generate_orders(20000, 50);
    
    for partitions in [1, 2, 4, 8] {
        group.throughput(Throughput::Elements(20000));
        group.bench_with_input(BenchmarkId::from_parameter(partitions), &partitions, |b, &p| {
            let config = PartitionConfig {
                partition_count: p,
                queue_capacity: 100_000,
                batch_size: 100,
                enable_cpu_affinity: false,
            };
            let engine = PartitionedEngine::new(config);
            b.iter(|| {
                let _ = engine.submit_order_batch(black_box(orders.clone()));
                std::thread::sleep(Duration::from_millis(1));
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_batch_vs_single, bench_partition_scaling);
criterion_main!(benches);
