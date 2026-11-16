//! Implied-in combo matching through the [`InstrumentRegistry`] seam.

use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::core::{
    ComboEvent, ComboPair, ComboPairMargin, CoreEvent, ImpliedComboFill, InstrumentRegistry,
};

fn spec(symbol: &str) -> ContractSpec {
    ContractSpec::new(symbol, 1, 30_000, 70_000)
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
    registry.set_prior_settlement(1, 50_000).unwrap();
    registry.set_prior_settlement(2, 45_000).unwrap();
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

fn leg_order(
    instrument_id: u32,
    user_id: u32,
    side: Side,
    price_ticks: i64,
    quantity: u64,
) -> Command {
    let mut command = Command::zeroed();
    command.command_type = CommandType::NewOrder;
    command.client_order_id = u64::from(user_id);
    command.user_id = user_id;
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

fn rest_leg(registry: &mut InstrumentRegistry, command: Command) -> u64 {
    let events = registry.execute(command).unwrap();
    let [CoreEvent::Accepted { order_id, .. }] = events.as_slice() else {
        panic!("expected one accepted leg order, got {events:?}");
    };
    *order_id
}

fn only_implied_fill(events: &[ComboEvent]) -> ImpliedComboFill {
    match events {
        [ComboEvent::ImpliedFilled { fill }] => *fill,
        other => panic!("expected exactly one implied-in fill, got {other:?}"),
    }
}

#[test]
fn buy_spread_consumes_both_leg_books_atomically() {
    use trading_system_engine::trading::core::positions::PositionKey;
    use trading_system_engine::trading::core::private_events::PrivateStatus;
    use trading_system_engine::trading::core::public_events::PublicEventType;

    let mut registry = registry_with_pair();
    let leg1_order = rest_leg(&mut registry, leg_order(1, 1, Side::Sell, 50_000, 5));
    let leg2_order = rest_leg(&mut registry, leg_order(2, 2, Side::Buy, 40_000, 5));
    for instrument_id in [1, 2] {
        let core = registry.core_mut(instrument_id).unwrap();
        core.drain_private_events();
        core.drain_public_events();
    }

    let events = registry
        .execute_combo(combo_order(1001, 3, 33, Side::Buy, 10_000, 5))
        .unwrap();

    let fill = only_implied_fill(&events);
    assert_eq!(fill.pair_id, 10);
    assert_eq!(fill.order_id, 1);
    assert_eq!(fill.owner.user_id, 3);
    assert_eq!(fill.owner.session_id, 33);
    assert_eq!(fill.owner.client_order_id, 1001);
    assert_eq!(fill.side, Side::Buy);
    assert_eq!(fill.spread_price, 10_000);
    assert_eq!(fill.quantity, 5);
    assert_eq!(fill.leg1_price_ticks, 50_000);
    assert_eq!(fill.leg2_price_ticks, 40_000);

    let book = registry.combo_book(10).unwrap();
    assert!(
        book.is_empty(),
        "a fully implied-filled taker must not rest"
    );
    assert_eq!(book.last_trade_price(), Some(10_000));

    for instrument_id in [1, 2] {
        let core = registry.core(instrument_id).unwrap();
        assert_eq!(core.best_bid(), None);
        assert_eq!(core.best_ask(), None);
    }
    assert_eq!(
        registry
            .core(1)
            .unwrap()
            .positions()
            .get(PositionKey::new(1, Side::Sell, Hedge::Spec))
            .today,
        5
    );
    assert_eq!(
        registry
            .core(2)
            .unwrap()
            .positions()
            .get(PositionKey::new(2, Side::Buy, Hedge::Spec))
            .today,
        5
    );

    let leg1_private = registry.core_mut(1).unwrap().drain_private_events();
    assert_eq!(leg1_private.len(), 1);
    assert_eq!(leg1_private[0].order_id, leg1_order);
    assert_eq!(leg1_private[0].account_id, 1);
    assert_eq!(leg1_private[0].filled_delta, 5);
    assert_eq!(leg1_private[0].status, PrivateStatus::Filled);
    let leg1_public = registry.core_mut(1).unwrap().drain_public_events();
    assert_eq!(leg1_public.len(), 1);
    assert_eq!(leg1_public[0].event_type, PublicEventType::Trade);
    assert_eq!(leg1_public[0].instrument_id, 1);
    assert_eq!(leg1_public[0].buy_order_id, 1);
    assert_eq!(leg1_public[0].sell_order_id, leg1_order);

    let leg2_private = registry.core_mut(2).unwrap().drain_private_events();
    assert_eq!(leg2_private.len(), 1);
    assert_eq!(leg2_private[0].order_id, leg2_order);
    assert_eq!(leg2_private[0].account_id, 2);
    assert_eq!(leg2_private[0].filled_delta, 5);
    assert_eq!(leg2_private[0].status, PrivateStatus::Filled);
    let leg2_public = registry.core_mut(2).unwrap().drain_public_events();
    assert_eq!(leg2_public.len(), 1);
    assert_eq!(leg2_public[0].event_type, PublicEventType::Trade);
    assert_eq!(leg2_public[0].instrument_id, 2);
    assert_eq!(leg2_public[0].sell_order_id, 1);
    assert_eq!(leg2_public[0].buy_order_id, leg2_order);
}

#[test]
fn sell_spread_consumes_both_leg_books_atomically() {
    use trading_system_engine::trading::core::positions::PositionKey;

    let mut registry = registry_with_pair();
    rest_leg(&mut registry, leg_order(1, 1, Side::Buy, 51_000, 4));
    rest_leg(&mut registry, leg_order(2, 2, Side::Sell, 41_000, 4));

    let events = registry
        .execute_combo(combo_order(1002, 3, 33, Side::Sell, 10_000, 4))
        .unwrap();

    let fill = only_implied_fill(&events);
    assert_eq!(fill.side, Side::Sell);
    assert_eq!(fill.spread_price, 10_000);
    assert_eq!(fill.quantity, 4);
    assert_eq!(fill.leg1_price_ticks, 51_000);
    assert_eq!(fill.leg2_price_ticks, 41_000);

    let book = registry.combo_book(10).unwrap();
    assert!(book.is_empty());
    assert_eq!(book.last_trade_price(), Some(10_000));
    assert_eq!(
        registry
            .core(1)
            .unwrap()
            .positions()
            .get(PositionKey::new(1, Side::Buy, Hedge::Spec))
            .today,
        4
    );
    assert_eq!(
        registry
            .core(2)
            .unwrap()
            .positions()
            .get(PositionKey::new(2, Side::Sell, Hedge::Spec))
            .today,
        4
    );
    for instrument_id in [1, 2] {
        assert_eq!(registry.core(instrument_id).unwrap().best_bid(), None);
        assert_eq!(registry.core(instrument_id).unwrap().best_ask(), None);
    }
}

#[test]
fn non_crossing_spread_rests_without_touching_leg_books() {
    let mut registry = registry_with_pair();
    rest_leg(&mut registry, leg_order(1, 1, Side::Sell, 50_000, 3));
    rest_leg(&mut registry, leg_order(2, 2, Side::Buy, 40_000, 3));
    for instrument_id in [1, 2] {
        let core = registry.core_mut(instrument_id).unwrap();
        core.drain_private_events();
        core.drain_public_events();
    }

    let events = registry
        .execute_combo(combo_order(1003, 3, 33, Side::Buy, 9_999, 3))
        .unwrap();

    let [ComboEvent::Rested { order }] = events.as_slice() else {
        panic!("expected the non-crossing combo order to rest, got {events:?}");
    };
    assert_eq!(order.remaining, 3);
    assert_eq!(registry.combo_book(10).unwrap().best_bid(), Some(9_999));
    assert_eq!(registry.combo_book(10).unwrap().last_trade_price(), None);
    assert_eq!(registry.core(1).unwrap().best_ask(), Some(50_000));
    assert_eq!(registry.core(2).unwrap().best_bid(), Some(40_000));
    for instrument_id in [1, 2] {
        let core = registry.core(instrument_id).unwrap();
        assert!(core.positions().snapshot().is_empty());
        assert!(core.private_events().is_empty());
        assert!(core.public_events().is_empty());
    }
}

#[test]
fn shortfall_on_either_leg_produces_no_fill_and_no_mutation() {
    // Missing second-leg liquidity.
    let mut registry = registry_with_pair();
    rest_leg(&mut registry, leg_order(1, 1, Side::Sell, 50_000, 5));
    registry.core_mut(1).unwrap().drain_private_events();
    registry.core_mut(1).unwrap().drain_public_events();

    let events = registry
        .execute_combo(combo_order(1004, 3, 33, Side::Buy, 10_000, 5))
        .unwrap();
    assert!(matches!(events.as_slice(), [ComboEvent::Rested { .. }]));
    assert_eq!(registry.core(1).unwrap().best_ask(), Some(50_000));
    assert!(registry.core(1).unwrap().positions().snapshot().is_empty());
    assert!(registry.core(1).unwrap().private_events().is_empty());
    assert!(registry.core(1).unwrap().public_events().is_empty());
    assert_eq!(registry.combo_book(10).unwrap().best_bid(), Some(10_000));

    // Missing first-leg liquidity in the other direction.
    let mut registry = registry_with_pair();
    rest_leg(&mut registry, leg_order(2, 2, Side::Buy, 40_000, 5));
    registry.core_mut(2).unwrap().drain_private_events();
    registry.core_mut(2).unwrap().drain_public_events();

    let events = registry
        .execute_combo(combo_order(1005, 3, 33, Side::Buy, 10_000, 5))
        .unwrap();
    assert!(matches!(events.as_slice(), [ComboEvent::Rested { .. }]));
    assert_eq!(registry.core(2).unwrap().best_bid(), Some(40_000));
    assert!(registry.core(2).unwrap().positions().snapshot().is_empty());
    assert!(registry.core(2).unwrap().private_events().is_empty());
    assert!(registry.core(2).unwrap().public_events().is_empty());
}

#[test]
fn multi_level_implied_fills_consume_minimum_and_rest_the_residual() {
    use trading_system_engine::trading::core::positions::PositionKey;

    let mut registry = registry_with_pair();
    rest_leg(&mut registry, leg_order(1, 1, Side::Sell, 50_000, 2));
    rest_leg(&mut registry, leg_order(1, 4, Side::Sell, 50_010, 2));
    rest_leg(&mut registry, leg_order(1, 5, Side::Sell, 50_020, 2));
    rest_leg(&mut registry, leg_order(2, 2, Side::Buy, 40_010, 2));
    rest_leg(&mut registry, leg_order(2, 3, Side::Buy, 40_000, 2));
    rest_leg(&mut registry, leg_order(2, 6, Side::Buy, 39_990, 2));

    let events = registry
        .execute_combo(combo_order(1006, 9, 99, Side::Buy, 10_010, 5))
        .unwrap();

    let fills: Vec<(i64, u64, u64, u64)> = events
        .iter()
        .filter_map(|event| match event {
            ComboEvent::ImpliedFilled { fill } => Some((
                fill.spread_price,
                fill.quantity,
                fill.leg1_price_ticks,
                fill.leg2_price_ticks,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        fills,
        vec![(9_990, 2, 50_000, 40_010), (10_010, 2, 50_010, 40_000)]
    );

    let residual = match events.last() {
        Some(ComboEvent::Rested { order }) => order,
        other => panic!("expected a resting combo residual, got {other:?}"),
    };
    assert_eq!(residual.quantity, 5);
    assert_eq!(residual.remaining, 1);

    let book = registry.combo_book(10).unwrap();
    assert_eq!(book.len(), 1, "only the base residual order may be stored");
    assert_eq!(book.best_bid(), Some(10_010));
    assert_eq!(book.order(residual.order_id.get()).unwrap().remaining, 1);
    assert_eq!(book.last_trade_price(), Some(10_010));

    assert_eq!(registry.core(1).unwrap().best_ask(), Some(50_020));
    assert_eq!(registry.core(2).unwrap().best_bid(), Some(39_990));
    assert_eq!(
        registry
            .core(1)
            .unwrap()
            .positions()
            .get(PositionKey::new(1, Side::Sell, Hedge::Spec))
            .today,
        2
    );
    assert_eq!(
        registry
            .core(1)
            .unwrap()
            .positions()
            .get(PositionKey::new(4, Side::Sell, Hedge::Spec))
            .today,
        2
    );
    assert_eq!(
        registry
            .core(2)
            .unwrap()
            .positions()
            .get(PositionKey::new(2, Side::Buy, Hedge::Spec))
            .today,
        2
    );
    assert!(
        registry
            .core(2)
            .unwrap()
            .positions()
            .snapshot()
            .iter()
            .all(|(key, _)| key.account_id != 6)
    );
}

#[test]
fn direct_combo_matching_runs_before_implied_in() {
    let mut registry = registry_with_pair();
    registry
        .execute_combo(combo_order(1007, 1, 11, Side::Sell, 100, 2))
        .unwrap();
    rest_leg(&mut registry, leg_order(1, 2, Side::Sell, 50_000, 3));
    rest_leg(&mut registry, leg_order(2, 3, Side::Buy, 50_000, 3));

    let events = registry
        .execute_combo(combo_order(1008, 4, 44, Side::Buy, 100, 5))
        .unwrap();

    assert_eq!(events.len(), 2, "direct fill then implied-in: {events:?}");
    let ComboEvent::Filled { fill } = &events[0] else {
        panic!("direct matching must run first, got {events:?}");
    };
    assert_eq!(fill.spread_price, 100);
    assert_eq!(fill.quantity, 2);
    let ComboEvent::ImpliedFilled { fill } = &events[1] else {
        panic!("expected implied-in for the residual, got {events:?}");
    };
    assert_eq!(fill.spread_price, 0);
    assert_eq!(fill.quantity, 3);
    assert_eq!(fill.leg1_price_ticks, 50_000);
    assert_eq!(fill.leg2_price_ticks, 50_000);

    let book = registry.combo_book(10).unwrap();
    assert!(book.is_empty());
    assert_eq!(book.last_trade_price(), Some(0));
}

#[test]
fn negative_buy_spread_fills_within_a_negative_limit() {
    let mut registry = registry_with_pair();
    rest_leg(&mut registry, leg_order(1, 1, Side::Sell, 40_000, 2));
    rest_leg(&mut registry, leg_order(2, 2, Side::Buy, 41_000, 2));

    let events = registry
        .execute_combo(combo_order(1009, 3, 33, Side::Buy, -1_000, 2))
        .unwrap();

    let fill = only_implied_fill(&events);
    assert_eq!(fill.spread_price, -1_000);
    assert_eq!(fill.quantity, 2);
    assert_eq!(fill.leg1_price_ticks, 40_000);
    assert_eq!(fill.leg2_price_ticks, 41_000);
    assert_eq!(
        registry.combo_book(10).unwrap().last_trade_price(),
        Some(-1_000)
    );
}
