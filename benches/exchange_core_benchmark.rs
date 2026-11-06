//! Exchange-core throughput on fixed 256-byte commands.

use matching_engine::domain::orderbook::ContractSpec;
use matching_engine::exchange::core::ExchangeCore;
use matching_engine::exchange::plugins::NoPlugins;
use matching_engine::exchange::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use matching_engine::exchange::runner::CoreRunner;
use matching_engine::exchange::transport::create;
use matching_engine::shared::SymbolId;
use std::hint::black_box;
use std::time::Instant;

fn command(seq: u64, side: Side, price: i64) -> Command {
    let mut command = Command::zeroed();
    command.gateway_seq = seq;
    command.client_order_id = seq;
    command.session_id = 1;
    command.member_id = 1;
    command.user_id = (seq % 16) as u32;
    command.seat_id = 1;
    command.instrument_id = 1;
    command.command_type = CommandType::NewOrder;
    command.price_type = PriceType::Limit;
    command.time_condition = TimeCondition::Gfd;
    command.volume_condition = VolumeCondition::Any;
    command.side = side;
    command.offset = Offset::Open;
    command.hedge = Hedge::Spec;
    command.price_ticks = price;
    command.quantity = 1;
    command.trigger_type = TriggerType::None;
    command
}

fn main() {
    let path = std::env::temp_dir().join(format!(
        "matching-engine-bench-commands-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let (producer, consumer) = create(&path, 1 << 20).unwrap();
    let spec = ContractSpec::new("BTCUSDT", 1, 1, 1_000_000);
    let core = ExchangeCore::new(spec, SymbolId::intern("BTCUSDT"));
    let mut runner = CoreRunner::new(core, consumer, NoPlugins);

    let orders = 200_000u64;
    for seq in 1..=orders {
        let side = if seq % 2 == 0 { Side::Buy } else { Side::Sell };
        let price = 500_000 + (seq % 100) as i64;
        producer.send(&command(seq, side, price)).unwrap();
    }

    let start = Instant::now();
    let executed = runner.poll();
    let elapsed = start.elapsed();
    black_box(runner.core());
    assert_eq!(executed as u64, orders);

    let per_sec = orders as f64 / elapsed.as_secs_f64();
    println!("exchange core crossing: {orders} commands in {elapsed:?} ({per_sec:.0} cmd/s)");

    // Resting-only workload: no trades, one new price level per order.
    for seq in 1..=orders {
        producer
            .send(&command(orders + seq, Side::Buy, 600_000 + seq as i64))
            .unwrap();
    }
    let start = Instant::now();
    let executed = runner.poll();
    let elapsed = start.elapsed();
    black_box(runner.core());
    assert_eq!(executed as u64, orders);
    let per_sec = orders as f64 / elapsed.as_secs_f64();
    println!("exchange core resting: {orders} commands in {elapsed:?} ({per_sec:.0} cmd/s)");

    let _ = std::fs::remove_file(&path);
}
