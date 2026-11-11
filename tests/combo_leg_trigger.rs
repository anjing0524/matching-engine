//! Resting Combo Order re-evaluation when ordinary leg liquidity arrives later.
//!
//! M1 processes one event as a combined match: ordinary leg matching first,
//! then the resting Combo Orders of every pair that names the affected leg.
//! A resting Combo Order acts as a taker against leg liquidity that appears
//! after it rested, so it must fill atomically without duplicating a fill or
//! re-entering matching.

use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::trading::core::positions::PositionKey;
use trading_system_engine::trading::core::private_events::PrivateStatus;
use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::core::{
    ComboEvent, ComboPair, ComboPairMargin, CoreEvent, InstrumentRegistry,
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
    side: Side,
    spread_price: i64,
    quantity: u64,
) -> Command {
    let mut command = Command::zeroed();
    command.command_type = CommandType::NewOrder;
    command.combo_id = 10;
    command.client_order_id = client_order_id;
    command.user_id = user_id;
    command.session_id = u64::from(user_id);
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
    client_order_id: u64,
    instrument_id: u32,
    user_id: u32,
    side: Side,
    price_ticks: i64,
    quantity: u64,
) -> Command {
    let mut command = Command::zeroed();
    command.command_type = CommandType::NewOrder;
    command.client_order_id = client_order_id;
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

fn rested_combo_order(events: &[ComboEvent]) -> u64 {
    match events {
        [ComboEvent::Rested { order }] => order.order_id,
        other => panic!("expected one rested combo order, got {other:?}"),
    }
}

#[test]
fn leg_liquidity_fills_a_resting_combo_once_per_round_without_duplicates() {
    let mut registry = registry_with_pair();
    let combo_id = rested_combo_order(
        &registry
            .execute_combo(combo_order(200, 3, Side::Buy, 10_000, 5))
            .unwrap(),
    );
    rest_leg(&mut registry, leg_order(3, 1, 1, Side::Sell, 50_000, 3));
    // The second leg side arrives with only two lots, so only two spread lots
    // can fill and the resting combo keeps a residual.
    rest_leg(&mut registry, leg_order(4, 2, 2, Side::Buy, 40_000, 2));

    let book = registry.combo_book(10).unwrap();
    assert_eq!(book.len(), 1);
    assert_eq!(
        book.order(combo_id)
            .expect("resting combo residual")
            .remaining,
        3
    );
    assert_eq!(book.last_trade_price(), Some(10_000));
    assert_eq!(registry.core(1).unwrap().best_ask(), Some(50_000));

    // The first leg ask has one lot left after the first implied fill.
    let first_fills = registry.drain_combo_private_events();
    assert_eq!(first_fills.len(), 1);
    assert_eq!(first_fills[0].quantity, 2);

    // A later second-leg bid triggers another round against the residual leg
    // ask, filling one more spread lot and leaving a combo residual.
    rest_leg(&mut registry, leg_order(5, 2, 2, Side::Buy, 40_000, 3));
    let second_fills = registry.drain_combo_private_events();
    assert_eq!(second_fills.len(), 1);
    assert_eq!(second_fills[0].quantity, 1);
    assert_eq!(
        registry
            .combo_book(10)
            .unwrap()
            .order(combo_id)
            .expect("combo residual still resting")
            .remaining,
        2
    );
    assert_eq!(registry.core(1).unwrap().best_ask(), None);
    assert_eq!(registry.core(2).unwrap().best_bid(), Some(40_000));

    // No leg ask remains, so an extra leg bid must not produce a fill or
    // duplicate the earlier executions.
    rest_leg(&mut registry, leg_order(6, 2, 2, Side::Buy, 40_000, 5));
    assert!(
        registry.drain_combo_private_events().is_empty(),
        "no implied fill may be produced without both leg sides"
    );
    assert_eq!(
        registry
            .combo_book(10)
            .unwrap()
            .order(combo_id)
            .expect("combo residual still resting")
            .remaining,
        2
    );
    assert_eq!(registry.core(2).unwrap().best_bid(), Some(40_000));
}

#[test]
fn base_leg_order_takes_priority_over_implied_at_the_same_price() {
    let mut registry = registry_with_pair();
    // A resting buy spread needs a first-leg ask. It rests first because no
    // first-leg ask exists yet.
    let combo_id = rested_combo_order(
        &registry
            .execute_combo(combo_order(300, 3, Side::Buy, 10_000, 5))
            .unwrap(),
    );
    // The second-leg bid alone cannot complete the implied combo.
    rest_leg(&mut registry, leg_order(7, 2, 2, Side::Buy, 40_000, 5));
    // A base first-leg bid at the same price the incoming seller will use.
    let resting_bid = rest_leg(&mut registry, leg_order(8, 1, 1, Side::Buy, 50_000, 2));

    // The incoming base leg seller crosses the resting base bid first. Only
    // its residual rests as an ask and can be consumed by the implied combo.
    registry
        .execute(leg_order(9, 1, 4, Side::Sell, 50_000, 5))
        .unwrap();

    let leg1_private = registry.core_mut(1).unwrap().drain_private_events();
    let base_fill = leg1_private
        .iter()
        .find(|event| event.order_id == resting_bid && event.status == PrivateStatus::Filled)
        .expect("the base leg bid must fill before any implied execution");
    assert_eq!(base_fill.account_id, 1);
    assert_eq!(base_fill.filled_delta, 2);

    let combo_fills = registry.drain_combo_private_events();
    assert_eq!(combo_fills.len(), 1, "one implied fill: {combo_fills:?}");
    assert_eq!(combo_fills[0].quantity, 3);
    assert_eq!(combo_fills[0].spread_price, 10_000);
    assert_eq!(combo_fills[0].leg1_price_ticks, 50_000);
    assert_eq!(combo_fills[0].leg2_price_ticks, 40_000);

    // The combo residual stays resting, and the second-leg bid keeps what the
    // implied fill did not consume.
    let book = registry.combo_book(10).unwrap();
    assert_eq!(book.order(combo_id).expect("combo residual").remaining, 2);
    assert_eq!(book.last_trade_price(), Some(10_000));
    assert_eq!(registry.core(1).unwrap().best_bid(), None);
    assert_eq!(registry.core(1).unwrap().best_ask(), None);
    assert_eq!(registry.core(2).unwrap().best_bid(), Some(40_000));
}

#[test]
fn resting_combo_fills_when_both_leg_orders_arrive_later() {
    let mut registry = registry_with_pair();
    let rested = registry
        .execute_combo(combo_order(100, 3, Side::Buy, 10_000, 5))
        .unwrap();
    rested_combo_order(&rested);
    assert_eq!(registry.combo_book(10).unwrap().best_bid(), Some(10_000));

    // The first leg order alone is not enough to form an implied combo.
    let leg1_order = rest_leg(&mut registry, leg_order(1, 1, 1, Side::Sell, 50_000, 5));
    assert_eq!(
        registry.combo_book(10).unwrap().best_bid(),
        Some(10_000),
        "the combo must keep resting until both leg sides exist"
    );
    assert!(registry.combo_private_events().is_empty());

    // The second leg order completes the implied combo and must trigger it.
    let leg2_order = rest_leg(&mut registry, leg_order(2, 2, 2, Side::Buy, 40_000, 5));

    assert!(
        registry.combo_book(10).unwrap().is_empty(),
        "the resting combo must fill and leave the book"
    );
    assert_eq!(
        registry.combo_book(10).unwrap().last_trade_price(),
        Some(10_000)
    );
    for instrument_id in [1, 2] {
        assert_eq!(registry.core(instrument_id).unwrap().best_bid(), None);
        assert_eq!(registry.core(instrument_id).unwrap().best_ask(), None);
    }

    let fills = registry.drain_combo_private_events();
    assert_eq!(fills.len(), 1, "one combo fill, not a duplicate: {fills:?}");
    let fill = fills[0];
    assert_eq!(fill.pair_id, 10);
    assert_eq!(fill.user_id, 3);
    assert_eq!(fill.side, Side::Buy);
    assert_eq!(fill.quantity, 5);
    assert_eq!(fill.spread_price, 10_000);
    assert_eq!(fill.leg1_price_ticks, 50_000);
    assert_eq!(fill.leg2_price_ticks, 40_000);

    // The leg owners receive ordinary leg fills for the quantity the combo took.
    let leg1_private = registry.core_mut(1).unwrap().drain_private_events();
    let leg1_fill = leg1_private
        .iter()
        .find(|event| event.order_id == leg1_order && event.status == PrivateStatus::Filled)
        .expect("leg1 owner fill");
    assert_eq!(leg1_fill.account_id, 1);
    assert_eq!(leg1_fill.filled_delta, 5);
    let leg2_private = registry.core_mut(2).unwrap().drain_private_events();
    let leg2_fill = leg2_private
        .iter()
        .find(|event| event.order_id == leg2_order && event.status == PrivateStatus::Filled)
        .expect("leg2 owner fill");
    assert_eq!(leg2_fill.account_id, 2);
    assert_eq!(leg2_fill.filled_delta, 5);

    // The combo owner carries a leg position on both Contracts, not on the pair.
    let leg1 = registry.core(1).unwrap();
    assert_eq!(
        leg1.positions()
            .get(PositionKey::new(3, Side::Buy, Hedge::Spec))
            .today,
        5
    );
    let leg2 = registry.core(2).unwrap();
    assert_eq!(
        leg2.positions()
            .get(PositionKey::new(3, Side::Sell, Hedge::Spec))
            .today,
        5
    );
}

#[test]
fn sell_spread_resting_combo_fills_when_leg_liquidity_arrives() {
    let mut registry = registry_with_pair();
    registry
        .execute_combo(combo_order(400, 3, Side::Sell, -1_000, 2))
        .unwrap();
    // A first-leg bid alone cannot complete a sell spread; it also becomes the
    // bid side the resting sell combo will consume.
    rest_leg(&mut registry, leg_order(10, 1, 1, Side::Buy, 40_000, 2));
    assert!(registry.drain_combo_private_events().is_empty());
    // The second-leg ask completes the implied sell spread.
    rest_leg(&mut registry, leg_order(11, 2, 2, Side::Sell, 41_000, 2));

    let fills = registry.drain_combo_private_events();
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].side, Side::Sell);
    assert_eq!(fills[0].quantity, 2);
    assert_eq!(fills[0].spread_price, -1_000);
    assert_eq!(fills[0].leg1_price_ticks, 40_000);
    assert_eq!(fills[0].leg2_price_ticks, 41_000);
    assert!(registry.combo_book(10).unwrap().is_empty());
    assert_eq!(
        registry.combo_book(10).unwrap().last_trade_price(),
        Some(-1_000)
    );
    assert_eq!(registry.core(1).unwrap().best_bid(), None);
    assert_eq!(registry.core(2).unwrap().best_ask(), None);
}

