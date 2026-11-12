//! Direct atomic Combo Matching behavior through the [`InstrumentRegistry`] seam.

use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::core::{
    ComboCommandError, ComboEvent, ComboFill, ComboPair, ComboPairMargin, InstrumentRegistry,
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

fn rested_order_id(events: &[ComboEvent]) -> u64 {
    match events {
        [ComboEvent::Rested { order }] => order.order_id,
        other => panic!("expected one rested order, got {other:?}"),
    }
}

fn first_fill(events: &[ComboEvent]) -> ComboFill {
    match events {
        [ComboEvent::Filled { fill }] => *fill,
        other => panic!("expected exactly one combo fill, got {other:?}"),
    }
}

#[test]
fn crossing_orders_fill_atomically_and_leave_the_book_empty() {
    let mut registry = registry_with_pair();
    let resting = registry
        .execute_combo(combo_order(101, 1, 11, Side::Sell, 100, 5))
        .unwrap();
    let resting_order_id = rested_order_id(&resting);

    let events = registry
        .execute_combo(combo_order(102, 2, 22, Side::Buy, 111, 5))
        .unwrap();

    assert_eq!(
        events.len(),
        1,
        "fully filled taker must not rest: {events:?}"
    );
    let fill = match &events[0] {
        ComboEvent::Filled { fill } => fill,
        other => panic!("expected one combo fill, got {other:?}"),
    };
    assert_eq!(fill.pair_id, 10);
    assert_eq!(fill.spread_price, 105);
    assert_eq!(fill.quantity, 5);
    assert_eq!(fill.buy_order_id, 2);
    assert_eq!(fill.sell_order_id, resting_order_id);
    assert_eq!(fill.buy_owner.user_id, 2);
    assert_eq!(fill.sell_owner.user_id, 1);

    let book = registry.combo_book(10).unwrap();
    assert!(book.is_empty());
    assert_eq!(book.last_trade_price(), Some(105));
}

#[test]
fn partial_fill_leaves_the_residual_resting_for_later_execution() {
    let mut registry = registry_with_pair();
    let resting = registry
        .execute_combo(combo_order(201, 1, 11, Side::Sell, 100, 3))
        .unwrap();
    let resting_order_id = rested_order_id(&resting);

    let events = registry
        .execute_combo(combo_order(202, 2, 22, Side::Buy, 111, 8))
        .unwrap();

    assert_eq!(
        events.len(),
        2,
        "expected a fill and a residual rest: {events:?}"
    );
    let fill = match &events[0] {
        ComboEvent::Filled { fill } => *fill,
        other => panic!("expected a combo fill, got {other:?}"),
    };
    assert_eq!(fill.spread_price, 105);
    assert_eq!(fill.quantity, 3);
    assert_eq!(fill.sell_order_id, resting_order_id);
    let residual = match &events[1] {
        ComboEvent::Rested { order } => *order,
        other => panic!("expected the residual to rest, got {other:?}"),
    };
    assert_eq!(residual.order_id, 2);
    assert_eq!(residual.quantity, 8);
    assert_eq!(residual.remaining, 5);
    assert_eq!(registry.combo_book(10).unwrap().best_bid(), Some(111));
    assert_eq!(registry.combo_book(10).unwrap().best_ask(), None);

    let events = registry
        .execute_combo(combo_order(203, 3, 33, Side::Sell, 100, 2))
        .unwrap();
    let fill = match &events[..] {
        [ComboEvent::Filled { fill }] => *fill,
        other => panic!("expected one combo fill, got {other:?}"),
    };
    assert_eq!(fill.buy_order_id, residual.order_id);
    assert_eq!(fill.spread_price, 105);
    assert_eq!(fill.quantity, 2);

    let book = registry.combo_book(10).unwrap();
    assert_eq!(book.best_bid(), Some(111));
    assert_eq!(book.order(residual.order_id).unwrap().remaining, 3);
    assert_eq!(book.len(), 1);
}

#[test]
fn non_overlapping_prices_do_not_cross_or_mutate_resting_orders() {
    let mut registry = registry_with_pair();
    let bid = registry
        .execute_combo(combo_order(301, 1, 11, Side::Buy, 100, 4))
        .unwrap();
    let bid_order_id = rested_order_id(&bid);

    let events = registry
        .execute_combo(combo_order(302, 2, 22, Side::Sell, 105, 6))
        .unwrap();

    assert_eq!(events.len(), 1, "no execution may occur: {events:?}");
    assert!(matches!(events[0], ComboEvent::Rested { .. }));

    let book = registry.combo_book(10).unwrap();
    assert_eq!(book.best_bid(), Some(100));
    assert_eq!(book.best_ask(), Some(105));
    assert_eq!(book.order(bid_order_id).unwrap().remaining, 4);
    assert_eq!(book.best_order(Side::Sell).unwrap().remaining, 6);
    assert_eq!(book.last_trade_price(), None);
}

#[test]
fn taker_walks_opposite_orders_by_price_then_time_priority() {
    let mut registry = registry_with_pair();
    for (client_order_id, price, quantity) in [(401, 100, 2), (402, 99, 4), (403, 100, 3)] {
        registry
            .execute_combo(combo_order(
                client_order_id,
                1,
                11,
                Side::Sell,
                price,
                quantity,
            ))
            .unwrap();
    }

    let events = registry
        .execute_combo(combo_order(404, 2, 22, Side::Buy, 105, 8))
        .unwrap();

    let fills: Vec<(u64, i64, u64)> = events
        .iter()
        .filter_map(|event| match event {
            ComboEvent::Filled { fill } => {
                Some((fill.sell_order_id, fill.spread_price, fill.quantity))
            }
            _ => None,
        })
        .collect();
    assert_eq!(fills, vec![(2, 102, 4), (1, 102, 2), (3, 102, 2)]);

    let book = registry.combo_book(10).unwrap();
    assert_eq!(book.best_ask(), Some(100));
    assert_eq!(book.best_order(Side::Sell).unwrap().order_id, 3);
    assert_eq!(book.order(3).unwrap().remaining, 1);
    assert_eq!(book.last_trade_price(), Some(102));
}

#[test]
fn execution_price_is_the_median_of_buy_sell_limits_and_last_trade() {
    let mut registry = registry_with_pair();

    registry
        .execute_combo(combo_order(501, 1, 11, Side::Sell, 100, 1))
        .unwrap();
    let liquidity = registry
        .execute_combo(combo_order(502, 2, 22, Side::Buy, 116, 1))
        .unwrap();
    assert_eq!(first_fill(&liquidity).spread_price, 108);
    assert_eq!(
        registry.combo_book(10).unwrap().last_trade_price(),
        Some(108)
    );

    registry
        .execute_combo(combo_order(503, 1, 11, Side::Sell, 100, 1))
        .unwrap();
    let last_inside_limits = registry
        .execute_combo(combo_order(504, 2, 22, Side::Buy, 111, 1))
        .unwrap();
    assert_eq!(first_fill(&last_inside_limits).spread_price, 108);

    registry
        .execute_combo(combo_order(505, 2, 22, Side::Buy, 130, 1))
        .unwrap();
    let last_below_sell_limit = registry
        .execute_combo(combo_order(506, 1, 11, Side::Sell, 112, 1))
        .unwrap();
    assert_eq!(first_fill(&last_below_sell_limit).spread_price, 112);

    registry
        .execute_combo(combo_order(507, 1, 11, Side::Sell, 100, 1))
        .unwrap();
    let last_above_buy_limit = registry
        .execute_combo(combo_order(508, 2, 22, Side::Buy, 105, 1))
        .unwrap();
    assert_eq!(first_fill(&last_above_buy_limit).spread_price, 105);
}

#[test]
fn first_trade_midpoint_rounds_toward_the_resting_order() {
    let mut registry = registry_with_pair();
    registry
        .execute_combo(combo_order(601, 1, 11, Side::Sell, 100, 1))
        .unwrap();
    let buy_taker = registry
        .execute_combo(combo_order(602, 2, 22, Side::Buy, 111, 1))
        .unwrap();
    assert_eq!(first_fill(&buy_taker).spread_price, 105);

    let mut registry = registry_with_pair();
    registry
        .execute_combo(combo_order(603, 2, 22, Side::Buy, 111, 1))
        .unwrap();
    let sell_taker = registry
        .execute_combo(combo_order(604, 1, 11, Side::Sell, 100, 1))
        .unwrap();
    assert_eq!(first_fill(&sell_taker).spread_price, 106);
}

#[test]
fn last_trade_price_is_updated_after_each_fill() {
    let mut registry = registry_with_pair();
    assert_eq!(registry.combo_book(10).unwrap().last_trade_price(), None);

    registry
        .execute_combo(combo_order(701, 1, 11, Side::Sell, 100, 1))
        .unwrap();
    registry
        .execute_combo(combo_order(702, 2, 22, Side::Buy, 112, 1))
        .unwrap();
    assert_eq!(
        registry.combo_book(10).unwrap().last_trade_price(),
        Some(106)
    );

    registry
        .execute_combo(combo_order(703, 2, 22, Side::Buy, 130, 1))
        .unwrap();
    registry
        .execute_combo(combo_order(704, 1, 11, Side::Sell, 118, 1))
        .unwrap();
    assert_eq!(
        registry.combo_book(10).unwrap().last_trade_price(),
        Some(118)
    );
}

#[test]
fn failed_plans_leave_both_resting_combo_orders_unchanged() {
    let mut registry = registry_with_pair();
    let rested = registry
        .execute_combo(combo_order(801, 1, 11, Side::Sell, 100, 5))
        .unwrap();
    let resting_order_id = rested_order_id(&rested);

    let mut market = combo_order(802, 2, 22, Side::Buy, 110, 5);
    market.price_type = PriceType::Market;
    assert_eq!(
        registry.execute_combo(market).unwrap_err(),
        ComboCommandError::UnsupportedPriceType {
            price_type: PriceType::Market,
        }
    );

    let zero_quantity = combo_order(803, 2, 22, Side::Buy, 110, 0);
    assert_eq!(
        registry.execute_combo(zero_quantity).unwrap_err(),
        ComboCommandError::ZeroQuantity { pair_id: 10 }
    );

    let book = registry.combo_book(10).unwrap();
    assert_eq!(book.len(), 1);
    assert_eq!(book.best_ask(), Some(100));
    assert_eq!(book.order(resting_order_id).unwrap().remaining, 5);
    assert_eq!(book.last_trade_price(), None);
}

#[test]
fn sell_taker_walks_bids_by_price_then_time_priority() {
    let mut registry = registry_with_pair();
    for (client_order_id, price, quantity) in [(901, 100, 2), (902, 101, 3), (903, 101, 1)] {
        registry
            .execute_combo(combo_order(
                client_order_id,
                1,
                11,
                Side::Buy,
                price,
                quantity,
            ))
            .unwrap();
    }

    let events = registry
        .execute_combo(combo_order(904, 2, 22, Side::Sell, 99, 5))
        .unwrap();

    let fills: Vec<(u64, i64, u64)> = events
        .iter()
        .filter_map(|event| match event {
            ComboEvent::Filled { fill } => {
                Some((fill.buy_order_id, fill.spread_price, fill.quantity))
            }
            _ => None,
        })
        .collect();
    assert_eq!(fills, vec![(2, 100, 3), (3, 100, 1), (1, 100, 1)]);

    let book = registry.combo_book(10).unwrap();
    assert_eq!(book.best_bid(), Some(100));
    assert_eq!(book.best_order(Side::Buy).unwrap().order_id, 1);
    assert_eq!(book.order(1).unwrap().remaining, 1);
    assert_eq!(book.last_trade_price(), Some(100));
}

#[test]
fn direct_combo_fill_leaves_leg_books_untouched_and_positions_combo_owners() {
    use trading_system_engine::trading::core::positions::PositionKey;
    use trading_system_engine::trading::core::protocol::Hedge;

    let mut registry = registry_with_pair();
    registry
        .execute_combo(combo_order(1001, 1, 11, Side::Sell, 100, 5))
        .unwrap();
    registry
        .execute_combo(combo_order(1002, 2, 22, Side::Buy, 111, 5))
        .unwrap();

    for instrument_id in [1, 2] {
        let core = registry.core(instrument_id).unwrap();
        assert_eq!(core.best_bid(), None, "leg {instrument_id} bid book moved");
        assert_eq!(core.best_ask(), None, "leg {instrument_id} ask book moved");
    }

    // A direct Combo Fill produces no leg trade, but both combo owners hold
    // the resulting position on each leg Contract exactly once.
    let leg1 = registry.core(1).unwrap();
    let leg2 = registry.core(2).unwrap();
    assert_eq!(
        leg1.positions()
            .get(PositionKey::new(2, Side::Buy, Hedge::Spec))
            .today,
        5
    );
    assert_eq!(
        leg2.positions()
            .get(PositionKey::new(2, Side::Sell, Hedge::Spec))
            .today,
        5
    );
    assert_eq!(
        leg1.positions()
            .get(PositionKey::new(1, Side::Sell, Hedge::Spec))
            .today,
        5
    );
    assert_eq!(
        leg2.positions()
            .get(PositionKey::new(1, Side::Buy, Hedge::Spec))
            .today,
        5
    );
}
