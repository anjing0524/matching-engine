//! Atomic leg-book liquidity behavior through the `TradingCore` public seam.

use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::shared::SymbolId;
use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::core::{CoreEvent, TradingCore};

fn core() -> TradingCore {
    TradingCore::new(
        ContractSpec::new("IF2501", 1, 40_000, 70_000),
        SymbolId::intern("IF2501"),
    )
}

fn new_order(user_id: u32, side: Side, price_ticks: i64, quantity: u64) -> Command {
    let mut command = Command::zeroed();
    command.command_type = CommandType::NewOrder;
    command.client_order_id = u64::from(user_id);
    command.user_id = user_id;
    command.instrument_id = 1;
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

fn rest(core: &mut TradingCore, command: Command) -> u64 {
    let events = core.execute(command);
    let [CoreEvent::Accepted { order_id, .. }] = events.as_slice() else {
        panic!("expected one accepted order, got {events:?}");
    };
    *order_id
}

#[test]
fn plan_reports_crossing_liquidity_in_price_time_order() {
    let mut core = core();
    let first = rest(&mut core, new_order(1, Side::Sell, 50_000, 2));
    let second = rest(&mut core, new_order(2, Side::Sell, 50_010, 4));
    rest(&mut core, new_order(3, Side::Sell, 50_010, 3));

    let plan = core
        .plan_leg_liquidity(Side::Buy, 50_010, 5)
        .expect("crossing liquidity must form a plan");

    let fills: Vec<(u64, u64, u64)> = plan
        .fills
        .iter()
        .map(|fill| (fill.order_id, fill.price_ticks, fill.quantity))
        .collect();
    assert_eq!(fills, vec![(first, 50_000, 2), (second, 50_010, 3)]);
    assert_eq!(plan.taker_side, Side::Buy);
    assert_eq!(plan.limit_price_ticks, 50_010);
    assert_eq!(plan.quantity, 5);

    // Planning is a read-only inspection.
    assert_eq!(core.best_ask(), Some(50_000));
    assert_eq!(core.best_bid(), None);
}

#[test]
fn consume_plan_produces_ordinary_fill_effects() {
    use trading_system_engine::trading::core::LegExecutionContext;
    use trading_system_engine::trading::core::positions::PositionKey;
    use trading_system_engine::trading::core::private_events::PrivateStatus;
    use trading_system_engine::trading::core::public_events::PublicEventType;

    let mut core = core();
    let resting = rest(&mut core, new_order(2, Side::Sell, 50_000, 5));
    // Ignore the accept events while setting up the book.
    core.drain_private_events();
    core.drain_public_events();

    let plan = core.plan_leg_liquidity(Side::Buy, 50_000, 3).unwrap();
    let execution = core
        .consume_leg_liquidity(&plan, LegExecutionContext::new(7, 99))
        .expect("a validated plan must consume");

    assert_eq!(execution.instrument_id, 7);
    assert_eq!(execution.taker_order_id, 99);
    assert_eq!(execution.taker_side, Side::Buy);
    assert_eq!(execution.quantity, 3);
    assert_eq!(execution.worst_price_ticks(), Some(50_000));
    assert_eq!(execution.fills.len(), 1);
    let fill = execution.fills[0];
    assert_eq!(fill.order_id, resting);
    assert_eq!(fill.account_id, 2);
    assert_eq!(fill.side, Side::Sell);
    assert_eq!(fill.price_ticks, 50_000);
    assert_eq!(fill.quantity, 3);
    assert_eq!(fill.remaining, 2);
    assert_eq!(fill.trade_id, 1);

    assert_eq!(core.best_ask(), Some(50_000));
    assert_eq!(
        core.positions()
            .get(PositionKey::new(2, Side::Sell, Hedge::Spec))
            .today,
        3
    );
    assert_eq!(
        core.pending_open()
            .get(PositionKey::new(2, Side::Sell, Hedge::Spec))
            .today,
        2
    );

    let private = core.drain_private_events();
    assert_eq!(private.len(), 1);
    assert_eq!(private[0].order_id, resting);
    assert_eq!(private[0].account_id, 2);
    assert_eq!(private[0].filled_delta, 3);
    assert_eq!(private[0].remaining, 2);
    assert_eq!(private[0].status, PrivateStatus::PartiallyFilled);

    let public = core.drain_public_events();
    assert_eq!(public.len(), 1);
    assert_eq!(public[0].event_type, PublicEventType::Trade);
    assert_eq!(public[0].instrument_id, 7);
    assert_eq!(public[0].trade_id, 1);
    assert_eq!(public[0].buy_order_id, 99);
    assert_eq!(public[0].sell_order_id, resting);
    assert_eq!(public[0].price_ticks, 50_000);
    assert_eq!(public[0].quantity, 3);
    assert_eq!(public[0].side, Some(Side::Buy));
}

#[test]
fn rejected_plans_leave_book_positions_and_events_unchanged() {
    use trading_system_engine::trading::core::kernel::{CancelCommand, OrderStatus};
    use trading_system_engine::trading::core::{LegExecutionContext, LegLiquidityError};

    let mut core = core();
    let resting = rest(&mut core, new_order(2, Side::Sell, 50_000, 5));
    core.drain_private_events();
    core.drain_public_events();

    let insufficient = core.plan_leg_liquidity(Side::Buy, 50_000, 6).unwrap_err();
    assert_eq!(
        insufficient,
        LegLiquidityError::InsufficientLiquidity {
            requested: 6,
            available: 5,
        }
    );
    assert_eq!(core.best_ask(), Some(50_000));
    assert!(core.positions().snapshot().is_empty());
    assert!(core.private_events().is_empty());
    assert!(core.public_events().is_empty());

    let zero = core.plan_leg_liquidity(Side::Buy, 50_000, 0).unwrap_err();
    assert_eq!(zero, LegLiquidityError::ZeroQuantity);
    assert_eq!(core.best_ask(), Some(50_000));

    // A plan becomes stale when better liquidity arrives after planning.
    let plan = core.plan_leg_liquidity(Side::Buy, 50_010, 5).unwrap();
    let better = rest(&mut core, new_order(3, Side::Sell, 49_990, 5));
    core.drain_private_events();
    core.drain_public_events();
    let stale = core
        .consume_leg_liquidity(&plan, LegExecutionContext::new(7, 99))
        .unwrap_err();
    assert_eq!(stale, LegLiquidityError::StalePlan);
    assert_eq!(core.best_ask(), Some(49_990));
    assert!(core.positions().snapshot().is_empty());
    assert!(core.private_events().is_empty());
    assert!(core.public_events().is_empty());

    // Cancel the planned order; the stale plan must still not mutate anything.
    let plan = core.plan_leg_liquidity(Side::Buy, 49_990, 5).unwrap();
    assert!(matches!(
        core.cancel(CancelCommand {
            user_id: 3,
            order_id: better,
        }),
        CoreEvent::Canceled { .. }
    ));
    core.drain_private_events();
    core.drain_public_events();
    let missing = core
        .consume_leg_liquidity(&plan, LegExecutionContext::new(7, 99))
        .unwrap_err();
    assert_eq!(
        missing,
        LegLiquidityError::InsufficientLiquidity {
            requested: 5,
            available: 0,
        }
    );
    assert_eq!(core.best_ask(), Some(50_000));
    assert!(core.positions().snapshot().is_empty());
    assert!(core.private_events().is_empty());
    assert!(core.public_events().is_empty());

    // The untouched order and its resting metadata are still intact.
    assert_eq!(core.order_status(resting), Some(OrderStatus::Accepted));
}

fn new_order_for(
    instrument_id: u32,
    user_id: u32,
    side: Side,
    price_ticks: i64,
    quantity: u64,
) -> Command {
    let mut command = new_order(user_id, side, price_ticks, quantity);
    command.instrument_id = instrument_id;
    command
}

fn spec(symbol: &str) -> ContractSpec {
    ContractSpec::new(symbol, 1, 40_000, 70_000)
}

fn registry_with_two_legs() -> trading_system_engine::trading::core::InstrumentRegistry {
    use trading_system_engine::trading::core::InstrumentRegistry;

    let mut registry = InstrumentRegistry::new();
    registry.register(1, spec("IF2501")).unwrap();
    registry.register(2, spec("IF2502")).unwrap();
    registry
}

#[test]
fn registry_consumes_two_legs_atomically_without_a_combo_pair() {
    use trading_system_engine::trading::core::positions::PositionKey;
    use trading_system_engine::trading::core::private_events::PrivateStatus;
    use trading_system_engine::trading::core::public_events::PublicEventType;

    let mut registry = registry_with_two_legs();
    assert_eq!(registry.pair_count(), 0);

    registry
        .execute(new_order_for(1, 1, Side::Sell, 50_000, 5))
        .unwrap();
    registry
        .execute(new_order_for(2, 2, Side::Buy, 40_000, 5))
        .unwrap();
    for instrument_id in [1, 2] {
        let core = registry.core_mut(instrument_id).unwrap();
        core.drain_private_events();
        core.drain_public_events();
    }

    let leg1_plan = registry
        .core(1)
        .unwrap()
        .plan_leg_liquidity(Side::Buy, 50_000, 5)
        .unwrap();
    let leg2_plan = registry
        .core(2)
        .unwrap()
        .plan_leg_liquidity(Side::Sell, 40_000, 5)
        .unwrap();

    let execution = registry
        .consume_combo_legs(1, 2, 77, &leg1_plan, &leg2_plan)
        .expect("both legs have matching liquidity");

    assert_eq!(execution.quantity, 5);
    assert_eq!(execution.taker_order_id, 77);
    assert_eq!(execution.leg1.instrument_id, 1);
    assert_eq!(execution.leg1.taker_side, Side::Buy);
    assert_eq!(execution.leg1.worst_price_ticks(), Some(50_000));
    assert_eq!(execution.leg2.instrument_id, 2);
    assert_eq!(execution.leg2.taker_side, Side::Sell);
    assert_eq!(execution.leg2.worst_price_ticks(), Some(40_000));

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
    assert_eq!(leg1_private[0].status, PrivateStatus::Filled);
    assert_eq!(leg1_private[0].account_id, 1);
    let leg1_public = registry.core_mut(1).unwrap().drain_public_events();
    assert_eq!(leg1_public.len(), 1);
    assert_eq!(leg1_public[0].event_type, PublicEventType::Trade);
    assert_eq!(leg1_public[0].instrument_id, 1);
    assert_eq!(leg1_public[0].buy_order_id, 77);
    assert_eq!(
        leg1_public[0].sell_order_id,
        execution.leg1.fills[0].order_id
    );

    let leg2_private = registry.core_mut(2).unwrap().drain_private_events();
    assert_eq!(leg2_private.len(), 1);
    assert_eq!(leg2_private[0].status, PrivateStatus::Filled);
    assert_eq!(leg2_private[0].account_id, 2);
    let leg2_public = registry.core_mut(2).unwrap().drain_public_events();
    assert_eq!(leg2_public.len(), 1);
    assert_eq!(leg2_public[0].event_type, PublicEventType::Trade);
    assert_eq!(leg2_public[0].instrument_id, 2);
    assert_eq!(leg2_public[0].sell_order_id, 77);
    assert_eq!(
        leg2_public[0].buy_order_id,
        execution.leg2.fills[0].order_id
    );
}

#[test]
fn registry_rejects_unequal_or_stale_two_leg_plans_before_mutation() {
    use trading_system_engine::trading::core::{ComboLeg, ComboLegError, LegLiquidityError};

    let mut registry = registry_with_two_legs();
    registry
        .execute(new_order_for(1, 1, Side::Sell, 50_000, 5))
        .unwrap();
    registry
        .execute(new_order_for(2, 2, Side::Buy, 40_000, 4))
        .unwrap();
    for instrument_id in [1, 2] {
        let core = registry.core_mut(instrument_id).unwrap();
        core.drain_private_events();
        core.drain_public_events();
    }

    let leg1_plan = registry
        .core(1)
        .unwrap()
        .plan_leg_liquidity(Side::Buy, 50_000, 5)
        .unwrap();
    let leg2_plan = registry
        .core(2)
        .unwrap()
        .plan_leg_liquidity(Side::Sell, 40_000, 4)
        .unwrap();
    assert_eq!(
        registry
            .consume_combo_legs(1, 2, 77, &leg1_plan, &leg2_plan)
            .unwrap_err(),
        ComboLegError::QuantityMismatch {
            leg1_quantity: 5,
            leg2_quantity: 4,
        }
    );
    assert_legs_untouched(&registry, Side::Sell, 50_000, Side::Buy, 40_000);

    // Build an equal-quantity second-leg plan, then make it stale with a later,
    // better bid. The first leg must not move because validation happens before
    // either commit.
    registry
        .execute(new_order_for(2, 3, Side::Buy, 40_010, 5))
        .unwrap();
    let leg2_plan = registry
        .core(2)
        .unwrap()
        .plan_leg_liquidity(Side::Sell, 40_010, 5)
        .unwrap();
    registry
        .execute(new_order_for(2, 4, Side::Buy, 40_020, 5))
        .unwrap();
    for instrument_id in [1, 2] {
        let core = registry.core_mut(instrument_id).unwrap();
        core.drain_private_events();
        core.drain_public_events();
    }
    assert_eq!(
        registry
            .consume_combo_legs(1, 2, 77, &leg1_plan, &leg2_plan)
            .unwrap_err(),
        ComboLegError::Plan {
            leg: ComboLeg::Second,
            error: LegLiquidityError::StalePlan,
        }
    );
    assert_eq!(registry.core(1).unwrap().best_ask(), Some(50_000));
    assert_eq!(registry.core(2).unwrap().best_bid(), Some(40_020));
    for instrument_id in [1, 2] {
        let core = registry.core(instrument_id).unwrap();
        assert!(core.positions().snapshot().is_empty());
        assert!(core.private_events().is_empty());
        assert!(core.public_events().is_empty());
    }
}

fn assert_legs_untouched(
    registry: &trading_system_engine::trading::core::InstrumentRegistry,
    leg1_side: Side,
    leg1_price: u64,
    leg2_side: Side,
    leg2_price: u64,
) {
    let leg1 = registry.core(1).unwrap();
    match leg1_side {
        Side::Buy => assert_eq!(leg1.best_bid(), Some(leg1_price)),
        Side::Sell => assert_eq!(leg1.best_ask(), Some(leg1_price)),
    }
    let leg2 = registry.core(2).unwrap();
    match leg2_side {
        Side::Buy => assert_eq!(leg2.best_bid(), Some(leg2_price)),
        Side::Sell => assert_eq!(leg2.best_ask(), Some(leg2_price)),
    }
    for instrument_id in [1, 2] {
        let core = registry.core(instrument_id).unwrap();
        assert!(core.positions().snapshot().is_empty());
        assert!(core.private_events().is_empty());
        assert!(core.public_events().is_empty());
    }
}

#[test]
fn sell_plan_walks_bids_by_descending_price_and_preserves_fifo() {
    let mut core = core();
    rest(&mut core, new_order(1, Side::Buy, 50_000, 1));
    let first = rest(&mut core, new_order(2, Side::Buy, 50_010, 2));
    let second = rest(&mut core, new_order(3, Side::Buy, 50_010, 3));

    let plan = core.plan_leg_liquidity(Side::Sell, 50_000, 3).unwrap();
    let fills: Vec<(u64, u64, u64)> = plan
        .fills
        .iter()
        .map(|fill| (fill.order_id, fill.price_ticks, fill.quantity))
        .collect();
    assert_eq!(fills, vec![(first, 50_010, 2), (second, 50_010, 1)]);
}

#[test]
fn consuming_a_close_order_releases_frozen_close_margin() {
    use trading_system_engine::trading::core::LegExecutionContext;
    use trading_system_engine::trading::core::plugins::AvailableClosePlugin;
    use trading_system_engine::trading::core::positions::{CloseKey, PositionKey};
    use trading_system_engine::trading::core::private_events::PrivateStatus;

    let mut core = core();
    core.positions_mut()
        .apply_open(PositionKey::new(1, Side::Buy, Hedge::Spec), 10)
        .unwrap();
    let mut close = new_order(1, Side::Sell, 50_000, 4);
    close.offset = Offset::CloseToday;
    let events = core.execute_with_plugins(close, &AvailableClosePlugin);
    assert!(matches!(events.as_slice(), [CoreEvent::Accepted { .. }]));
    core.drain_private_events();
    core.drain_public_events();

    let position = PositionKey::new(1, Side::Buy, Hedge::Spec);
    let close_key = CloseKey::new(position, Offset::CloseToday);
    assert_eq!(
        core.frozen_close().available(close_key, core.positions()),
        6
    );

    let plan = core.plan_leg_liquidity(Side::Buy, 50_000, 4).unwrap();
    let execution = core
        .consume_leg_liquidity(&plan, LegExecutionContext::new(1, 55))
        .unwrap();
    assert_eq!(execution.quantity, 4);
    assert_eq!(core.positions().get(position).today, 6);
    assert_eq!(
        core.frozen_close().available(close_key, core.positions()),
        6
    );
    let private = core.drain_private_events();
    assert_eq!(private.len(), 1);
    assert_eq!(private[0].status, PrivateStatus::Filled);
    assert_eq!(private[0].filled_delta, 4);
    assert_eq!(private[0].remaining, 0);
}

#[test]
fn consuming_the_last_quantity_removes_the_resting_order() {
    use trading_system_engine::trading::core::LegExecutionContext;
    use trading_system_engine::trading::core::positions::PositionKey;
    use trading_system_engine::trading::core::private_events::PrivateStatus;

    let mut core = core();
    rest(&mut core, new_order(2, Side::Sell, 50_000, 5));
    core.drain_private_events();
    core.drain_public_events();

    let first = core.plan_leg_liquidity(Side::Buy, 50_000, 3).unwrap();
    core.consume_leg_liquidity(&first, LegExecutionContext::new(1, 7))
        .unwrap();
    let second = core.plan_leg_liquidity(Side::Buy, 50_000, 2).unwrap();
    let execution = core
        .consume_leg_liquidity(&second, LegExecutionContext::new(1, 7))
        .unwrap();

    assert_eq!(execution.fills[0].remaining, 0);
    assert_eq!(core.best_ask(), None);
    assert_eq!(
        core.positions()
            .get(PositionKey::new(2, Side::Sell, Hedge::Spec))
            .today,
        5
    );
    assert_eq!(
        core.pending_open()
            .get(PositionKey::new(2, Side::Sell, Hedge::Spec))
            .today,
        0
    );
    let private = core.drain_private_events();
    let last = private.last().expect("a fill event must be emitted");
    assert_eq!(last.status, PrivateStatus::Filled);
    assert_eq!(last.filled_delta, 2);
    assert_eq!(last.remaining, 0);
}
