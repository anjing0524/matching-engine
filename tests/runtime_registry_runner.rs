//! Registry-backed command routing for the single-process runtime.
//!
//! [`RegistryRunner`] is the production command seam: ordinary commands go to
//! the shared Contract core for their instrument id, while commands carrying a
//! non-zero `combo_id` go to the registry's Combo Pair book. Combo private fills
//! are returned alongside core private/public events so the gateway can project
//! them with one per-user private sequence.

use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::trading::core::plugins::NoPlugins;
use trading_system_engine::trading::core::private_events::PrivateStatus;
use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::core::{ComboPair, ComboPairMargin, InstrumentRegistry};
use trading_system_engine::trading::transport::create;
use trading_system_engine::trading::transport::runner::RegistryRunner;

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
    registry.set_margin_balance(1, 1_000_000_000);
    registry.set_margin_balance(2, 1_000_000_000);
    registry.set_require_explicit_margin(true);
    registry
}

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "trading-registry-runner-{name}-{}",
        std::process::id()
    ))
}

fn combo_order(user_id: u32, client_order_id: u64, side: Side, spread: i64) -> Command {
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
    command.price_ticks = spread;
    command.quantity = 1;
    command.trigger_type = TriggerType::None;
    command
}

fn leg_order(user_id: u32, client_order_id: u64, side: Side, price: i64) -> Command {
    let mut command = combo_order(user_id, client_order_id, side, price);
    command.combo_id = 0;
    command.instrument_id = 1;
    command
}

