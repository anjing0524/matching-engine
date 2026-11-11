//! Public combo market data.
//!
//! Leg trades keep updating their ordinary Contract market data. Every combo
//! trade also publishes one combo price-statistics event carrying the pair id,
//! realized spread, quantity, and both leg reference prices. Implied orders are
//! transient matching candidates: they never rest in the public combo book and
//! never receive a public order event.

use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::core::public_events::{PublicEvent, PublicEventType};
use trading_system_engine::trading::core::{
    ComboPair, ComboPairMargin, CoreEvent, InstrumentRegistry,
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
    registry.set_prior_settlement(1, 45_000).unwrap();
    registry.set_prior_settlement(2, 40_000).unwrap();
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

fn combo_trades(events: &[PublicEvent]) -> Vec<&PublicEvent> {
    events
        .iter()
        .filter(|event| event.event_type == PublicEventType::ComboTrade)
        .collect()
}

#[test]
fn implied_combo_trade_publishes_combo_price_statistics_and_leg_trades() {
    let mut registry = registry_with_pair();
    registry
        .execute_combo(combo_order(100, 3, Side::Buy, 10_000, 5))
        .unwrap();
    rest_leg(&mut registry, leg_order(1, 1, 1, Side::Sell, 50_000, 5));
    // Drop the setup events so only the trigger's public data is inspected.
    let _ = registry.drain_public_events();

    rest_leg(&mut registry, leg_order(2, 2, 2, Side::Buy, 40_000, 5));
    let public = registry.drain_public_events();

    let combo = combo_trades(&public);
    assert_eq!(combo.len(), 1, "one combo price statistic: {public:?}");
    let event = combo[0];
    assert_eq!(event.pair_id, 10);
    assert_eq!(event.spread_price, 10_000);
    assert_eq!(event.quantity, 5);
    assert_eq!(event.leg1_price_ticks, 50_000);
    assert_eq!(event.leg2_price_ticks, 40_000);
    assert_eq!(event.side, Some(Side::Buy));

    // Both leg owners' trades remain ordinary public leg trades.
    let leg_trades: Vec<&PublicEvent> = public
        .iter()
        .filter(|event| event.event_type == PublicEventType::Trade)
        .collect();
    assert_eq!(leg_trades.len(), 2, "one leg trade per leg: {public:?}");
    assert!(leg_trades.iter().all(|event| event.pair_id == 0));
    assert!(leg_trades.iter().any(|event| event.instrument_id == 1));
    assert!(leg_trades.iter().any(|event| event.instrument_id == 2));

    // Implied orders have no public order identity and never rest in the
    // public book: the only pair-scoped public event is the combo trade.
    assert!(
        public
            .iter()
            .all(|event| event.pair_id == 0 || event.event_type == PublicEventType::ComboTrade),
        "no implied order may be published: {public:?}"
    );
    let book = registry.combo_book(10).unwrap();
    assert!(book.is_empty());
}

#[test]
fn direct_combo_trade_publishes_combo_price_statistics_with_reference_leg_prices() {
    let mut registry = registry_with_pair();
    registry
        .execute_combo(combo_order(101, 1, Side::Sell, 100, 2))
        .unwrap();
    // Reference quotes on the sides a buy-spread taker would cross. They do
    // not form the leg1 bid and leg2 ask a sell spread needs, so the resting
    // combo cannot fill by implication before the direct taker arrives.
    rest_leg(&mut registry, leg_order(3, 1, 2, Side::Sell, 49_000, 2));
    rest_leg(&mut registry, leg_order(4, 2, 3, Side::Buy, 41_000, 2));
    let _ = registry.drain_public_events();

    registry
        .execute_combo(combo_order(102, 4, Side::Buy, 100, 2))
        .unwrap();
    let public = registry.drain_public_events();

    let combo = combo_trades(&public);
    assert_eq!(combo.len(), 1, "one combo price statistic: {public:?}");
    let event = combo[0];
    assert_eq!(event.event_type, PublicEventType::ComboTrade);
    assert_eq!(event.pair_id, 10);
    assert_eq!(event.spread_price, 100);
    assert_eq!(event.quantity, 2);
    // A direct fill consumes no leg liquidity, so it reports the best crossing
    // reference on each leg.
    assert_eq!(event.leg1_price_ticks, 49_000);
    assert_eq!(event.leg2_price_ticks, 41_000);
    assert_eq!(event.side, None);
    assert!(
        public
            .iter()
            .all(|event| event.pair_id == 0 || event.event_type == PublicEventType::ComboTrade),
        "no implied order may be published: {public:?}"
    );
}
