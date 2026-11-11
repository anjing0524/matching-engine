//! Single-process registry runtime: command routing, private projection,
//! reconnect replay, and snapshot restore through the composed tick.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;
use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::trading::core::plugins::NoPlugins;
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
