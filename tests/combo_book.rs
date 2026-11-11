//! Base Combo Book behavior through the [`InstrumentRegistry`] seam.

use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::core::{
    ComboBookError, ComboCommandError, ComboEvent, ComboPair, ComboPairMargin, InstrumentRegistry,
};

fn spec(symbol: &str) -> ContractSpec {
    ContractSpec::new(symbol, 1, 40_000, 70_000)
}

fn registry_with_pair() -> InstrumentRegistry {
    let mut registry = InstrumentRegistry::new();
    registry.register(1, spec("IF2501")).unwrap();
    registry.register(2, spec("IF2502")).unwrap();
    registry
        .register_pair(ComboPair::new(
            10,
            1,
            2,
            1,
            ComboPairMargin::new(300, 1_200, 1_000),
        ))
        .unwrap();
    registry
}

fn combo_order(
    client_order_id: u64,
    user_id: u32,
    session_id: u64,
    side: Side,
    spread_price: i64,
    quantity: u64,
) -> Command {
    let mut command = Command::zeroed();
    command.command_type = CommandType::NewOrder;
    command.combo_id = 10;
    command.client_order_id = client_order_id;
    command.user_id = user_id;
    command.session_id = session_id;
    command.price_type = PriceType::Limit;
    command.time_condition = TimeCondition::Gfd;
    command.volume_condition = VolumeCondition::Any;
    command.side = side;
    command.offset = Offset::Open;
    command.hedge = Hedge::Spec;
    command.price_ticks = spread_price;
    command.quantity = quantity;
    command.trigger_type = TriggerType::None;
    command
}

fn cancel_order(order_id: u64, user_id: u32) -> Command {
    let mut command = Command::zeroed();
    command.command_type = CommandType::Cancel;
    command.combo_id = 10;
    command.cancel_order_id = order_id;
    command.user_id = user_id;
    command
}

fn registry_with_two_pairs() -> InstrumentRegistry {
    let mut registry = registry_with_pair();
    registry.register(3, spec("IC2501")).unwrap();
    registry
        .register_pair(ComboPair::new(
            20,
            1,
            3,
            5,
            ComboPairMargin::new(300, 1_200, 1_000),
        ))
        .unwrap();
    registry
}

#[test]
fn same_price_orders_keep_time_priority() {
    let mut registry = registry_with_pair();

    let first = registry
        .execute_combo(combo_order(101, 1, 11, Side::Buy, -50, 5))
        .unwrap();
    let first = match first.as_slice() {
        [ComboEvent::Rested { order }] => *order,
        other => panic!("expected one rested order, got {other:?}"),
    };
    let second = registry
        .execute_combo(combo_order(102, 1, 11, Side::Buy, -50, 7))
        .unwrap();
    let second = match second.as_slice() {
        [ComboEvent::Rested { order }] => *order,
        other => panic!("expected one rested order, got {other:?}"),
    };

    assert_eq!(first.order_id, 1);
    assert_eq!(second.order_id, 2);
    assert_eq!(first.owner.user_id, 1);
    assert_eq!(first.owner.session_id, 11);
    assert_eq!(first.owner.client_order_id, 101);
    assert_eq!(second.owner.client_order_id, 102);
    assert!(first.arrival_seq < second.arrival_seq);

    let book = registry.combo_book(10).unwrap();
    assert_eq!(book.len(), 2);
    assert_eq!(book.best_order(Side::Buy).unwrap().order_id, first.order_id);

    registry
        .execute_combo(cancel_order(first.order_id, 1))
        .unwrap();
    let book = registry.combo_book(10).unwrap();
    assert_eq!(
        book.best_order(Side::Buy).unwrap().order_id,
        second.order_id
    );
}

#[test]
fn better_spread_price_wins_before_arrival_time() {
    let mut registry = registry_with_pair();

    for (client_order_id, price) in [(201, 100), (202, 110), (203, 105)] {
        registry
            .execute_combo(combo_order(client_order_id, 1, 11, Side::Buy, price, 5))
            .unwrap();
    }
    // Ask limits stay above the best bid so direct matching leaves both
    // sides resting for the priority assertions.
    for (client_order_id, price) in [(301, 125), (302, 130), (303, 120)] {
        registry
            .execute_combo(combo_order(client_order_id, 1, 11, Side::Sell, price, 5))
            .unwrap();
    }

    let book = registry.combo_book(10).unwrap();
    assert_eq!(book.best_bid(), Some(110));
    assert_eq!(book.best_ask(), Some(120));
    assert_eq!(
        book.best_order(Side::Buy).unwrap().owner.client_order_id,
        202
    );
    assert_eq!(
        book.best_order(Side::Sell).unwrap().owner.client_order_id,
        303
    );
}

