//! RingBuffer vs 链表实现性能对比

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use matching_engine::orderbook::OrderBook;
use matching_engine::orderbook_v2::OrderBookV2;
use matching_engine::protocol::{NewOrderRequest, OrderType};
use std::sync::Arc;

fn generate_orders(count: usize) -> Vec<NewOrderRequest> {
    (0..count)
        .map(|i| NewOrderRequest {
            user_id: (i as u64) % 100,
            symbol: Arc::from("BTC/USD"),
            order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
            price: 50000 + ((i % 10) as u64) * 10,
            quantity: 10,
        })
        .collect()
}

fn bench_orderbook_v1(c: &mut Criterion) {
    let mut group = c.benchmark_group("OrderBook V1 (Linked List)");
    
    for count in [100, 500, 1000] {
        let orders = generate_orders(count);
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

fn bench_orderbook_v2(c: &mut Criterion) {
    let mut group = c.benchmark_group("OrderBook V2 (RingBuffer)");
    
    for count in [100, 500, 1000] {
        let orders = generate_orders(count);
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

criterion_group!(benches, bench_orderbook_v1, bench_orderbook_v2);
criterion_main!(benches);
