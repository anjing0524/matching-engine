//! Account-level available-margin backing for M1 Combo Order entry through the
//! [`InstrumentRegistry`] seam.
//!
//! Entry freezes the prior-settlement estimate on both leg Contracts. The
//! registry rejects the whole entry when either leg cannot reserve from the
//! account's configured balance, and availability reflects cancel, partial-fill
//! release, actual trade-price margin, and single-side-largest.

use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::core::{
    ComboCommandError, ComboEvent, ComboLeg, ComboMarginError, ComboPair, ComboPairMargin,
    CoreEvent, InstrumentRegistry,
};

const LEG1_SETTLEMENT: u64 = 50_000;
const LEG2_SETTLEMENT: u64 = 40_000;
const QUANTITY: u64 = 5;
/// `50_000 * 300 * 5 * 1_200 / 10_000`.
const LEG1_ESTIMATE: u128 = 9_000_000;
/// `40_000 * 300 * 5 * 1_000 / 10_000`.
const LEG2_ESTIMATE: u128 = 6_000_000;
const BOTH_LEGS_ESTIMATE: u64 = 15_000_000;

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
    registry
}

fn registry_with_settlements() -> InstrumentRegistry {
    let mut registry = registry_with_pair();
    registry.set_prior_settlement(1, LEG1_SETTLEMENT).unwrap();
    registry.set_prior_settlement(2, LEG2_SETTLEMENT).unwrap();
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
fn entry_reserves_both_legs_at_the_exact_available_boundary() {
    let mut registry = registry_with_settlements();
    registry.set_margin_balance(1, BOTH_LEGS_ESTIMATE);
    assert_eq!(registry.margin_balance(1), Some(BOTH_LEGS_ESTIMATE));
    assert_eq!(
        registry.available_margin(1),
        Some(BOTH_LEGS_ESTIMATE),
        "an untouched balance is fully available"
    );
    assert_eq!(registry.margin_balance(2), None);

    let events = registry
        .execute_combo(combo_order(1, 1, 11, Side::Sell, 100, QUANTITY))
        .unwrap();
    let order_id = rested_order_id(&events);

    let freeze = registry
        .combo_margin_freeze(order_id)
        .expect("an accepted entry freezes both legs");
    assert_eq!(freeze.leg1_frozen, LEG1_ESTIMATE);
    assert_eq!(freeze.leg2_frozen, LEG2_ESTIMATE);
    assert_eq!(freeze.remaining, QUANTITY);
    assert_eq!(registry.leg_margin(1, 1).frozen, LEG1_ESTIMATE);
    assert_eq!(registry.leg_margin(1, 2).frozen, LEG2_ESTIMATE);
    assert_eq!(
        registry.available_margin(1),
        Some(0),
        "the exact boundary leaves nothing available"
    );
}

#[test]
fn entry_is_rejected_when_the_first_leg_cannot_reserve() {
    let mut registry = registry_with_settlements();
    let balance = u64::try_from(LEG1_ESTIMATE).unwrap() - 1;
    registry.set_margin_balance(1, balance);

    let error = registry
        .execute_combo(combo_order(1, 1, 11, Side::Sell, 100, QUANTITY))
        .unwrap_err();

    assert_eq!(
        error,
        ComboCommandError::Margin(ComboMarginError::InsufficientMargin {
            account_id: 1,
            leg: ComboLeg::First,
            required: LEG1_ESTIMATE,
            available: u128::from(balance),
        })
    );
    assert_eq!(registry.available_margin(1), Some(balance));
}

#[test]
fn entry_is_rejected_when_the_second_leg_cannot_reserve() {
    let mut registry = registry_with_settlements();
    let balance = BOTH_LEGS_ESTIMATE - 1;
    registry.set_margin_balance(1, balance);

    let error = registry
        .execute_combo(combo_order(1, 1, 11, Side::Sell, 100, QUANTITY))
        .unwrap_err();

    assert_eq!(
        error,
        ComboCommandError::Margin(ComboMarginError::InsufficientMargin {
            account_id: 1,
            leg: ComboLeg::Second,
            required: LEG2_ESTIMATE,
            available: u128::from(balance) - LEG1_ESTIMATE,
        })
    );
    assert_eq!(registry.available_margin(1), Some(balance));
}

#[test]
fn rejected_entry_does_not_mutate_book_margin_or_position() {
    let mut registry = registry_with_settlements();
    registry.set_margin_balance(2, BOTH_LEGS_ESTIMATE);

    // Liquidity and a direct counterparty the rejected entry would otherwise
    // consume.
    rest_leg(
        &mut registry,
        leg_order(1, 100, Side::Sell, 50_000, QUANTITY),
    );
    rest_leg(
        &mut registry,
        leg_order(2, 101, Side::Buy, 40_000, QUANTITY),
    );
    registry
        .execute_combo(combo_order(2, 2, 22, Side::Sell, 100, QUANTITY))
        .unwrap();
    for instrument_id in [1, 2] {
        let core = registry.core_mut(instrument_id).unwrap();
        core.drain_private_events();
        core.drain_public_events();
    }
    registry.drain_combo_private_events();

    let combo_book_before = registry.combo_book(10).unwrap().snapshot();
    let leg1_before = registry.core(1).unwrap().snapshot();
    let leg2_before = registry.core(2).unwrap().snapshot();
    let balance = u64::try_from(LEG1_ESTIMATE).unwrap() - 1;
    registry.set_margin_balance(1, balance);

    let error = registry
        .execute_combo(combo_order(1, 1, 11, Side::Buy, 100, QUANTITY))
        .unwrap_err();
    assert!(
        matches!(
            error,
            ComboCommandError::Margin(ComboMarginError::InsufficientMargin {
                leg: ComboLeg::First,
                ..
            })
        ),
        "unexpected rejection: {error:?}"
    );

    assert_eq!(
        registry.combo_book(10).unwrap().snapshot(),
        combo_book_before
    );
    assert_eq!(registry.core(1).unwrap().snapshot(), leg1_before);
    assert_eq!(registry.core(2).unwrap().snapshot(), leg2_before);
    for instrument_id in [1, 2] {
        let margin = registry.leg_margin(1, instrument_id);
        assert_eq!(margin.frozen, 0);
        assert_eq!(margin.required(), 0);
    }
    assert!(registry.combo_private_events().is_empty());
    assert_eq!(registry.available_margin(1), Some(balance));

    // A rejected entry consumes no exchange order id: the resting order above
    // was id 1, so the next accepted order must be id 2.
    registry.set_margin_balance(1, BOTH_LEGS_ESTIMATE);
    let events = registry
        .execute_combo(combo_order(3, 1, 11, Side::Buy, 100, QUANTITY))
        .unwrap();
    let [ComboEvent::Filled { fill }] = events.as_slice() else {
        panic!("expected a direct fill, got {events:?}");
    };
    assert_eq!(fill.buy_order_id, 2);
}

#[test]
fn cancel_releases_the_reservation_and_restores_availability() {
    let mut registry = registry_with_settlements();
    registry.set_margin_balance(1, BOTH_LEGS_ESTIMATE);
    let rested = registry
        .execute_combo(combo_order(1, 1, 11, Side::Sell, 100, QUANTITY))
        .unwrap();
    let order_id = rested_order_id(&rested);
    assert_eq!(registry.available_margin(1), Some(0));

    let canceled = registry.execute_combo(cancel_combo(1, order_id)).unwrap();
    assert!(matches!(canceled.as_slice(), [ComboEvent::Canceled { .. }]));

    assert_eq!(registry.combo_margin_freeze(order_id), None);
    assert_eq!(registry.leg_margin(1, 1).frozen, 0);
    assert_eq!(registry.leg_margin(1, 2).frozen, 0);
    assert_eq!(
        registry.available_margin(1),
        Some(BOTH_LEGS_ESTIMATE),
        "cancel must return the whole reservation"
    );
    assert!(registry.combo_book(10).unwrap().is_empty());
}

#[test]
fn fill_replaces_the_estimate_with_actual_trade_price_margin() {
    let mut registry = registry_with_settlements();
    // Leg liquidity trades above the prior settlements, so actual margin is
    // larger than the frozen estimate.
    rest_leg(
        &mut registry,
        leg_order(1, 100, Side::Sell, 55_000, QUANTITY),
    );
    rest_leg(
        &mut registry,
        leg_order(2, 101, Side::Buy, 45_000, QUANTITY),
    );

    let balance = 20_000_000;
    registry.set_margin_balance(3, balance);
    let events = registry
        .execute_combo(combo_order(1, 3, 33, Side::Buy, 10_000, QUANTITY))
        .unwrap();
    assert!(matches!(
        events.as_slice(),
        [ComboEvent::ImpliedFilled { .. }]
    ));

    let leg1 = registry.leg_margin(3, 1);
    assert_eq!(leg1.frozen, 0, "the estimate must leave the ledger");
    assert_eq!(leg1.long_trade_margin, 9_900_000);
    assert_eq!(leg1.required(), 9_900_000);
    let leg2 = registry.leg_margin(3, 2);
    assert_eq!(leg2.frozen, 0);
    assert_eq!(leg2.short_trade_margin, 6_750_000);
    assert_eq!(leg2.required(), 6_750_000);
    assert_eq!(
        registry.available_margin(3),
        Some(balance - 16_650_000),
        "availability must charge the actual margin, not the old estimate plus actual"
    );
    assert_eq!(registry.combo_margin_freeze(1), None);
}

#[test]
fn available_margin_uses_single_side_largest_after_hedged_fills() {
    let mut registry = registry_with_settlements();
    let balance = 30_000_000;
    registry.set_margin_balance(9, balance);
    registry.set_margin_balance(1, BOTH_LEGS_ESTIMATE);
    registry.set_margin_balance(2, 6_000_000);

    registry
        .execute_combo(combo_order(1, 1, 11, Side::Sell, 100, QUANTITY))
        .unwrap();
    // Account 9 buys five spreads, then sells two against another owner.
    registry
        .execute_combo(combo_order(2, 9, 91, Side::Buy, 100, QUANTITY))
        .unwrap();
    registry
        .execute_combo(combo_order(3, 2, 22, Side::Buy, 100, 2))
        .unwrap();
    registry
        .execute_combo(combo_order(4, 9, 91, Side::Sell, 100, 2))
        .unwrap();

    let leg1 = registry.leg_margin(9, 1);
    assert_eq!(leg1.long_trade_margin, LEG1_ESTIMATE);
    assert_eq!(leg1.short_trade_margin, 3_600_000);
    assert_eq!(leg1.required(), LEG1_ESTIMATE);
    let leg2 = registry.leg_margin(9, 2);
    assert_eq!(leg2.long_trade_margin, 2_400_000);
    assert_eq!(leg2.short_trade_margin, LEG2_ESTIMATE);
    assert_eq!(leg2.required(), LEG2_ESTIMATE);
    assert_eq!(
        registry.available_margin(9),
        Some(balance - 15_000_000),
        "the hedged sides must be charged once, at the larger side"
    );
}

#[test]
fn partial_fill_releases_exactly_the_filled_estimate() {
    let mut registry = registry_with_settlements();
    let balance = 25_000_000;
    registry.set_margin_balance(1, balance);
    registry.set_margin_balance(2, 25_000_000);
    let rested = registry
        .execute_combo(combo_order(1, 1, 11, Side::Sell, 100, QUANTITY))
        .unwrap();
    let resting_order_id = rested_order_id(&rested);
    assert_eq!(registry.available_margin(1), Some(balance - 15_000_000));

    registry
        .execute_combo(combo_order(2, 2, 22, Side::Buy, 100, 2))
        .unwrap();

    let freeze = registry
        .combo_margin_freeze(resting_order_id)
        .expect("the residual keeps a freeze");
    assert_eq!(freeze.remaining, 3);
    assert_eq!(registry.available_margin(1), Some(balance - 15_000_000));

    // Cancel releases only the unfilled estimate; filled actual margin stays.
    registry
        .execute_combo(cancel_combo(1, resting_order_id))
        .unwrap();
    assert_eq!(registry.available_margin(1), Some(balance - 6_000_000));
}