#[test]
fn cancel_removes_the_order_and_preserves_remaining_quantity() {
    let mut registry = registry_with_pair();
    let rested = registry
        .execute_combo(combo_order(401, 7, 21, Side::Sell, 45, 8))
        .unwrap();
    let rested = match rested.as_slice() {
        [ComboEvent::Rested { order }] => *order,
        other => panic!("expected one rested order, got {other:?}"),
    };
    assert_eq!(rested.quantity, 8);
    assert_eq!(rested.remaining, 8);
    assert_eq!(
        registry
            .combo_book(10)
            .unwrap()
            .order(rested.order_id)
            .unwrap()
            .remaining,
        8
    );

    let canceled = registry
        .execute_combo(cancel_order(rested.order_id, 7))
        .unwrap();
    let canceled = match canceled.as_slice() {
        [ComboEvent::Canceled { order }] => *order,
        other => panic!("expected one canceled order, got {other:?}"),
    };
    assert_eq!(canceled.order_id, rested.order_id);
    assert_eq!(canceled.quantity, 8);
    assert_eq!(canceled.remaining, 8);

    let book = registry.combo_book(10).unwrap();
    assert!(book.is_empty());
    assert_eq!(book.best_ask(), None);
    assert!(book.order(rested.order_id).is_none());
}

#[test]
fn cancel_removes_only_the_targeted_price_level() {
    let mut registry = registry_with_pair();
    for (client_order_id, price) in [(451, 100), (452, 110)] {
        registry
            .execute_combo(combo_order(client_order_id, 1, 11, Side::Buy, price, 5))
            .unwrap();
    }

    registry.execute_combo(cancel_order(1, 1)).unwrap();
    let book = registry.combo_book(10).unwrap();
    assert_eq!(book.len(), 1);
    assert_eq!(book.best_bid(), Some(110));
    assert_eq!(book.best_order(Side::Buy).unwrap().order_id, 2);

    registry.execute_combo(cancel_order(2, 1)).unwrap();
    assert!(registry.combo_book(10).unwrap().is_empty());
}

#[test]
fn duplicate_cancel_is_rejected_without_mutating_the_book() {
    let mut registry = registry_with_pair();
    let rested = registry
        .execute_combo(combo_order(501, 7, 21, Side::Buy, 45, 8))
        .unwrap();
    let order_id = match rested.as_slice() {
        [ComboEvent::Rested { order }] => order.order_id,
        other => panic!("expected one rested order, got {other:?}"),
    };

    registry.execute_combo(cancel_order(order_id, 7)).unwrap();
    let error = registry
        .execute_combo(cancel_order(order_id, 7))
        .unwrap_err();
    assert_eq!(
        error,
        ComboCommandError::Book(ComboBookError::UnknownOrder { order_id })
    );
    assert!(registry.combo_book(10).unwrap().is_empty());
}

#[test]
fn negative_spread_prices_keep_side_priority() {
    let mut registry = registry_with_pair();
    for (client_order_id, side, price) in [
        (601, Side::Buy, -75),
        (602, Side::Buy, -60),
        (603, Side::Sell, -50),
        (604, Side::Sell, -55),
    ] {
        registry
            .execute_combo(combo_order(client_order_id, 1, 11, side, price, 3))
            .unwrap();
    }

    let book = registry.combo_book(10).unwrap();
    assert_eq!(book.best_bid(), Some(-60));
    assert_eq!(book.best_ask(), Some(-55));
    assert_eq!(book.best_order(Side::Buy).unwrap().spread_price, -60);
    assert_eq!(book.best_order(Side::Sell).unwrap().spread_price, -55);
    assert_eq!(book.order(1).unwrap().spread_price, -75);
}

#[test]
fn market_fak_fok_and_stop_orders_are_rejected_explicitly() {
    let mut registry = registry_with_pair();

    let mut market = combo_order(701, 1, 11, Side::Buy, 10, 5);
    market.price_type = PriceType::Market;
    assert_eq!(
        registry.execute_combo(market).unwrap_err(),
        ComboCommandError::UnsupportedPriceType {
            price_type: PriceType::Market,
        }
    );

    let mut advanced = combo_order(702, 1, 11, Side::Buy, 10, 5);
    advanced.price_type = PriceType::AdvancedMarket;
    assert_eq!(
        registry.execute_combo(advanced).unwrap_err(),
        ComboCommandError::UnsupportedPriceType {
            price_type: PriceType::AdvancedMarket,
        }
    );

    let mut fak = combo_order(703, 1, 11, Side::Buy, 10, 5);
    fak.time_condition = TimeCondition::Ioc;
    assert_eq!(
        registry.execute_combo(fak).unwrap_err(),
        ComboCommandError::UnsupportedTimeCondition {
            time_condition: TimeCondition::Ioc,
        }
    );

    let mut gis = combo_order(704, 1, 11, Side::Buy, 10, 5);
    gis.time_condition = TimeCondition::Gis;
    assert_eq!(
        registry.execute_combo(gis).unwrap_err(),
        ComboCommandError::UnsupportedTimeCondition {
            time_condition: TimeCondition::Gis,
        }
    );

    let mut all_or_none = combo_order(705, 1, 11, Side::Buy, 10, 5);
    all_or_none.volume_condition = VolumeCondition::All;
    assert_eq!(
        registry.execute_combo(all_or_none).unwrap_err(),
        ComboCommandError::UnsupportedVolumeCondition {
            volume_condition: VolumeCondition::All,
        }
    );

    let mut minimum = combo_order(706, 1, 11, Side::Buy, 10, 5);
    minimum.volume_condition = VolumeCondition::Minimum;
    assert_eq!(
        registry.execute_combo(minimum).unwrap_err(),
        ComboCommandError::UnsupportedVolumeCondition {
            volume_condition: VolumeCondition::Minimum,
        }
    );

    let mut stop = combo_order(707, 1, 11, Side::Buy, 10, 5);
    stop.trigger_type = TriggerType::Stop;
    assert_eq!(
        registry.execute_combo(stop).unwrap_err(),
        ComboCommandError::UnsupportedTriggerType {
            trigger_type: TriggerType::Stop,
        }
    );

    assert!(registry.combo_book(10).unwrap().is_empty());
}

