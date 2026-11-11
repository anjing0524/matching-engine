//! Multi-contract core behavior through the public registry seam.

use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::core::public_events::TradingPhase;
use trading_system_engine::trading::core::{CoreEvent, InstrumentRegistry, RegistryError};

fn spec(symbol: &str) -> ContractSpec {
    ContractSpec::new(symbol, 1, 40_000, 70_000)
}

fn new_order(instrument_id: u32, side: Side, price_ticks: i64, quantity: u64) -> Command {
    let mut command = Command::zeroed();
    command.command_type = CommandType::NewOrder;
    command.client_order_id = 1;
    command.user_id = 1;
    command.instrument_id = instrument_id;
    command.price_type = PriceType::Limit;
    command.time_condition = TimeCondition::Gfd;
    command.volume_condition = VolumeCondition::Any;
    command.side = side;
    command.offset = Offset::Open;
    command.hedge = Hedge::Spec;
    command.price_ticks = price_ticks;
    command.quantity = quantity;
    command.trigger_type = TriggerType::None;
    command
}

#[test]
fn single_contract_accepts_and_cancels_like_trading_core() {
    let mut registry = InstrumentRegistry::new();
    registry.register(7, spec("BTCUSDT")).unwrap();

    let events = registry
        .execute(new_order(7, Side::Buy, 50_000, 10))
        .unwrap();
    let [
        CoreEvent::Accepted {
            order_id,
            user_id: 1,
            remaining_quantity: 10,
            ..
        },
    ] = events.as_slice()
    else {
        panic!("expected one accepted event, got {events:?}");
    };
    let order_id = *order_id;
    assert_eq!(registry.core(7).unwrap().best_bid(), Some(50_000));

    let mut cancel = Command::zeroed();
    cancel.command_type = CommandType::Cancel;
    cancel.instrument_id = 7;
    cancel.user_id = 1;
    cancel.cancel_order_id = order_id;
    assert_eq!(
        registry.execute(cancel).unwrap(),
        vec![CoreEvent::Canceled {
            order_id,
            user_id: 1,
        }]
    );
    assert_eq!(registry.core(7).unwrap().best_bid(), None);
}

#[test]
fn single_contract_fills_update_both_owners_and_keep_private_order() {
    use trading_system_engine::trading::core::positions::PositionKey;
    use trading_system_engine::trading::core::private_events::PrivateStatus;

    let mut registry = InstrumentRegistry::new();
    registry.register(7, spec("BTCUSDT")).unwrap();
    registry
        .execute(new_order(7, Side::Buy, 50_000, 10))
        .unwrap();

    let mut sell = new_order(7, Side::Sell, 50_000, 10);
    sell.user_id = 2;
    sell.client_order_id = 2;
    let events = registry.execute(sell).unwrap();
    let [
        CoreEvent::Trade {
            price_ticks: 50_000,
            quantity: 10,
            buyer_user_id: 1,
            seller_user_id: 2,
            ..
        },
    ] = events.as_slice()
    else {
        panic!("expected one trade event, got {events:?}");
    };

    let core = registry.core_mut(7).unwrap();
    assert_eq!(
        core.positions()
            .get(PositionKey::new(1, Side::Buy, Hedge::Spec))
            .today,
        10
    );
    assert_eq!(
        core.positions()
            .get(PositionKey::new(2, Side::Sell, Hedge::Spec))
            .today,
        10
    );
    assert_eq!(core.best_bid(), None);
    assert_eq!(core.best_ask(), None);

    let private = core.drain_private_events();
    assert_eq!(private.len(), 3);
    assert_eq!(private[0].status, PrivateStatus::Accepted);
    assert_eq!(private[0].seq, 1);
    assert_eq!(private[1].status, PrivateStatus::Filled);
    assert_eq!(private[1].seq, 2);
    assert_eq!(private[2].status, PrivateStatus::Filled);
    assert_eq!(private[2].seq, 3);
}

#[test]
fn two_instruments_hold_independent_books() {
    let mut registry = InstrumentRegistry::new();
    registry.register(1, spec("IF2501")).unwrap();
    registry.register(2, spec("IF2502")).unwrap();

    registry
        .execute(new_order(1, Side::Buy, 50_000, 10))
        .unwrap();
    registry
        .execute(new_order(2, Side::Sell, 50_000, 10))
        .unwrap();

    let first = registry.core(1).unwrap();
    assert_eq!(first.best_bid(), Some(50_000));
    assert_eq!(first.best_ask(), None);
    let second = registry.core(2).unwrap();
    assert_eq!(second.best_bid(), None);
    assert_eq!(second.best_ask(), Some(50_000));
}

