//! Combo queue/cancel and atomic fill soak.

use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::shared::protocol::OrderType;
use trading_system_engine::trading::combo::combo_engine::ComboEngine;
use trading_system_engine::trading::core::protocol::{Hedge, Offset, Side};

#[test]
fn combo_queue_cancel_soak() {
    let mut engine = ComboEngine::new(
        ContractSpec::new("LEG1", 1, 1_000, 100_000),
        ContractSpec::new("LEG2", 1, 1, 100_000),
    );
    for round in 0..5u64 {
        let orders = 200_000u64;
        for seq in 1..=orders {
            let order_id = round * orders + seq;
            assert!(engine.queue(
                order_id,
                1,
                Side::Buy,
                49_000 + (seq % 100),
                1,
                Offset::Open,
                Hedge::Spec,
            ));
        }
        assert_eq!(engine.queue_len() as u64, orders);
        for seq in 1..=orders {
            let order_id = round * orders + seq;
            assert!(engine.cancel(order_id));
        }
        assert_eq!(engine.queue_len(), 0);
    }
}

#[test]
fn combo_atomic_fill_soak() {
    let mut engine = ComboEngine::new(
        ContractSpec::new("LEG1", 1, 1_000, 100_000),
        ContractSpec::new("LEG2", 1, 1, 100_000),
    );
    let rounds = 50_000u64;
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
        assert!(engine.queue(seq, 1, Side::Buy, 49_900, 1, Offset::Open, Hedge::Spec));
        let fill = engine.try_execute_best(Side::Buy).unwrap().unwrap();
        assert_eq!(fill.quantity, 1);
    }
    assert_eq!(engine.queue_len(), 0);
}
