//! Tick-based Array OrderBook vs BTreeMap性能对比

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId, Throughput};
use matching_engine::orderbook::OrderBook;
use matching_engine::orderbook_v2::OrderBookV2;
use matching_engine::orderbook_tick::{TickBasedOrderBook, ContractSpec};
use matching_engine::protocol::{NewOrderRequest, OrderType};
use std::sync::Arc;

fn generate_orders(count: usize, tick_size: u64, base_price: u64) -> Vec<NewOrderRequest> {
    (0..count)
        .map(|i| NewOrderRequest {
            user_id: (i as u64) % 100,
            symbol: Arc::from("rb2501"),
            order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
            // 确保价格在tick上
            price: base_price + ((i % 100) as u64) * tick_size,
            quantity: 10,
        })
        .collect()
}

fn bench_btreemap_linked_list(c: &mut Criterion) {
    let mut group = c.benchmark_group("BTreeMap + Linked List");
    
    for count in [100, 500, 1000] {
        let orders = generate_orders(count, 10, 3000);
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter(|| {
                let mut book = OrderBook::new();
                for order in &orders {
                    let _ = book.match_order(black_box(order.clone()));
                }
            });
        });
    }
    group.finish();
}

fn bench_btreemap_ringbuffer(c: &mut Criterion) {
    let mut group = c.benchmark_group("BTreeMap + RingBuffer");
    
    for count in [100, 500, 1000] {
        let orders = generate_orders(count, 10, 3000);
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter(|| {
                let mut book = OrderBookV2::new();
                for order in &orders {
                    let _ = book.match_order(black_box(order.clone()));
                }
            });
        });
    }
    group.finish();
}

fn bench_array_ringbuffer(c: &mut Criterion) {
    let mut group = c.benchmark_group("Array + RingBuffer (Tick-based)");
    
    for count in [100, 500, 1000] {
        let orders = generate_orders(count, 10, 3000);
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter(|| {
                // 螺纹钢期货: tick=1, 价格范围2000-6000
                let spec = ContractSpec::new("rb2501", 10, 2000, 6000);
                let mut book = TickBasedOrderBook::new(spec);
                for order in &orders {
                    let _ = book.match_order(black_box(order.clone()));
                }
            });
        });
    }
    group.finish();
}

/// 真实期货盘口场景：价格集中分布
/// 特点：90%订单集中在最优价±20tick内，符合真实交易特征
fn bench_realistic_futures(c: &mut Criterion) {
    let mut group = c.benchmark_group("Realistic Futures Market (concentrated prices)");

    let mid_price = 3500u64;
    let tick_size = 10u64;

    // 生成1000个订单，90%集中在±20tick内
    let mut orders = Vec::new();
    for i in 0..1000 {
        let (order_type, price_offset) = if i % 2 == 0 {
            // 买单
            (OrderType::Buy, -(i as i64 % 20))
        } else {
            // 卖单
            (OrderType::Sell, i as i64 % 20)
        };

        // 90%订单在±20tick内，10%在±50tick内
        let offset = if i < 900 {
            price_offset  // ±20 tick
        } else {
            price_offset * 2 + if i % 2 == 0 { -30 } else { 30 }  // ±50 tick
        };

        let price = (mid_price as i64 + offset * tick_size as i64) as u64;

        orders.push(NewOrderRequest {
            user_id: i as u64,
            symbol: Arc::from("rb2501"),
            order_type,
            price,
            quantity: 10,
        });
    }

    group.throughput(Throughput::Elements(1000));

    group.bench_function("BTreeMap", |b| {
        b.iter(|| {
            let mut book = OrderBookV2::new();
            for order in &orders {
                let _ = book.match_order(black_box(order.clone()));
            }
        });
    });

    group.bench_function("Array", |b| {
        b.iter(|| {
            // 价格范围覆盖±100tick
            let spec = ContractSpec::new("rb2501", tick_size, mid_price - 1000, mid_price + 1000);
            let mut book = TickBasedOrderBook::new(spec);
            for order in &orders {
                let _ = book.match_order(black_box(order.clone()));
            }
        });
    });

    group.finish();
}

/// 深度测试：大量价格层
fn bench_deep_orderbook(c: &mut Criterion) {
    let mut group = c.benchmark_group("Deep OrderBook (1000 price levels)");
    
    // 生成分散在1000个价格层的订单
    let orders: Vec<_> = (0..1000)
        .map(|i| NewOrderRequest {
            user_id: i as u64,
            symbol: Arc::from("rb2501"),
            order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
            price: 2000 + (i as u64) * 10, // 每个价格一个订单
            quantity: 10,
        })
        .collect();
    
    group.throughput(Throughput::Elements(1000));
    
    group.bench_function("BTreeMap", |b| {
        b.iter(|| {
            let mut book = OrderBook::new();
            for order in &orders {
                let _ = book.match_order(black_box(order.clone()));
            }
        });
    });
    
    group.bench_function("Array", |b| {
        b.iter(|| {
            let spec = ContractSpec::new("rb2501", 10, 2000, 12000);
            let mut book = TickBasedOrderBook::new(spec);
            for order in &orders {
                let _ = book.match_order(black_box(order.clone()));
            }
        });
    });
    
    group.finish();
}

criterion_group!(
    benches,
    bench_btreemap_linked_list,
    bench_btreemap_ringbuffer,
    bench_array_ringbuffer,
    bench_realistic_futures,
    bench_deep_orderbook,
);

criterion_main!(benches);
