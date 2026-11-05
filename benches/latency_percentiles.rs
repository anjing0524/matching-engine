//! In-process matching latency percentiles.
//!
//! Criterion reports means; trading desks need the tail. This harness records
//! every matching call and prints p50/p90/p99/p99.9/p99.99/max in nanoseconds.
//! The `Instant` read overhead (~20-30 ns) is included in each sample.
use matching_engine::domain::orderbook::{ContractSpec, TickBasedOrderBook};
use matching_engine::shared::SymbolId;
use matching_engine::shared::latency::LatencyRecorder;
use matching_engine::shared::protocol::{NewOrderRequest, OrderType};
use std::time::Instant;

const SAMPLES: usize = 1_000_000;

fn order(symbol: SymbolId, side: OrderType, price: u64) -> NewOrderRequest {
    NewOrderRequest {
        user_id: 1,
        client_order_id: 0,
        symbol,
        order_type: side,
        price,
        quantity: 1,
    }
}

fn main() {
    let symbol = SymbolId::intern("BTCUSDT");
    let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
    let mut book = TickBasedOrderBook::new(spec);

    // Warm the allocator, branch predictors, and caches.
    for i in 0..10_000 {
        let side = if i % 2 == 0 {
            OrderType::Buy
        } else {
            OrderType::Sell
        };
        let _ = book.match_order(order(symbol, side, 50_000));
    }
    book = TickBasedOrderBook::new(ContractSpec::new("BTCUSDT", 1, 40_000, 70_000));

    let recorder = LatencyRecorder::new();
    let started = Instant::now();

    for i in 0..SAMPLES {
        // Even orders rest; odd orders cross the resting order at the same price.
        let side = if i % 2 == 0 {
            OrderType::Buy
        } else {
            OrderType::Sell
        };
        let price = 50_000;
        let op_start = Instant::now();
        let _ = book.match_order(order(symbol, side, price));
        recorder.record_duration(op_start.elapsed());
    }

    let elapsed = started.elapsed();
    let summary = recorder.snapshot();
    let throughput = SAMPLES as f64 / elapsed.as_secs_f64();

    println!("matching calls: {}", summary.count);
    println!("wall time: {elapsed:?}  throughput: {throughput:.0} ops/s");
    println!(
        "matching latency: p50={}ns p90={}ns p99={}ns p99.9={}ns p99.99={}ns max={}ns",
        summary.p50, summary.p90, summary.p99, summary.p99_9, summary.p99_99, summary.max
    );

    // O(1) cancellation: cancel 100k resting orders from one FIFO level.
    let mut cancel_spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
    cancel_spec.queue_capacity = 100_000;
    let mut cancel_book = TickBasedOrderBook::new(cancel_spec);
    for user_id in 1..=100_000u64 {
        let _ = cancel_book.match_order(NewOrderRequest {
            user_id,
            client_order_id: 0,
            symbol,
            order_type: OrderType::Sell,
            price: 50_000,
            quantity: 1,
        });
    }

    let cancel_recorder = LatencyRecorder::new();
    for order_id in 1..=100_000u64 {
        let started = Instant::now();
        cancel_book
            .cancel_order(order_id, order_id)
            .expect("cancel must succeed");
        cancel_recorder.record_duration(started.elapsed());
    }
    let cancel_summary = cancel_recorder.snapshot();
    println!(
        "cancel latency: p50={}ns p90={}ns p99={}ns p99.9={}ns p99.99={}ns max={}ns",
        cancel_summary.p50,
        cancel_summary.p90,
        cancel_summary.p99,
        cancel_summary.p99_9,
        cancel_summary.p99_99,
        cancel_summary.max
    );
}
