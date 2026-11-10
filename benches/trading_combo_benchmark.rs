//! Combo queue and atomic leg-execution throughput.

use std::hint::black_box;
use std::time::Instant;
use trading_system_engine::domain::orderbook::{ContractSpec, TickBasedOrderBook};
use trading_system_engine::shared::protocol::OrderType;
use trading_system_engine::trading::combo::engine::ComboEngine;
use trading_system_engine::trading::core::protocol::{Hedge, Offset, Side};

fn main() {
    let orders = 200_000u64;

    let mut engine = ComboEngine::new(
        ContractSpec::new("LEG1", 1, 1_000, 100_000),
        ContractSpec::new("LEG2", 1, 1, 100_000),
    );
    let start = Instant::now();
    for seq in 1..=orders {
        engine.queue(
            seq,
            1,
            Side::Buy,
            49_000 + (seq % 100),
            1,
            Offset::Open,
            Hedge::Spec,
        );
    }
    let elapsed = start.elapsed();
    black_box(engine.queue_len());
    println!(
        "combo queue: {orders} in {elapsed:?} ({:.0} order/s)",
        orders as f64 / elapsed.as_secs_f64()
    );

    let start = Instant::now();
    for seq in 1..=orders {
        engine.cancel(seq);
    }
    let elapsed = start.elapsed();
    black_box(engine.queue_len());
    println!(
        "combo cancel: {orders} in {elapsed:?} ({:.0} cancel/s)",
        orders as f64 / elapsed.as_secs_f64()
    );

    // Atomic execution: replenish both legs, then execute one combo buy.
    let rounds = 50_000u64;
    let mut engine = ComboEngine::new(
        ContractSpec::new("LEG1", 1, 1_000, 100_000),
        ContractSpec::new("LEG2", 1, 1, 100_000),
    );
    let start = Instant::now();
    for seq in 1..=rounds {
        engine
            .executor_mut()
            .leg1
            .insert_resting_order(seq * 2, OrderType::Sell, 50_000, 1, 1, seq * 2)
            .unwrap();
        engine
            .executor_mut()
            .leg2
            .insert_resting_order(seq * 2 + 1, OrderType::Buy, 100, 1, 1, seq * 2 + 1)
            .unwrap();
        engine.queue(seq, 1, Side::Buy, 49_900, 1, Offset::Open, Hedge::Spec);
        let _ = engine.try_execute_best(Side::Buy);
    }
    let elapsed = start.elapsed();
    black_box(engine.leg1_positions());
    println!(
        "combo atomic fills: {rounds} in {elapsed:?} ({:.0} fill/s)",
        rounds as f64 / elapsed.as_secs_f64()
    );

    // Isolate snapshot cost on a book holding one order.
    let mut book = TickBasedOrderBook::new(ContractSpec::new("LEG", 1, 1_000, 100_000));
    book.insert_resting_order(1, OrderType::Sell, 50_000, 1, 1, 1)
        .unwrap();
    let mut buffer = Vec::new();
    let start = Instant::now();
    for _ in 0..rounds {
        book.snapshot_orders_into(&mut buffer);
        black_box(buffer.as_slice());
    }
    let elapsed = start.elapsed();
    println!(
        "snapshot only: {rounds} in {elapsed:?} ({:.0} ns/snapshot)",
        elapsed.as_nanos() as f64 / rounds as f64
    );
    drop(engine);
}