#[test]
fn unknown_instrument_rejects_order_and_cancel_before_mutation() {
    let mut registry = InstrumentRegistry::new();
    registry.register(1, spec("IF2501")).unwrap();
    registry
        .execute(new_order(1, Side::Buy, 50_000, 10))
        .unwrap();

    let error = registry
        .execute(new_order(2, Side::Sell, 50_000, 10))
        .unwrap_err();
    assert_eq!(error, RegistryError::UnknownInstrument { instrument_id: 2 });
    assert_eq!(registry.len(), 1);
    assert_eq!(registry.core(1).unwrap().best_bid(), Some(50_000));

    let mut cancel = Command::zeroed();
    cancel.command_type = CommandType::Cancel;
    cancel.instrument_id = 9;
    cancel.user_id = 1;
    cancel.cancel_order_id = 1;
    assert_eq!(
        registry.execute(cancel).unwrap_err(),
        RegistryError::UnknownInstrument { instrument_id: 9 }
    );
    assert_eq!(registry.core(1).unwrap().best_bid(), Some(50_000));
}

#[test]
fn duplicate_registration_keeps_the_first_instrument() {
    let mut registry = InstrumentRegistry::new();
    registry.register(1, spec("IF2501")).unwrap();

    let error = registry.register(1, spec("IF2502")).unwrap_err();
    assert_eq!(
        error,
        RegistryError::DuplicateInstrument { instrument_id: 1 }
    );
    assert_eq!(registry.len(), 1);
    assert_eq!(registry.spec(1).unwrap().symbol, "IF2501");
}

#[test]
fn invalid_contract_specs_are_rejected_before_matching() {
    let mut registry = InstrumentRegistry::new();
    let zero_tick = ContractSpec {
        symbol: "BAD".to_string(),
        tick_size: 0,
        min_price: 40_000,
        max_price: 70_000,
        queue_capacity: 2_048,
    };
    assert_eq!(
        registry.register(1, zero_tick).unwrap_err(),
        RegistryError::InvalidSpec {
            instrument_id: 1,
            reason: "tick size must be positive",
        }
    );

    let off_range = ContractSpec {
        symbol: "BAD".to_string(),
        tick_size: 2,
        min_price: 40_000,
        max_price: 70_001,
        queue_capacity: 2_048,
    };
    assert_eq!(
        registry.register(2, off_range).unwrap_err(),
        RegistryError::InvalidSpec {
            instrument_id: 2,
            reason: "price range must be divisible by tick size",
        }
    );
    assert!(registry.is_empty());
}

#[test]
fn lookup_and_validation_are_centralized_in_the_registry() {
    let mut registry = InstrumentRegistry::new();
    assert!(registry.is_empty());
    registry.register(3, spec("IF2503")).unwrap();

    assert_eq!(registry.len(), 1);
    assert!(registry.contains(3));
    assert!(!registry.contains(4));
    assert_eq!(registry.spec(3).unwrap().symbol, "IF2503");
    assert_eq!(registry.spec(3).unwrap().tick_size, 1);
    assert!(registry.spec(4).is_none());
    assert!(registry.core(3).is_some());
    assert!(registry.core(4).is_none());
}

#[test]
fn time_and_control_commands_update_every_instrument() {
    let mut registry = InstrumentRegistry::new();
    registry.register(1, spec("IF2501")).unwrap();
    registry.register(2, spec("IF2502")).unwrap();

    let mut tick = Command::zeroed();
    tick.command_type = CommandType::TimeTick;
    tick.global_seq = 42;
    tick.trading_ts_ns = 123;
    assert!(registry.execute(tick).unwrap().is_empty());

    let mut control = Command::zeroed();
    control.command_type = CommandType::Control;
    control.flags = TradingPhase::Break.code();
    control.quantity = 3;
    assert!(registry.execute(control).unwrap().is_empty());

    for instrument_id in [1, 2] {
        let core = registry.core_mut(instrument_id).unwrap();
        assert_eq!(core.last_tick_seq(), 42);
        assert_eq!(core.last_trading_ts_ns(), 123);
        assert_eq!(core.phase(), TradingPhase::Break);
        assert_eq!(core.trading_day(), 3);
        let public = core.drain_public_events();
        assert_eq!(public.len(), 2);
    }
}