#[test]
fn combo_commands_route_to_the_pair_book_not_the_ordinary_core() {
    let path = temp_path("route");
    let _ = std::fs::remove_file(&path);
    let (producer, consumer) = create(&path, 8).unwrap();
    let mut runner = RegistryRunner::new(registry_with_pair(), consumer, NoPlugins);

    // The instrument id is deliberately set: a combo command must be routed by
    // combo id, never treated as an ordinary order on its leg Contract.
    let mut command = combo_order(1, 1, Side::Sell, 100);
    command.instrument_id = 1;
    producer.send(&command).unwrap();
    let events = runner.poll_events();

    assert_eq!(events.commands, 1);
    assert!(events.combo_private.is_empty());
    let accepted: Vec<_> = events
        .private
        .iter()
        .filter(|event| event.status == PrivateStatus::Accepted)
        .collect();
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0].account_id, 1);
    assert_eq!(accepted[0].order_id, 1);
    assert_eq!(accepted[0].remaining, 1);
    assert!(
        runner.registry().core(1).unwrap().best_bid().is_none()
            && runner.registry().core(1).unwrap().best_ask().is_none(),
        "a combo command must not mutate an ordinary Contract book"
    );
    assert_eq!(
        runner
            .registry()
            .combo_book(10)
            .unwrap()
            .snapshot()
            .asks
            .len(),
        1
    );
    drop(producer);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn direct_combo_fill_returns_one_private_fill_per_owner() {
    let path = temp_path("direct-fill");
    let _ = std::fs::remove_file(&path);
    let (producer, consumer) = create(&path, 8).unwrap();
    let mut runner = RegistryRunner::new(registry_with_pair(), consumer, NoPlugins);

    producer.send(&combo_order(1, 1, Side::Sell, 100)).unwrap();
    assert_eq!(runner.poll_events().commands, 1);
    producer.send(&combo_order(2, 2, Side::Buy, 100)).unwrap();
    let events = runner.poll_events();

    assert_eq!(events.commands, 1);
    assert_eq!(events.combo_private.len(), 2);
    let mut users: Vec<u64> = events
        .combo_private
        .iter()
        .map(|fill| fill.user_id)
        .collect();
    users.sort_unstable();
    assert_eq!(users, vec![1, 2]);
    assert!(
        events
            .combo_private
            .iter()
            .all(|fill| fill.pair_id == 10 && fill.quantity == 1 && fill.spread_price == 100)
    );
    let taker_accepted = events
        .private
        .iter()
        .find(|event| event.status == PrivateStatus::Accepted)
        .expect("taker acceptance");
    assert_eq!(taker_accepted.account_id, 2);
    assert_eq!(taker_accepted.remaining, 0);
    drop(producer);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn ordinary_leg_commands_still_reach_the_shared_contract_core() {
    let path = temp_path("leg-route");
    let _ = std::fs::remove_file(&path);
    let (producer, consumer) = create(&path, 8).unwrap();
    let mut runner = RegistryRunner::new(registry_with_pair(), consumer, NoPlugins);

    producer.send(&leg_order(1, 1, Side::Buy, 50_000)).unwrap();
    let events = runner.poll_events();

    assert_eq!(events.commands, 1);
    assert!(events.combo_private.is_empty());
    assert!(events.private.iter().any(|event| {
        event.status == PrivateStatus::Accepted && event.account_id == 1 && event.order_id == 1
    }));
    assert!(events.public.iter().any(|event| {
        event.event_type
            == trading_system_engine::trading::core::public_events::PublicEventType::OrderRested
    }));
    assert_eq!(runner.registry().core(1).unwrap().best_bid(), Some(50_000));
    drop(producer);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn unsupported_combo_order_is_rejected_without_mutating_state() {
    let path = temp_path("reject");
    let _ = std::fs::remove_file(&path);
    let (producer, consumer) = create(&path, 8).unwrap();
    let mut runner = RegistryRunner::new(registry_with_pair(), consumer, NoPlugins);

    let mut command = combo_order(1, 1, Side::Buy, 100);
    command.time_condition = TimeCondition::Gis;
    producer.send(&command).unwrap();
    let events = runner.poll_events();

    assert_eq!(events.commands, 1);
    assert!(events.combo_private.is_empty());
    let rejected = events
        .private
        .iter()
        .find(|event| event.status == PrivateStatus::Rejected)
        .expect("private rejection");
    assert_eq!(rejected.account_id, 1);
    assert!(rejected.reason.is_some());
    assert_eq!(runner.registry().next_combo_order_id(), 1);
    assert!(
        runner
            .registry()
            .combo_book(10)
            .unwrap()
            .snapshot()
            .bids
            .is_empty()
    );
    drop(producer);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn runner_fails_closed_when_a_combo_account_has_no_balance() {
    let path = temp_path("fail-closed");
    let _ = std::fs::remove_file(&path);
    let (producer, consumer) = create(&path, 8).unwrap();
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
    // Deliberately no balance and no explicit strict flag: the production
    // runner itself must enable fail-closed entry for a paired venue.
    let mut runner = RegistryRunner::new(registry, consumer, NoPlugins);
    assert!(runner.registry().requires_explicit_margin());

    producer.send(&combo_order(7, 1, Side::Buy, 100)).unwrap();
    let events = runner.poll_events();
    let rejected = events
        .private
        .iter()
        .find(|event| event.status == PrivateStatus::Rejected)
        .expect("unconfigured account rejection");
    assert_eq!(rejected.account_id, 7);
    assert!(events.combo_private.is_empty());
    assert_eq!(runner.registry().next_combo_order_id(), 1);
    drop(producer);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn resting_combo_fill_from_a_leg_command_returns_combo_private_and_public_stats() {
    let path = temp_path("resting-combo-trigger");
    let _ = std::fs::remove_file(&path);
    let (producer, consumer) = create(&path, 8).unwrap();
    let mut runner = RegistryRunner::new(registry_with_pair(), consumer, NoPlugins);

    producer
        .send(&combo_order(1, 1, Side::Buy, 10_000))
        .unwrap();
    let accepted = runner.poll_events();
    assert!(accepted.private.iter().any(|event| {
        event.status == PrivateStatus::Accepted && event.account_id == 1 && event.order_id == 1
    }));
    assert!(accepted.combo_private.is_empty());

    // The first leg order only rests. The second completes the implied combo
    // and fills the resting Combo Order through the same runner poll.
    producer.send(&leg_order(2, 2, Side::Sell, 50_000)).unwrap();
    let first_leg = runner.poll_events();
    assert!(first_leg.combo_private.is_empty());
    assert_eq!(
        first_leg
            .public
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        vec![1]
    );
    let mut second_leg = leg_order(3, 3, Side::Buy, 40_000);
    second_leg.instrument_id = 2;
    producer.send(&second_leg).unwrap();
    let filled = runner.poll_events();

    assert_eq!(filled.combo_private.len(), 1, "{:?}", filled.combo_private);
    // One venue sequence continues across both leg cores and the Combo trade.
    assert_eq!(filled.public.first().map(|event| event.seq), Some(2));
    assert!(
        filled
            .public
            .windows(2)
            .all(|pair| pair[0].seq + 1 == pair[1].seq),
        "public sequence must not collide or regress: {:?}",
        filled.public
    );
    let fill = filled.combo_private[0];
    assert_eq!(fill.pair_id, 10);
    assert_eq!(fill.user_id, 1);
    assert_eq!(fill.quantity, 1);
    assert_eq!(fill.spread_price, 10_000);
    let public = filled
        .public
        .iter()
        .find(|event| {
            event.event_type
                == trading_system_engine::trading::core::public_events::PublicEventType::ComboTrade
        })
        .expect("combo price statistic");
    assert_eq!(public.pair_id, 10);
    assert_eq!(public.spread_price, 10_000);
    assert_eq!(public.quantity, 1);
    assert_eq!(
        public.seq,
        filled.public.last().map(|event| event.seq).unwrap()
    );
    drop(producer);
    let _ = std::fs::remove_file(&path);
}
