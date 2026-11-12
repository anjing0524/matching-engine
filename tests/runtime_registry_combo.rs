//! Single-process registry runtime: command routing, private projection,
//! reconnect replay, and snapshot restore through the composed tick.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;
use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::trading::core::plugins::NoPlugins;
use trading_system_engine::trading::core::private_events::{PrivateEffect, PrivateStatus};
use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::core::{ComboPair, ComboPairMargin, InstrumentRegistry};
use trading_system_engine::trading::gateway::runtime::GatewayRuntime;
use trading_system_engine::trading::gateway::server::start;
use trading_system_engine::trading::gateway::{GATEWAY_FRAME_SIZE, GatewayFrame};
use trading_system_engine::trading::runtime::{RuntimeOptions, TradingRuntime, TradingSnapshot};
use trading_system_engine::trading::transport::create;
use trading_system_engine::trading::transport::middleware::MiddlewareSequencer;
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
        "trading-runtime-registry-{name}-{}",
        std::process::id()
    ))
}

fn combo_order(
    user_id: u32,
    client_order_id: u64,
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

fn runtime_with_registry(
    registry: InstrumentRegistry,
    path: &std::path::Path,
) -> (TradingRuntime<NoPlugins>, String) {
    let _ = std::fs::remove_file(path);
    let (producer, consumer) = create(path, 64).unwrap();
    // No combo transport: the gateway writes combo commands to the same
    // command ring, and the registry routes them by `combo_id`.
    let middleware = MiddlewareSequencer::new(producer);
    let (server, events) = start("127.0.0.1:0").unwrap();
    let address = server.addr();
    let gateway = GatewayRuntime::new(middleware, events);
    let runner = RegistryRunner::new(registry, consumer, NoPlugins);
    let runtime = TradingRuntime::from_registry(gateway, runner, server)
        .with_options(RuntimeOptions::default());
    (runtime, address.to_string())
}

fn login(stream: &mut TcpStream, user_id: u32, last_private_seq: u64) {
    stream
        .write_all(
            &GatewayFrame::Login {
                member_id: 10,
                auth_token: 0,
                user_id,
                seat_id: 3,
                session_epoch: 1,
                last_private_seq,
            }
            .encode(),
        )
        .unwrap();
}

fn read_combo_private(
    runtime: &mut TradingRuntime<NoPlugins>,
    stream: &mut TcpStream,
) -> trading_system_engine::trading::core::combo_book::ComboPrivateFill {
    for _ in 0..500 {
        runtime.tick();
        let mut frame = [0u8; GATEWAY_FRAME_SIZE];
        if stream.read_exact(&mut frame).is_ok()
            && let GatewayFrame::ComboPrivate(fill) = GatewayFrame::decode(&frame).unwrap()
        {
            return fill;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("combo private fill did not reach the member");
}

fn leg_order(
    user_id: u32,
    client_order_id: u64,
    side: Side,
    instrument_id: u32,
    price_ticks: i64,
) -> Command {
    let mut command = combo_order(user_id, client_order_id, 1, side, price_ticks, 1);
    command.combo_id = 0;
    command.instrument_id = instrument_id;
    command
}

fn read_private_effects(
    runtime: &mut TradingRuntime<NoPlugins>,
    stream: &mut TcpStream,
    count: usize,
) -> Vec<PrivateEffect> {
    let mut effects = Vec::new();
    for _ in 0..500 {
        if effects.len() == count {
            return effects;
        }
        runtime.tick();
        let mut frame = [0u8; GATEWAY_FRAME_SIZE];
        if stream.read_exact(&mut frame).is_err() {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        match GatewayFrame::decode(&frame).unwrap() {
            GatewayFrame::PrivateEvent(event) => effects.push(PrivateEffect::Core(event)),
            GatewayFrame::ComboPrivate(fill) => effects.push(PrivateEffect::Combo(fill)),
            other => panic!("unexpected private frame: {other:?}"),
        }
    }
    panic!("only {} of {count} private effects arrived", effects.len());
}

#[test]
fn combo_command_reaches_registry_and_private_fill_through_runtime() {
    let path = temp_path("fill-ring");
    let mut registry = registry_with_pair();
    // A resting sell spread owned by user 1; user 2 will take it.
    registry
        .execute_combo(combo_order(1, 1, 1, Side::Sell, 100, 1))
        .unwrap();
    let (mut runtime, address) = runtime_with_registry(registry, &path);

    let mut stream = TcpStream::connect(&address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    login(&mut stream, 2, 0);
    stream
        .write_all(&GatewayFrame::Command(combo_order(2, 2, 1, Side::Buy, 100, 1)).encode())
        .unwrap();

    let fill = read_combo_private(&mut runtime, &mut stream);
    assert_eq!(fill.pair_id, 10);
    assert_eq!(fill.user_id, 2);
    assert_eq!(fill.side, Side::Buy);
    assert_eq!(fill.quantity, 1);
    assert_eq!(fill.spread_price, 100);
    assert!(fill.seq > 0);

    // The runtime is the registry path, not the legacy combo engine.
    let snapshot = runtime.snapshot();
    assert!(snapshot.registry.is_some());
    assert!(snapshot.combo.is_none());
    assert!(snapshot.core.is_none());
    let _ = std::fs::remove_file(path);
}

#[test]
fn reconnect_replays_combo_private_fill_after_last_private_seq() {
    let path = temp_path("replay-ring");
    let mut registry = registry_with_pair();
    registry
        .execute_combo(combo_order(1, 1, 1, Side::Sell, 100, 1))
        .unwrap();
    let (mut runtime, address) = runtime_with_registry(registry, &path);

    let mut first = TcpStream::connect(&address).unwrap();
    first
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    login(&mut first, 2, 0);
    first
        .write_all(&GatewayFrame::Command(combo_order(2, 2, 1, Side::Buy, 100, 1)).encode())
        .unwrap();
    let fill = read_combo_private(&mut runtime, &mut first);
    drop(first);
    for _ in 0..20 {
        runtime.tick();
        std::thread::sleep(Duration::from_millis(1));
    }

    let mut second = TcpStream::connect(&address).unwrap();
    second
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    login(&mut second, 2, fill.seq - 1);

    let mut replayed = None;
    for _ in 0..500 {
        runtime.tick();
        let mut frame = [0u8; GATEWAY_FRAME_SIZE];
        if second.read_exact(&mut frame).is_ok()
            && let GatewayFrame::ComboPrivate(replay) = GatewayFrame::decode(&frame).unwrap()
        {
            replayed = Some(replay);
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let replayed = replayed.expect("combo private replay");
    assert_eq!(replayed.seq, fill.seq);
    assert_eq!(replayed.pair_id, fill.pair_id);
    assert_eq!(replayed.order_id, fill.order_id);
    assert_eq!(replayed.spread_price, fill.spread_price);
    let _ = std::fs::remove_file(path);
}

#[test]
fn runtime_snapshot_roundtrip_restores_registry_combo_state() {
    let path = temp_path("snapshot-ring");
    let snapshot_path = temp_path("snapshot.json");
    let mut registry = registry_with_pair();
    let resting_order_id = match registry
        .execute_combo(combo_order(1, 1, 1, Side::Buy, 50, 2))
        .unwrap()
        .as_slice()
    {
        [trading_system_engine::trading::core::ComboEvent::Rested { order }] => order.order_id,
        other => panic!("expected a resting combo order, got {other:?}"),
    };
    let (runtime, _address) = runtime_with_registry(registry, &path);
    let snapshot = runtime.snapshot();
    let registry_snapshot = snapshot.registry.clone().expect("registry snapshot");
    assert_eq!(registry_snapshot.next_combo_order_id, resting_order_id + 1);
    snapshot.save(&snapshot_path).unwrap();
    let loaded = TradingSnapshot::load(&snapshot_path).unwrap();
    assert_eq!(loaded.registry.as_ref(), Some(&registry_snapshot));

    let path = temp_path("snapshot-restored-ring");
    let (mut restored, address) = runtime_with_registry(registry_with_pair(), &path);
    restored.restore_snapshot(loaded).unwrap();
    assert_eq!(
        restored.snapshot().registry.as_ref(),
        Some(&registry_snapshot)
    );

    // The restored resting Combo Order still matches: a sell taker crosses it.
    let mut stream = TcpStream::connect(&address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    login(&mut stream, 2, 0);
    stream
        .write_all(&GatewayFrame::Command(combo_order(2, 2, 1, Side::Sell, 50, 2)).encode())
        .unwrap();
    let fill = read_combo_private(&mut restored, &mut stream);
    assert_eq!(fill.quantity, 2);
    assert_eq!(fill.spread_price, 50);
    assert_eq!(fill.user_id, 2);
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(snapshot_path);
    let _ = std::fs::remove_file(temp_path("snapshot-ring"));
}

#[test]
fn restart_and_reconnect_preserve_combo_then_core_private_order() {
    let path = temp_path("restart-order-ring");
    let mut registry = registry_with_pair();
    // A resting sell spread owned by user 1; user 2 will take it.
    registry
        .execute_combo(combo_order(1, 1, 1, Side::Sell, 100, 1))
        .unwrap();
    let (mut runtime, address) = runtime_with_registry(registry, &path);

    let mut member = TcpStream::connect(&address).unwrap();
    member
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    login(&mut member, 2, 0);
    member
        .write_all(&GatewayFrame::Command(combo_order(2, 2, 1, Side::Buy, 100, 1)).encode())
        .unwrap();
    let combo_fill = read_combo_private(&mut runtime, &mut member);
    assert_eq!(combo_fill.seq, 2);

    // Rest an ordinary leg order owned by user 2, so a later leg trade gives
    // this user a core private effect after the restart.
    member
        .write_all(&GatewayFrame::Command(leg_order(2, 3, Side::Sell, 1, 50_000)).encode())
        .unwrap();
    let accepted = read_private_effects(&mut runtime, &mut member, 1);
    let PrivateEffect::Core(accepted) = &accepted[0] else {
        panic!("expected a core private effect");
    };
    assert_eq!(accepted.account_id, 2);
    assert_eq!(accepted.seq, 3);

    let snapshot = runtime.snapshot();

    // Restart into a fresh runtime that restores the same venue state.
    let restored_path = temp_path("restart-order-restored-ring");
    let (mut restored, restored_address) =
        runtime_with_registry(registry_with_pair(), &restored_path);
    restored.restore_snapshot(snapshot).unwrap();

    // User 2 reconnects without acknowledging the pre-restart effects. Replay
    // must preserve the single sequence across core and combo producers.
    let mut member = TcpStream::connect(&restored_address).unwrap();
    member
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    login(&mut member, 2, 0);
    let replay = read_private_effects(&mut restored, &mut member, 3);
    assert_eq!(
        replay.iter().map(PrivateEffect::seq).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(matches!(replay[0], PrivateEffect::Core(_)));
    assert!(matches!(replay[1], PrivateEffect::Combo(_)));
    assert!(matches!(replay[2], PrivateEffect::Core(_)));

    // A new core fill after restart continues the same per-user sequence
    // instead of restarting from 1.
    let mut taker = TcpStream::connect(&restored_address).unwrap();
    login(&mut taker, 3, 0);
    taker
        .write_all(&GatewayFrame::Command(leg_order(3, 4, Side::Buy, 1, 50_000)).encode())
        .unwrap();
    let live = read_private_effects(&mut restored, &mut member, 1);
    let PrivateEffect::Core(event) = &live[0] else {
        panic!("expected a core fill for the resting leg order");
    };
    assert_eq!(event.account_id, 2);
    assert_eq!(event.seq, 4);
    assert_ne!(event.status, PrivateStatus::Accepted);

    // A later reconnect replays the merged stream in sequence order.
    let mut again = TcpStream::connect(&restored_address).unwrap();
    again
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    login(&mut again, 2, 0);
    let replay = read_private_effects(&mut restored, &mut again, 4);
    assert_eq!(
        replay.iter().map(PrivateEffect::seq).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert!(matches!(replay[1], PrivateEffect::Combo(_)));

    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(restored_path);
}

#[test]
fn same_tick_combo_then_leg_commands_keep_producer_order() {
    use trading_system_engine::trading::core::private_events::PrivateEffect;
    let path = temp_path("same-tick-order");
    let mut registry = registry_with_pair();
    registry
        .execute_combo(combo_order(1, 1, 1, Side::Sell, 100, 1))
        .unwrap();
    let (mut runtime, address) = runtime_with_registry(registry, &path);
    let mut stream = TcpStream::connect(&address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    login(&mut stream, 2, 0);
    stream
        .write_all(&GatewayFrame::Command(combo_order(2, 2, 1, Side::Buy, 100, 1)).encode())
        .unwrap();
    stream
        .write_all(&GatewayFrame::Command(leg_order(2, 3, Side::Buy, 1, 40_000)).encode())
        .unwrap();

    let effects = read_private_effects(&mut runtime, &mut stream, 3);
    assert!(matches!(effects[0], PrivateEffect::Core(_)));
    assert!(matches!(effects[1], PrivateEffect::Combo(_)));
    assert!(matches!(effects[2], PrivateEffect::Core(_)));
    assert_eq!(effects[0].seq(), 1);
    assert_eq!(effects[1].seq(), 2);
    assert_eq!(effects[2].seq(), 3);
    let _ = std::fs::remove_file(path);
}
