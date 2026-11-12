//! Combo fills, combo-owner positions, and leg-level margin through the
//! [`InstrumentRegistry`] seam.

use trading_system_engine::domain::orderbook::ContractSpec;
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
    registry.set_prior_settlement(2, 40_000).unwrap();
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

fn cancel_combo(user_id: u32, order_id: u64) -> Command {
    let mut command = Command::zeroed();
    command.command_type = CommandType::Cancel;
    command.combo_id = 10;
    command.cancel_order_id = order_id;
    command.user_id = user_id;
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

fn rested_order_id(events: &[ComboEvent]) -> u64 {
    match events {
        [ComboEvent::Rested { order }] => order.order_id,
        other => panic!("expected one rested combo order, got {other:?}"),
    }
}

#[test]
fn direct_fill_produces_one_private_fill_per_combo_owner() {
    let mut registry = registry_with_pair();
    // Reference leg quotes for a buy-spread taker: leg1 ask and leg2 bid.
    rest_leg(&mut registry, leg_order(1, 100, Side::Sell, 50_000, 10));
    rest_leg(&mut registry, leg_order(2, 101, Side::Buy, 40_000, 10));

    let rested = registry
        .execute_combo(combo_order(101, 1, 11, Side::Sell, 100, 5))
        .unwrap();
    let resting_order_id = rested_order_id(&rested);

    let events = registry
        .execute_combo(combo_order(102, 2, 22, Side::Buy, 100, 5))
        .unwrap();
    assert!(matches!(events.as_slice(), [ComboEvent::Filled { .. }]));

    let fills = registry.drain_combo_private_events();
    assert_eq!(
        fills.len(),
        2,
        "one private fill per combo owner: {fills:?}"
    );
    let buy = fills[0];
    assert_eq!(buy.seq, 1);
    assert_eq!(buy.pair_id, 10);
    assert_eq!(buy.order_id, 2);
    assert_eq!(buy.client_order_id, 102);
    assert_eq!(buy.user_id, 2);
    assert_eq!(buy.session_id, 22);
    assert_eq!(buy.side, Side::Buy);
    assert_eq!(buy.quantity, 5);
    assert_eq!(buy.spread_price, 100);
    assert_eq!(buy.leg1_price_ticks, 50_000);
    assert_eq!(buy.leg2_price_ticks, 40_000);

    let sell = fills[1];
    assert_eq!(sell.seq, 2);
    assert_eq!(sell.order_id, resting_order_id);
    assert_eq!(sell.client_order_id, 101);
    assert_eq!(sell.user_id, 1);
    assert_eq!(sell.session_id, 11);
    assert_eq!(sell.side, Side::Sell);
    assert_eq!(sell.quantity, 5);
    assert_eq!(sell.spread_price, 100);
    assert_eq!(sell.leg1_price_ticks, 50_000);
    assert_eq!(sell.leg2_price_ticks, 40_000);

    assert!(registry.drain_combo_private_events().is_empty());
}

#[test]
fn direct_fill_applies_combo_owner_positions_on_both_legs_exactly_once() {
    use trading_system_engine::trading::core::positions::PositionKey;

    let mut registry = registry_with_pair();
    rest_leg(&mut registry, leg_order(1, 100, Side::Sell, 50_000, 10));
    rest_leg(&mut registry, leg_order(2, 101, Side::Buy, 40_000, 10));
    registry
        .execute_combo(combo_order(101, 1, 11, Side::Sell, 100, 5))
        .unwrap();
    registry
        .execute_combo(combo_order(102, 2, 22, Side::Buy, 100, 5))
        .unwrap();

    let leg1 = registry.core(1).unwrap();
    let leg2 = registry.core(2).unwrap();

    // The buy-spread owner is long leg1 and short leg2.
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
    // The sell-spread owner is short leg1 and long leg2.
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

    // Combo owners receive no ordinary leg fill and no leg owner position.
    for core in [leg1, leg2] {
        assert!(
            core.private_events()
                .iter()
                .all(|event| event.account_id != 1 && event.account_id != 2),
            "combo owners must not receive ordinary leg fills"
        );
    }
    assert!(
        registry
            .core(1)
            .unwrap()
            .positions()
            .snapshot()
            .iter()
            .all(|(key, _)| key.account_id != 100)
    );
    assert!(
        registry
            .core(2)
            .unwrap()
            .positions()
            .snapshot()
            .iter()
            .all(|(key, _)| key.account_id != 101)
    );
}

#[test]
fn implied_fill_produces_private_fill_for_taker_and_leg_fills_for_owners() {
    use trading_system_engine::trading::core::positions::PositionKey;
    use trading_system_engine::trading::core::private_events::PrivateStatus;

    let mut registry = registry_with_pair();
    let leg1_order = rest_leg(&mut registry, leg_order(1, 1, Side::Sell, 50_000, 5));
    let leg2_order = rest_leg(&mut registry, leg_order(2, 2, Side::Buy, 40_000, 5));
    for instrument_id in [1, 2] {
        let core = registry.core_mut(instrument_id).unwrap();
        core.drain_private_events();
        core.drain_public_events();
    }
    let _ = registry.drain_combo_private_events();

    let events = registry
        .execute_combo(combo_order(1001, 3, 33, Side::Buy, 10_000, 5))
        .unwrap();
    assert!(matches!(
        events.as_slice(),
        [ComboEvent::ImpliedFilled { .. }]
    ));

    let fills = registry.drain_combo_private_events();
    assert_eq!(fills.len(), 1, "one private fill for the taker: {fills:?}");
    let fill = fills[0];
    assert_eq!(fill.seq, 1);
    assert_eq!(fill.pair_id, 10);
    assert_eq!(fill.order_id, 1);
    assert_eq!(fill.client_order_id, 1001);
    assert_eq!(fill.user_id, 3);
    assert_eq!(fill.session_id, 33);
    assert_eq!(fill.side, Side::Buy);
    assert_eq!(fill.quantity, 5);
    assert_eq!(fill.spread_price, 10_000);
    assert_eq!(fill.leg1_price_ticks, 50_000);
    assert_eq!(fill.leg2_price_ticks, 40_000);

    // The combo owner carries positions on both leg Contracts.
    assert_eq!(
        registry
            .core(1)
            .unwrap()
            .positions()
            .get(PositionKey::new(3, Side::Buy, Hedge::Spec))
            .today,
        5
    );
    assert_eq!(
        registry
            .core(2)
            .unwrap()
            .positions()
            .get(PositionKey::new(3, Side::Sell, Hedge::Spec))
            .today,
        5
    );

    // The resting leg owners still receive the #40 ordinary leg fills.
    let leg1_private = registry.core_mut(1).unwrap().drain_private_events();
    assert_eq!(leg1_private.len(), 1);
    assert_eq!(leg1_private[0].order_id, leg1_order);
    assert_eq!(leg1_private[0].account_id, 1);
    assert_eq!(leg1_private[0].filled_delta, 5);
    assert_eq!(leg1_private[0].status, PrivateStatus::Filled);
    let leg2_private = registry.core_mut(2).unwrap().drain_private_events();
    assert_eq!(leg2_private.len(), 1);
    assert_eq!(leg2_private[0].order_id, leg2_order);
    assert_eq!(leg2_private[0].account_id, 2);
    assert_eq!(leg2_private[0].filled_delta, 5);
    assert_eq!(leg2_private[0].status, PrivateStatus::Filled);

    // The combo taker must not receive an ordinary leg fill.
    for instrument_id in [1, 2] {
        let core = registry.core_mut(instrument_id).unwrap();
        assert!(core.private_events().is_empty());
        assert_eq!(core.drain_public_events().len(), 1);
    }
}

#[test]
fn private_sequence_stays_ordered_across_direct_and_implied_effects() {
    let mut registry = registry_with_pair();
    // Direct liquidity: a resting sell combo for two spreads.
    registry
        .execute_combo(combo_order(1101, 1, 11, Side::Sell, 100, 2))
        .unwrap();
    // Implied-in liquidity: both legs cross the residual at spread 10_000.
    rest_leg(&mut registry, leg_order(1, 2, Side::Sell, 50_000, 3));
    rest_leg(&mut registry, leg_order(2, 3, Side::Buy, 40_000, 3));
    for instrument_id in [1, 2] {
        let core = registry.core_mut(instrument_id).unwrap();
        core.drain_private_events();
        core.drain_public_events();
    }
    let _ = registry.drain_combo_private_events();

    registry
        .execute_combo(combo_order(1102, 9, 99, Side::Buy, 10_000, 5))
        .unwrap();

    let fills = registry.drain_combo_private_events();
    let sequence: Vec<(u64, u64, u64)> = fills
        .iter()
        .map(|fill| (fill.seq, fill.user_id, fill.quantity))
        .collect();
    assert_eq!(sequence, vec![(1, 9, 2), (2, 1, 2), (3, 9, 3)]);
}

#[test]
fn combo_pair_has_no_position_of_its_own() {
    let mut registry = registry_with_pair();
    registry
        .execute_combo(combo_order(101, 1, 11, Side::Sell, 100, 5))
        .unwrap();
    registry
        .execute_combo(combo_order(102, 2, 22, Side::Buy, 100, 5))
        .unwrap();

    // Positions exist only on the two leg Contracts, keyed by account.
    for instrument_id in [1, 2] {
        for (key, position) in registry.core(instrument_id).unwrap().positions().snapshot() {
            assert!(key.account_id == 1 || key.account_id == 2);
            assert_eq!(position.today, 5);
        }
    }
    assert_eq!(registry.pair(10).unwrap().pair_id, 10);
}

#[test]
fn entry_freezes_prior_settlement_margin_on_both_legs_and_cancel_releases_it() {
    use trading_system_engine::trading::core::RegistryError;

    let mut registry = registry_with_pair();
    registry.set_prior_settlement(1, 50_000).unwrap();
    registry.set_prior_settlement(2, 40_000).unwrap();
    assert_eq!(
        registry.set_prior_settlement(99, 1),
        Err(RegistryError::UnknownInstrument { instrument_id: 99 })
    );

    let rested = registry
        .execute_combo(combo_order(501, 1, 11, Side::Sell, 100, 5))
        .unwrap();
    let order_id = rested_order_id(&rested);

    let leg1 = registry.leg_margin(1, 1);
    assert_eq!(leg1.frozen, 9_000_000);
    assert_eq!(leg1.long_trade_margin, 0);
    assert_eq!(leg1.short_trade_margin, 0);
    assert_eq!(leg1.required(), 0);
    let leg2 = registry.leg_margin(1, 2);
    assert_eq!(leg2.frozen, 6_000_000);
    assert_eq!(leg2.required(), 0);
    assert_eq!(registry.leg_margin(2, 1).frozen, 0);

    let freeze = registry
        .combo_margin_freeze(order_id)
        .expect("a resting combo order freezes leg margin");
    assert_eq!(freeze.order_id, order_id);
    assert_eq!(freeze.pair_id, 10);
    assert_eq!(freeze.account_id, 1);
    assert_eq!(freeze.remaining, 5);
    assert_eq!(freeze.leg1_frozen, 9_000_000);
    assert_eq!(freeze.leg2_frozen, 6_000_000);

    registry.execute_combo(cancel_combo(1, order_id)).unwrap();

    assert_eq!(registry.combo_margin_freeze(order_id), None);
    assert_eq!(registry.leg_margin(1, 1).frozen, 0);
    assert_eq!(registry.leg_margin(1, 2).frozen, 0);
    assert!(registry.combo_book(10).unwrap().is_empty());
}

#[test]
fn implied_fill_replaces_frozen_margin_with_actual_trade_price_margin() {
    let mut registry = registry_with_pair();
    registry.set_prior_settlement(1, 50_000).unwrap();
    registry.set_prior_settlement(2, 40_000).unwrap();
    rest_leg(&mut registry, leg_order(1, 1, Side::Sell, 50_000, 5));
    rest_leg(&mut registry, leg_order(2, 2, Side::Buy, 40_000, 5));

    registry
        .execute_combo(combo_order(1001, 3, 33, Side::Buy, 10_000, 5))
        .unwrap();

    let leg1 = registry.leg_margin(3, 1);
    assert_eq!(leg1.frozen, 0, "filled margin must leave the freeze ledger");
    assert_eq!(leg1.long_trade_margin, 9_000_000);
    assert_eq!(leg1.short_trade_margin, 0);
    assert_eq!(leg1.required(), 9_000_000);

    let leg2 = registry.leg_margin(3, 2);
    assert_eq!(leg2.frozen, 0);
    assert_eq!(leg2.long_trade_margin, 0);
    assert_eq!(leg2.short_trade_margin, 6_000_000);
    assert_eq!(leg2.required(), 6_000_000);

    // Fully filled combo orders carry no residual freeze.
    assert_eq!(registry.combo_margin_freeze(1), None);
}

#[test]
fn partial_direct_fill_releases_only_the_filled_freeze() {
    let mut registry = registry_with_pair();
    registry.set_prior_settlement(1, 50_000).unwrap();
    registry.set_prior_settlement(2, 40_000).unwrap();
    let rested = registry
        .execute_combo(combo_order(601, 1, 11, Side::Sell, 100, 5))
        .unwrap();
    let resting_order_id = rested_order_id(&rested);

    registry
        .execute_combo(combo_order(602, 2, 22, Side::Buy, 100, 2))
        .unwrap();

    let freeze = registry
        .combo_margin_freeze(resting_order_id)
        .expect("the residual keeps a freeze");
    assert_eq!(freeze.remaining, 3);
    assert_eq!(freeze.leg1_frozen, 5_400_000);
    assert_eq!(freeze.leg2_frozen, 3_600_000);
    assert_eq!(registry.leg_margin(1, 1).frozen, 5_400_000);
    assert_eq!(registry.leg_margin(1, 2).frozen, 3_600_000);

    // Both owners replace the filled part with actual trade-price margin.
    let sell_leg1 = registry.leg_margin(1, 1);
    assert_eq!(sell_leg1.short_trade_margin, 3_600_000);
    assert_eq!(registry.leg_margin(1, 2).long_trade_margin, 2_400_000);
    let buy_leg1 = registry.leg_margin(2, 1);
    assert_eq!(buy_leg1.long_trade_margin, 3_600_000);
    assert_eq!(registry.leg_margin(2, 2).short_trade_margin, 2_400_000);

    assert_eq!(registry.combo_margin_freeze(2), None);

    registry
        .execute_combo(cancel_combo(1, resting_order_id))
        .unwrap();
    assert_eq!(registry.leg_margin(1, 1).frozen, 0);
    assert_eq!(registry.leg_margin(1, 2).frozen, 0);
    assert_eq!(registry.leg_margin(1, 1).short_trade_margin, 3_600_000);
}

#[test]
fn single_side_largest_applies_per_account_and_leg() {
    let mut registry = registry_with_pair();
    registry.set_prior_settlement(1, 50_000).unwrap();
    registry.set_prior_settlement(2, 40_000).unwrap();

    // Account 9 buys five spreads, then sells two against another owner.
    registry
        .execute_combo(combo_order(701, 1, 11, Side::Sell, 100, 5))
        .unwrap();
    registry
        .execute_combo(combo_order(702, 9, 91, Side::Buy, 100, 5))
        .unwrap();
    registry
        .execute_combo(combo_order(703, 2, 22, Side::Buy, 100, 2))
        .unwrap();
    registry
        .execute_combo(combo_order(704, 9, 91, Side::Sell, 100, 2))
        .unwrap();

    // Leg1: long 5 x 50_000 and short 2 x 50_000.
    let leg1 = registry.leg_margin(9, 1);
    assert_eq!(leg1.long_trade_margin, 9_000_000);
    assert_eq!(leg1.short_trade_margin, 3_600_000);
    assert_eq!(
        leg1.required(),
        9_000_000,
        "single-side largest must not add the hedged sides"
    );
    // Leg2: short 5 x 40_000 and long 2 x 40_000.
    let leg2 = registry.leg_margin(9, 2);
    assert_eq!(leg2.long_trade_margin, 2_400_000);
    assert_eq!(leg2.short_trade_margin, 6_000_000);
    assert_eq!(leg2.required(), 6_000_000);
}

#[test]
fn unsupported_combo_entry_freezes_nothing_and_does_not_mutate() {
    use trading_system_engine::trading::core::ComboCommandError;

    let mut registry = registry_with_pair();
    registry.set_prior_settlement(1, 50_000).unwrap();
    registry.set_prior_settlement(2, 40_000).unwrap();
    let mut market = combo_order(801, 1, 11, Side::Buy, 100, 5);
    market.price_type = PriceType::AdvancedMarket;

    let error = registry.execute_combo(market).unwrap_err();
    assert_eq!(
        error,
        ComboCommandError::UnsupportedPriceType {
            price_type: PriceType::AdvancedMarket
        }
    );
    assert!(registry.combo_book(10).unwrap().is_empty());
    assert_eq!(registry.leg_margin(1, 1).frozen, 0);
    assert_eq!(registry.leg_margin(1, 2).frozen, 0);
    assert!(registry.combo_private_events().is_empty());
    assert!(registry.core(1).unwrap().positions().snapshot().is_empty());
    assert!(registry.core(2).unwrap().positions().snapshot().is_empty());

    let mut close = combo_order(802, 1, 11, Side::Buy, 100, 5);
    close.offset = Offset::Close;
    let error = registry.execute_combo(close).unwrap_err();
    assert_eq!(
        error,
        ComboCommandError::UnsupportedOffset {
            offset: Offset::Close
        }
    );
    assert_eq!(registry.leg_margin(1, 1).frozen, 0);
    assert!(registry.combo_book(10).unwrap().is_empty());
}

#[test]
fn failed_implied_in_leaves_leg_books_and_combo_effects_unchanged() {
    let mut registry = registry_with_pair();
    registry.set_prior_settlement(1, 50_000).unwrap();
    registry.set_prior_settlement(2, 40_000).unwrap();
    rest_leg(&mut registry, leg_order(1, 1, Side::Sell, 50_000, 5));
    registry.core_mut(1).unwrap().drain_private_events();
    registry.core_mut(1).unwrap().drain_public_events();

    let events = registry
        .execute_combo(combo_order(901, 3, 33, Side::Buy, 10_000, 5))
        .unwrap();
    assert!(matches!(events.as_slice(), [ComboEvent::Rested { .. }]));
    assert!(registry.combo_private_events().is_empty());
    assert!(registry.core(1).unwrap().positions().snapshot().is_empty());
    assert!(registry.core(2).unwrap().positions().snapshot().is_empty());
    assert_eq!(registry.leg_margin(3, 1).long_trade_margin, 0);
    assert_eq!(registry.leg_margin(3, 2).short_trade_margin, 0);
    assert_eq!(registry.core(1).unwrap().best_ask(), Some(50_000));
    assert!(registry.core(1).unwrap().private_events().is_empty());
    assert!(registry.core(1).unwrap().public_events().is_empty());

    // The unfilled taker rests, so its estimate stays frozen.
    let freeze = registry
        .combo_margin_freeze(1)
        .expect("the resting residual keeps its freeze");
    assert_eq!(freeze.remaining, 5);
    assert_eq!(freeze.leg1_frozen, 9_000_000);
    assert_eq!(freeze.leg2_frozen, 6_000_000);
}
