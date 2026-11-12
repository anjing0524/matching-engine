//! Registry-backed combo snapshot, restore, and reconnect replay.
//!
//! The seam is [`InstrumentRegistry`]: static Combo Pairs, base Combo Orders,
//! the shared ordinary leg books, leg margin, and combo private sequence all
//! live behind it. Tests capture and restore state there, then keep matching.

use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::core::{
    ComboEvent, ComboPair, ComboPairMargin, CoreEvent, InstrumentRegistry, RegistrySnapshot,
    RegistrySnapshotError,
};
use trading_system_engine::trading::snapshot::REGISTRY_SNAPSHOT_VERSION;
use trading_system_engine::trading::snapshot::registry as registry_snapshot;
use trading_system_engine::trading::snapshot::snapshot_file;

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

fn run(registry: &mut InstrumentRegistry, commands: &[Command]) {
    for command in commands {
        if command.combo_id != 0 {
            registry.execute_combo(*command).unwrap();
        } else {
            registry.execute(*command).unwrap();
        }
    }
}

fn script() -> Vec<Command> {
    vec![
        leg_order(1, 100, Side::Sell, 50_000, 10),
        leg_order(2, 101, Side::Buy, 40_000, 10),
        leg_order(1, 102, Side::Buy, 44_000, 10),
        leg_order(2, 103, Side::Sell, 46_000, 10),
        // Implied-in taker: no resting combo order, two leg orders cross.
        combo_order(2_001, 1, 11, Side::Buy, 12_000, 1),
        // Resting sell spread that the next buy taker partly consumes.
        combo_order(2_002, 2, 22, Side::Sell, 12_000, 2),
        // Direct fill plus implied-in residual.
        combo_order(2_003, 3, 33, Side::Buy, 12_000, 3),
        // Resting buy spread at a price the following sell crosses.
        combo_order(2_004, 4, 44, Side::Buy, 90, 4),
        combo_order(2_005, 5, 55, Side::Sell, 95, 2),
        cancel_combo(4, 4),
        leg_order(1, 105, Side::Buy, 45_000, 5),
        combo_order(2_006, 6, 66, Side::Sell, 5_000, 1),
    ]
}

fn decoded(snapshot: &RegistrySnapshot) -> RegistrySnapshot {
    let json = snapshot_file::encode(snapshot, REGISTRY_SNAPSHOT_VERSION).unwrap();
    snapshot_file::decode(&json, REGISTRY_SNAPSHOT_VERSION).unwrap()
}