#[test]
fn unknown_pair_and_invalid_quantity_are_rejected_before_resting() {
    let mut registry = registry_with_pair();

    let mut unknown_pair = combo_order(801, 1, 11, Side::Buy, 10, 5);
    unknown_pair.combo_id = 99;
    assert_eq!(
        registry.execute_combo(unknown_pair).unwrap_err(),
        ComboCommandError::UnknownPair { pair_id: 99 }
    );

    let zero_quantity = combo_order(802, 1, 11, Side::Buy, 10, 0);
    assert_eq!(
        registry.execute_combo(zero_quantity).unwrap_err(),
        ComboCommandError::ZeroQuantity { pair_id: 10 }
    );

    assert!(registry.combo_book(10).unwrap().is_empty());
}

#[test]
fn malformed_and_unknown_cancels_are_rejected() {
    let mut registry = registry_with_pair();

    assert_eq!(
        registry.execute_combo(cancel_order(0, 1)).unwrap_err(),
        ComboCommandError::MissingCancelOrderId
    );
    assert_eq!(
        registry.execute_combo(cancel_order(999, 1)).unwrap_err(),
        ComboCommandError::Book(ComboBookError::UnknownOrder { order_id: 999 })
    );

    let mut tick = Command::zeroed();
    tick.command_type = CommandType::TimeTick;
    tick.combo_id = 10;
    assert_eq!(
        registry.execute_combo(tick).unwrap_err(),
        ComboCommandError::UnsupportedCommandType {
            command_type: CommandType::TimeTick,
        }
    );
}

#[test]
fn each_combo_pair_owns_an_independent_book() {
    let mut registry = registry_with_two_pairs();

    let mut first = combo_order(901, 1, 11, Side::Buy, 25, 4);
    first.combo_id = 10;
    registry.execute_combo(first).unwrap();
    let mut second = combo_order(902, 1, 11, Side::Sell, 30, 6);
    second.combo_id = 20;
    registry.execute_combo(second).unwrap();

    assert_eq!(registry.combo_book(10).unwrap().len(), 1);
    assert_eq!(registry.combo_book(20).unwrap().len(), 1);
    assert_eq!(registry.combo_book(10).unwrap().pair_id(), 10);
    assert_eq!(registry.combo_book(20).unwrap().pair_id(), 20);
    assert_eq!(registry.combo_book(10).unwrap().best_bid(), Some(25));
    assert_eq!(registry.combo_book(20).unwrap().best_ask(), Some(30));

    let mut wrong_pair_cancel = cancel_order(1, 1);
    wrong_pair_cancel.combo_id = 20;
    assert_eq!(
        registry.execute_combo(wrong_pair_cancel).unwrap_err(),
        ComboCommandError::Book(ComboBookError::UnknownOrder { order_id: 1 })
    );
    assert_eq!(registry.combo_book(20).unwrap().len(), 1);
}

#[test]
fn cancel_by_another_owner_is_rejected_without_mutating_book() {
    let mut registry = registry_with_pair();
    let order = registry
        .execute_combo(combo_order(950, 1, 11, Side::Buy, 10, 5))
        .unwrap();
    let order_id = match order.as_slice() {
        [ComboEvent::Rested { order }] => order.order_id,
        other => panic!("expected rested order, got {other:?}"),
    };
    let error = registry
        .execute_combo(cancel_order(order_id, 2))
        .unwrap_err();
    assert!(matches!(
        error,
        ComboCommandError::Book(ComboBookError::NotOrderOwner { .. })
    ));
    assert!(registry.combo_book(10).unwrap().order(order_id).is_some());
}
