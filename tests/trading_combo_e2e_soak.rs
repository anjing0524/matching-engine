//! Many combo commands through the middleware combo ring into the two-leg engine.

use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::trading::combo_engine::ComboEngine;
use trading_system_engine::trading::combo_runtime::ComboRuntime;
use trading_system_engine::trading::protocol::{Command, CommandType, Hedge, Offset, Side};
use trading_system_engine::trading::transport::create;
use trading_system_engine::shared::protocol::OrderType;

#[test]
fn ten_thousand_combo_commands_fill() {
    let path = std::env::temp_dir().join(format!(
        "matching-engine-combo-e2e-soak-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let (producer, consumer) = create(&path, 16_384).unwrap();

    let mut engine = ComboEngine::new(
        ContractSpec::new("LEG1", 1, 1_000, 100_000),
        ContractSpec::new("LEG2", 1, 1, 100_000),
    );
    let orders = 10_000u64;
    for seq in 1..=orders {
        let level = seq % 5;
        engine
            .executor_mut()
            .leg1
            .insert_resting_order(seq, OrderType::Sell, 50_000 + level, 1, 1, seq)
            .unwrap();
        engine
            .executor_mut()
            .leg2
            .insert_resting_order(seq, OrderType::Buy, 100 + level, 1, 1, seq + orders)
            .unwrap();
    }
    let mut runtime = ComboRuntime::new(engine).with_command_consumer(consumer);

    for seq in 1..=orders {
        let mut command = Command::zeroed();
        command.command_type = CommandType::NewOrder;
        command.combo_id = 7;
        command.client_order_id = seq;
        command.user_id = 1;
        command.side = Side::Buy;
        command.price_ticks = 50_000;
        command.quantity = 1;
        command.offset = Offset::Open;
        command.hedge = Hedge::Spec;
        producer.send(&command).unwrap();
    }
    assert_eq!(runtime.poll_commands() as u64, orders);
    loop {
        match runtime.engine_mut().try_execute_best(Side::Buy) {
            None => break,
            Some(Ok(_)) => {}
            Some(Err(error)) => panic!("combo execution failed: {error:?}"),
        }
    }
    assert_eq!(runtime.engine().queue_len(), 0);
    assert_eq!(runtime.engine().open_qty(1), 0);
    let _ = std::fs::remove_file(&path);
}