#[test]
fn snapshot_roundtrip_preserves_combo_and_leg_state() {
    let mut registry = registry_with_pair();
    registry.set_prior_settlement(1, 45_000).unwrap();
    registry.set_prior_settlement(2, 40_000).unwrap();
    // An ordinary leg book on each shared Contract.
    rest_leg(&mut registry, leg_order(1, 100, Side::Buy, 44_000, 10));
    rest_leg(&mut registry, leg_order(1, 101, Side::Sell, 51_000, 10));
    rest_leg(&mut registry, leg_order(2, 102, Side::Buy, 39_000, 10));
    rest_leg(&mut registry, leg_order(2, 103, Side::Sell, 46_000, 10));

    // One direct fill seeds the combo last trade price and actual leg margin.
    registry
        .execute_combo(combo_order(1_001, 1, 11, Side::Sell, 100, 1))
        .unwrap();
    registry
        .execute_combo(combo_order(1_002, 2, 22, Side::Buy, 100, 1))
        .unwrap();

    // Resting base Combo Orders on both sides of a non-crossing spread.
    let resting_buy = rested_order_id(
        &registry
            .execute_combo(combo_order(1_003, 3, 33, Side::Buy, 90, 3))
            .unwrap(),
    );
    registry
        .execute_combo(combo_order(1_004, 4, 44, Side::Sell, 30_000, 3))
        .unwrap();

    let snapshot = registry.snapshot();
    let restored_snapshot = decoded(&snapshot);
    let mut restored = InstrumentRegistry::new();
    restored.restore_snapshot(restored_snapshot).unwrap();

    assert_eq!(restored.snapshot(), snapshot);
    assert_eq!(
        restored.combo_book(10).unwrap().last_trade_price(),
        Some(100)
    );
    assert_eq!(restored.pair_count(), 1);

    // Restored margin is still attached to its Combo Order: cancel releases it.
    assert_eq!(
        registry.leg_margin(3, 1).frozen,
        restored.leg_margin(3, 1).frozen
    );
    assert!(restored.leg_margin(3, 1).frozen > 0);
    let canceled = restored
        .execute_combo(cancel_combo(3, resting_buy))
        .unwrap();
    assert!(matches!(canceled.as_slice(), [ComboEvent::Canceled { .. }]));
    assert_eq!(restored.leg_margin(3, 1).frozen, 0);

    // Restored leg liquidity still supports implied-in with a continuing
    // registry-owned private sequence.
    let filtered = restored
        .execute_combo(combo_order(1_005, 5, 55, Side::Buy, 12_000, 1))
        .unwrap();
    assert!(
        matches!(filtered.as_slice(), [ComboEvent::ImpliedFilled { fill }] if fill.quantity == 1),
        "{filtered:?}"
    );
    assert_eq!(
        registry.replay_combo_private_after(0).len() + 1,
        restored.replay_combo_private_after(0).len()
    );
    assert_eq!(
        restored
            .replay_combo_private_after(0)
            .last()
            .map(|fill| fill.seq),
        Some(3)
    );
}

