//! Multi-gateway TCP + public dispatch load test.
//!
//! Ignored by default; run with:
//! `cargo test --release --test trading_tcp_load -- --ignored --nocapture`

use std::io::Write;
use std::net::TcpStream;
use std::time::{Duration, Instant};
use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::shared::SymbolId;
use trading_system_engine::trading::core::TradingCore;
use trading_system_engine::trading::gateway::GatewayFrame;
use trading_system_engine::trading::gateway_runtime::GatewayRuntime;
use trading_system_engine::trading::gateway_server::start;
use trading_system_engine::trading::market_data::{MarketDataPublisher, MarketDataSubscriber};
use trading_system_engine::trading::middleware::MiddlewareSequencer;
use trading_system_engine::trading::plugins::NoPlugins;
use trading_system_engine::trading::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::runner::CoreRunner;
use trading_system_engine::trading::runtime::{RuntimeOptions, TradingRuntime};
use trading_system_engine::trading::transport::create;

fn command(id: u64, user: u32) -> Command {
    let mut command = Command::zeroed();
    command.client_order_id = id;
    command.user_id = user;
    command.command_type = CommandType::NewOrder;
    command.price_type = PriceType::Limit;
    command.time_condition = TimeCondition::Gfd;
    command.volume_condition = VolumeCondition::Any;
    command.side = Side::Buy;
    command.offset = Offset::Open;
    command.hedge = Hedge::Spec;
    command.price_ticks = 50_000 + (id % 100) as i64;
    command.quantity = 1;
    command.trigger_type = TriggerType::None;
    command
}

#[test]
#[ignore = "load test; run explicitly"]
fn multi_gateway_tcp_fanout() {
    let path =
        std::env::temp_dir().join(format!("trading-system-engine-tcp-load-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let (producer, consumer) = create(&path, 1 << 17).unwrap();
    let middleware = MiddlewareSequencer::new(producer);
    let (server, events) = start("127.0.0.1:0").unwrap();
    let addr = server.addr();
    let gateway = GatewayRuntime::new(middleware, events);
    let spec = ContractSpec::new("BTCUSDT", 1, 1, 1_000_000);
    let core = TradingCore::new(spec, SymbolId::intern("BTCUSDT"));
    let runner = CoreRunner::new(core, consumer, NoPlugins);
    let subscriber = MarketDataSubscriber::bind("127.0.0.1:0").unwrap();
    let publisher =
        MarketDataPublisher::bind("127.0.0.1:0", subscriber.local_addr().unwrap()).unwrap();
    let clients = std::env::var("TRADING_LOAD_CLIENTS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(8);
    let per_client = std::env::var("TRADING_LOAD_ORDERS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(500);
    let total_expected = clients * per_client;
    let (received_tx, received_rx) = std::sync::mpsc::channel();
    let drain_thread = std::thread::spawn(move || {
        let mut received = 0u64;
        while received < total_expected {
            match subscriber.recv() {
                Ok(_) => received += 1,
                Err(_) => break,
            }
        }
        let _ = received_tx.send(received);
    });
    let mut runtime = TradingRuntime::new(gateway, runner, server).with_options(RuntimeOptions {
        market_data: Some(publisher),
        ..Default::default()
    });

    let total = clients * per_client;
    let mut streams = Vec::new();
    for client in 0..clients {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .write_all(
                &GatewayFrame::Login {
                    member_id: client as u32 + 1,
                    auth_token: 0,
                    user_id: client as u32 + 1,
                    seat_id: 1,
                    session_epoch: 1,
                    last_private_seq: 0,
                }
                .encode(),
            )
            .unwrap();
        for seq in 0..per_client {
            let id = client * per_client + seq + 1;
            stream
                .write_all(&GatewayFrame::Command(command(id, client as u32 + 1)).encode())
                .unwrap();
        }
        streams.push(stream);
    }

    let start_time = Instant::now();
    while runtime.public_sent() < total {
        runtime.tick();
        std::thread::sleep(Duration::from_micros(200));
    }
    let elapsed = start_time.elapsed();
    let rate = total as f64 / elapsed.as_secs_f64();
    println!("{total} gateway orders in {elapsed:?} ({rate:.0} order/s)");

    // The consumer thread drains concurrently; UDP is still best-effort, so
    // require a high-but-not-perfect delivery ratio on shared CI runners.
    let received = received_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or(0);
    let _ = drain_thread.join();
    println!("public packets received: {received}/{total}");
    assert!(
        received * 10 >= total * 9,
        "public delivery below 90%: {received}/{total}"
    );
    drop(streams);
    let _ = std::fs::remove_file(&path);
}
