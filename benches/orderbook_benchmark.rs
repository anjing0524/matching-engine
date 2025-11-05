use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use matching_engine::orderbook::OrderBook;
use matching_engine::protocol::{NewOrderRequest, OrderType};
use std::sync::Arc;

// OrderBook 需要实现 Clone trait 才能在基准测试中被高效克隆
// 我们需要在 orderbook.rs 中添加 #[derive(Clone)]

fn realistic_match_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("Realistic OrderBook Matching");

    let book_size = 1000;

    // 1. 一次性创建一个预填充的“母版”订单簿
    let mut master_orderbook = OrderBook::new();
    for i in 0..book_size {
        master_orderbook.match_order(NewOrderRequest {
            user_id: (i + 1) as u64,
            symbol: "BTC/USD".to_string(),
            order_type: OrderType::Sell,
            price: 50000 + i as u64,
            quantity: 10,
        });
    }

    group.bench_function("1-to-1 Match in a cloned book with 1000 levels", |b| {
        b.iter_batched(
            // 2. Setup: 每次迭代只是克隆母版，这个操作非常快
            || {
                let orderbook_clone = master_orderbook.clone();
                let incoming_order = NewOrderRequest {
                    user_id: 0,
                    symbol: Arc::from("BTC/USD"),
                    order_type: OrderType::Buy,
                    price: 50000,
                    quantity: 10,
                };
                (orderbook_clone, incoming_order)
            },
            // 3. Measured Routine: 实际的撮合操作
            |(mut orderbook, order)| {
                orderbook.match_order(black_box(order));
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(benches, realistic_match_benchmark);
criterion_main!(benches);