#[test]
fn combo_private_fills_replay_after_last_private_seq() {
    let mut registry = registry_with_pair();
    registry.set_prior_settlement(1, 45_000).unwrap();
    registry.set_prior_settlement(2, 40_000).unwrap();
    run(&mut registry, &script());

    assert_eq!(registry.next_combo_private_seq(), 5);
    let all = registry.replay_combo_private_after(0);
    assert_eq!(
        all.iter().map(|fill| fill.seq).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert_eq!(
        registry
            .replay_combo_private_after(2)
            .iter()
            .map(|fill| fill.seq)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert!(registry.replay_combo_private_after(4).is_empty());

    // The snapshot carries the undrained buffer and the sequence counter, so a
    // restarted registry continues the same private stream.
    let snapshot = registry.snapshot();
    let mut restored = InstrumentRegistry::new();
    restored.restore_snapshot(decoded(&snapshot)).unwrap();
    assert_eq!(restored.next_combo_private_seq(), 5);
    assert_eq!(
        restored
            .replay_combo_private_after(3)
            .iter()
            .map(|fill| fill.seq)
            .collect::<Vec<_>>(),
        vec![4]
    );

    let filled = restored
        .execute_combo(combo_order(3_001, 7, 77, Side::Buy, 5_000, 1))
        .unwrap();
    assert!(
        matches!(filled.as_slice(), [ComboEvent::Filled { fill }] if fill.quantity == 1),
        "{filled:?}"
    );
    assert_eq!(
        restored
            .replay_combo_private_after(4)
            .iter()
            .map(|fill| fill.seq)
            .collect::<Vec<_>>(),
        vec![5, 6]
    );

    // Replay is scoped to the registry's undrained buffer, not a full log.
    assert_eq!(restored.drain_combo_private_events().len(), 6);
    assert!(restored.replay_combo_private_after(0).is_empty());
}

#[test]
fn partial_or_incompatible_snapshots_are_rejected() {
    let mut source = registry_with_pair();
    source.set_prior_settlement(1, 45_000).unwrap();
    source.set_prior_settlement(2, 40_000).unwrap();
    run(&mut source, &script());
    let snapshot = source.snapshot();
    assert!(!snapshot.combo_private_events.is_empty());
    let max_assigned = snapshot
        .combo_books
        .iter()
        .flat_map(|book| book.bids.iter().chain(book.asks.iter()))
        .map(|order| order.order_id)
        .chain(
            snapshot
                .combo_private_events
                .iter()
                .map(|fill| fill.order_id),
        )
        .max()
        .unwrap();

    let mut target = registry_with_pair();
    target.set_prior_settlement(1, 1).unwrap();
    let untouched = target.snapshot();

    let mut value = serde_json::to_value(&snapshot).unwrap();
    value["combo_books"] = serde_json::json!([]);
    let error = target
        .restore_snapshot(serde_json::from_value(value).unwrap())
        .unwrap_err();
    assert_eq!(
        error,
        RegistrySnapshotError::MissingComboBook { pair_id: 10 }
    );

    let mut value = serde_json::to_value(&snapshot).unwrap();
    value["next_combo_order_id"] = serde_json::json!(max_assigned);
    let error = target
        .restore_snapshot(serde_json::from_value(value).unwrap())
        .unwrap_err();
    assert_eq!(
        error,
        RegistrySnapshotError::InvalidNextComboOrderId {
            next_combo_order_id: max_assigned,
            max_assigned_order_id: max_assigned,
        }
    );

    let mut value = serde_json::to_value(&snapshot).unwrap();
    value["margin"]["reservations"] = serde_json::json!([]);
    for margin in value["margin"]["leg_margins"].as_array_mut().unwrap() {
        margin["frozen"] = serde_json::json!(0);
    }
    let error = target
        .restore_snapshot(serde_json::from_value(value).unwrap())
        .unwrap_err();
    assert!(matches!(
        error,
        RegistrySnapshotError::MissingMarginReservation { .. }
    ));

    let mut value = serde_json::to_value(&snapshot).unwrap();
    value["combo_private_events"][0]["pair_id"] = serde_json::json!(99);
    let error = target
        .restore_snapshot(serde_json::from_value(value).unwrap())
        .unwrap_err();
    assert_eq!(
        error,
        RegistrySnapshotError::UnknownComboPrivatePair {
            seq: snapshot.combo_private_events[0].seq,
            pair_id: 99,
        }
    );

    let mut value = serde_json::to_value(&snapshot).unwrap();
    value["prior_settlements"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!([99, 1_000]));
    let error = target
        .restore_snapshot(serde_json::from_value(value).unwrap())
        .unwrap_err();
    assert_eq!(
        error,
        RegistrySnapshotError::UnknownPriorSettlementInstrument { instrument_id: 99 }
    );

    // Dropping the venues settlement prices is partial: the frozen estimate
    // can no longer be reproduced.
    let mut value = serde_json::to_value(&snapshot).unwrap();
    value["prior_settlements"] = serde_json::json!([]);
    let error = target
        .restore_snapshot(serde_json::from_value(value).unwrap())
        .unwrap_err();
    assert!(matches!(error, RegistrySnapshotError::Margin(_)));

    let mut value = serde_json::to_value(&snapshot).unwrap();
    value["combo_books"][0]["asks"][0]["pair_id"] = serde_json::json!(11);
    let error = target
        .restore_snapshot(serde_json::from_value(value).unwrap())
        .unwrap_err();
    assert!(matches!(
        error,
        RegistrySnapshotError::ComboBookRestore { pair_id: 10, .. }
    ));

    let mut value = serde_json::to_value(&snapshot).unwrap();
    let frozen = value["margin"]["leg_margins"][0]["frozen"]
        .as_u64()
        .unwrap();
    value["margin"]["leg_margins"][0]["frozen"] = serde_json::json!(frozen + 1);
    let error = target
        .restore_snapshot(serde_json::from_value(value).unwrap())
        .unwrap_err();
    assert!(matches!(error, RegistrySnapshotError::Margin(_)));

    let mut value = serde_json::to_value(&snapshot).unwrap();
    let instrument = value["instruments"][0].clone();
    value["instruments"]
        .as_array_mut()
        .unwrap()
        .push(instrument);
    let error = target
        .restore_snapshot(serde_json::from_value(value).unwrap())
        .unwrap_err();
    assert!(matches!(
        error,
        RegistrySnapshotError::DuplicateInstrument { .. }
    ));

    assert_eq!(
        target.snapshot(),
        untouched,
        "a rejected snapshot must not mutate the registry"
    );
}

#[test]
fn restored_matching_is_deterministic_on_the_same_command_sequence() {
    let commands = script();
    let split = 6;

    let mut reference = registry_with_pair();
    reference.set_prior_settlement(1, 45_000).unwrap();
    reference.set_prior_settlement(2, 40_000).unwrap();
    run(&mut reference, &commands[..split]);

    let snapshot = reference.snapshot();
    let mut restored = InstrumentRegistry::new();
    restored.restore_snapshot(decoded(&snapshot)).unwrap();
    assert_eq!(restored.snapshot(), snapshot);

    run(&mut reference, &commands[split..]);
    run(&mut restored, &commands[split..]);

    assert_eq!(restored.snapshot(), reference.snapshot());
    assert_eq!(restored.snapshot().next_combo_order_id, 7);
    assert_eq!(
        restored
            .replay_combo_private_after(0)
            .iter()
            .map(|fill| fill.seq)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
}

#[test]
fn snapshot_file_replay_matches_continuous_execution() {
    let commands = script();
    let split = 6;
    let path = std::env::temp_dir().join(format!(
        "trading-combo-registry-snapshot-{}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);

    let mut reference = registry_with_pair();
    reference.set_prior_settlement(1, 45_000).unwrap();
    reference.set_prior_settlement(2, 40_000).unwrap();
    run(&mut reference, &commands[..split]);
    registry_snapshot::save(&path, &reference).unwrap();

    let replayed =
        registry_snapshot::replay_from_snapshot(&path, commands[split..].iter().copied()).unwrap();
    run(&mut reference, &commands[split..]);
    assert_eq!(replayed.snapshot(), reference.snapshot());

    // Versioned envelope: an unknown schema version is rejected.
    let json = std::fs::read_to_string(&path).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    value["version"] = serde_json::json!(REGISTRY_SNAPSHOT_VERSION + 1);
    std::fs::write(&path, value.to_string()).unwrap();
    assert!(registry_snapshot::load(&path).is_err());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn snapshot_restores_the_one_venue_public_sequence() {
    let mut registry = registry_with_pair();
    registry.set_prior_settlement(1, 45_000).unwrap();
    registry.set_prior_settlement(2, 40_000).unwrap();

    rest_leg(&mut registry, leg_order(1, 100, Side::Sell, 50_000, 5));
    let first = registry.drain_public_events();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].seq, 1);

    let snapshot = registry.snapshot();
    assert_eq!(snapshot.next_public_seq, 2);
    let mut restored = InstrumentRegistry::new();
    let mut value = serde_json::to_value(&snapshot).unwrap();
    value["next_public_seq"] = serde_json::json!(0);
    let error = restored
        .restore_snapshot(serde_json::from_value(value).unwrap())
        .unwrap_err();
    assert_eq!(
        error,
        RegistrySnapshotError::InvalidNextPublicSeq { next_public_seq: 0 }
    );

    restored.restore_snapshot(decoded(&snapshot)).unwrap();
    assert_eq!(restored.next_public_seq(), 2);

    // Continue both streams with the same mixed leg/combo commands. The
    // restored venue must not repeat the sequence already delivered.
    for candidate in [&mut registry, &mut restored] {
        rest_leg(candidate, leg_order(2, 101, Side::Buy, 40_000, 5));
        candidate
            .execute_combo(combo_order(1_001, 1, 11, Side::Sell, 100, 1))
            .unwrap();
        candidate
            .execute_combo(combo_order(1_002, 2, 22, Side::Buy, 100, 1))
            .unwrap();
    }
    let expected = registry.drain_public_events();
    let actual = restored.drain_public_events();
    assert_eq!(actual, expected);
    assert_eq!(
        expected.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(restored.next_public_seq(), 4);
    assert_eq!(restored.snapshot(), registry.snapshot());
}
