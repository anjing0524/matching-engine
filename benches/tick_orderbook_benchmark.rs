//! Tick-based order book benchmarks.
//!
//! Measures order entry, FIFO matching, best-price discovery, and deep-book
//! behaviour of [`TickBasedOrderBook`].
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use trading_system_engine::domain::orderbook::{ContractSpec, TickBasedOrderBook};
use trading_system_engine::shared::protocol::{NewOrderRequest, OrderType};

fn order(user_id: u64, order_type: OrderType, price: u64, quantity: u64) -> NewOrderRequest {
    NewOrderRequest {
        user_id,
        client_order_id: 0,
        symbol: trading_system_engine::shared::SymbolId::intern("rb2501"),
        order_type,
        price,
        quantity,
    }
}

fn new_book(tick_size: u64, min_price: u64, max_price: u64) -> TickBasedOrderBook {
    TickBasedOrderBook::new(ContractSpec::new("rb2501", tick_size, min_price, max_price))
}

/// Inserts crossing buy/sell orders to exercise the full matching path.
fn bench_matching_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("matching_throughput");

    for count in [100u64, 1_000, 10_000] {
        group.throughput(Throughput::Elements(count));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            b.iter(|| {
                let mut book = new_book(10, 3_000, 4_000);
                for i in 0..count {
                    let side = if i % 2 == 0 {
                        OrderType::Buy
                    } else {
                        OrderType::Sell
                    };
                    let price = 3_500 + (i % 50) * 10;
                    let _ = book.match_order(black_box(order(i, side, price, 10)));
                }
            });
        });
    }

    group.finish();
}

/// Adds orders that never cross so each one rests and is looked up later.
fn bench_resting_orders(c: &mut Criterion) {
    let mut group = c.benchmark_group("resting_orders");
    group.throughput(Throughput::Elements(1_000));

    group.bench_function("add_1000_levels", |b| {
        b.iter(|| {
            let mut book = new_book(1, 2_000, 12_000);
            for i in 0..1_000 {
                let _ = book.match_order(black_box(order(i, OrderType::Buy, 2_000 + i, 10)));
            }
            black_box(book.best_bid());
        });
    });

    group.finish();
}

/// FIFO matching inside a single price level.
fn bench_fifo_queue(c: &mut Criterion) {
    let mut group = c.benchmark_group("fifo_queue");

    for depth in [10, 100, 1_000] {
        group.throughput(Throughput::Elements(depth));
        group.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |b, &depth| {
            b.iter_batched(
                || {
                    let mut book = new_book(1, 2_000, 12_000);
                    for i in 0..depth {
                        book.match_order(order(i, OrderType::Sell, 5_000, 10));
                    }
                    book
                },
                |mut book| {
                    let _ = book.match_order(order(99_999, OrderType::Buy, 5_000, depth * 10));
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

/// Cost of best-bid/best-ask discovery as the book grows.
fn bench_best_price_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("best_price_lookup");

    for levels in [10, 100, 1_000, 6_000] {
        group.throughput(Throughput::Elements(levels));
        group.bench_with_input(
            BenchmarkId::from_parameter(levels),
            &levels,
            |b, &levels| {
                let mut book = new_book(1, 2_000, 12_000);
                for i in 0..levels {
                    book.match_order(order(i, OrderType::Buy, 2_000 + i, 10));
                }

                b.iter(|| black_box((book.best_bid(), book.best_ask())));
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_matching_throughput,
    bench_resting_orders,
    bench_fifo_queue,
    bench_best_price_lookup,
);
criterion_main!(benches);