#[test]
fn resting_combos_fill_in_price_time_priority_without_duplicates() {
    let mut registry = registry_with_pair();
    // The higher bid has price priority; the second order keeps time priority
    // at the same spread.
    registry
        .execute_combo(combo_order(500, 3, Side::Buy, 10_000, 2))
        .unwrap();
    registry
        .execute_combo(combo_order(501, 4, Side::Buy, 9_000, 2))
        .unwrap();
    registry
        .execute_combo(combo_order(502, 5, Side::Buy, 10_000, 2))
        .unwrap();
    rest_leg(&mut registry, leg_order(12, 2, 2, Side::Buy, 40_000, 3));
    rest_leg(&mut registry, leg_order(13, 1, 1, Side::Sell, 50_000, 3));

    let fills = registry.drain_combo_private_events();
    let summary: Vec<(u64, u64, i64)> = fills
        .iter()
        .map(|fill| (fill.order_id, fill.quantity, fill.spread_price))
        .collect();
    assert_eq!(
        summary,
        vec![(1, 2, 10_000), (3, 1, 10_000)],
        "price-time priority, each order consumed once"
    );
    let book = registry.combo_book(10).unwrap();
    assert_eq!(
        book.order(2).expect("the 9_000 bid still rests").remaining,
        2
    );
    assert_eq!(
        book.order(3).expect("the 10_000 residual rests").remaining,
        1
    );
    assert_eq!(book.best_bid(), Some(10_000));
}
