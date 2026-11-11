//! Duration-based core soak.
//!
//! Ignored by default; duration is controlled by `TRADING_SOAK_SECS`:
//! `TRADING_SOAK_SECS=3600 cargo test --release --test trading_timed_soak -- --ignored --nocapture`

use std::time::{Duration, Instant};
use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::shared::SymbolId;
use trading_system_engine::trading::core::TradingCore;
use trading_system_engine::trading::core::plugins::NoPlugins;
use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::transport::create;
use trading_system_engine::trading::transport::runner::CoreRunner;

fn command(seq: u64) -> Command {
    let mut command = Command::zeroed();
    command.gateway_seq = seq;
    command.client_order_id = seq;
    command.session_id = 1;
    command.member_id = 1;
    command.user_id = (seq % 64) as u32;
    command.seat_id = 1;
    command.instrument_id = 1;
    command.command_type = CommandType::NewOrder;
    command.price_type = PriceType::Limit;
    command.time_condition = TimeCondition::Gfd;
    command.volume_condition = VolumeCondition::Any;
    command.side = if seq.is_multiple_of(2) {
        Side::Buy
    } else {
        Side::Sell
    };
    command.offset = Offset::Open;
    command.hedge = Hedge::Spec;
    command.price_ticks = 500_000 + (seq % 100) as i64;
    command.quantity = 1;
    command.trigger_type = TriggerType::None;
    command
}

#[test]
#[ignore = "duration soak; run explicitly"]
fn timed_core_soak() {
    let seconds = std::env::var("TRADING_SOAK_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(60);
    let path = std::env::temp_dir().join(format!(
        "trading-system-engine-timed-soak-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let (producer, consumer) = create(&path, 1 << 17).unwrap();
    let spec = ContractSpec::new("BTCUSDT", 1, 1, 1_000_000);
    let core = TradingCore::new(spec, SymbolId::intern("BTCUSDT"));
    let mut runner = CoreRunner::new(core, consumer, NoPlugins);

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let batch = 100_000u64;
    let mut seq = 1u64;
    let mut total = 0u64;
    let started = Instant::now();
    while Instant::now() < deadline {
        for _ in 0..batch {
            producer.send(&command(seq)).unwrap();
            seq += 1;
        }
        total += runner.poll() as u64;
        let _ = runner.poll_events();
    }
    drop(producer);
    let elapsed = started.elapsed();
    println!(
        "timed core soak: {total} commands in {elapsed:?} ({:.0} cmd/s)",
        total as f64 / elapsed.as_secs_f64()
    );
    assert!(total > 0);
    let _ = std::fs::remove_file(&path);
}
