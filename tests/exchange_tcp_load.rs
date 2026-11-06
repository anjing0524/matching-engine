//! Multi-gateway TCP + public dispatch load test.
//!
//! Ignored by default; run with:
//! `cargo test --release --test exchange_tcp_load -- --ignored --nocapture`

use matching_engine::domain::orderbook::ContractSpec;
use matching_engine::exchange::core::ExchangeCore;
use matching_engine::exchange::exchange_runtime::ExchangeRuntime;
use matching_engine::exchange::gateway::GatewayFrame;
use matching_engine::exchange::gateway_runtime::GatewayRuntime;
use matching_engine::exchange::gateway_server::start;
use matching_engine::exchange::market_data::{MarketDataPublisher, MarketDataSubscriber};
use matching_engine::exchange::middleware::MiddlewareSequencer;
use matching_engine::exchange::plugins::NoPlugins;
use matching_engine::exchange::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use matching_engine::exchange::runner::CoreRunner;
use matching_engine::exchange::transport::create;
use matching_engine::shared::SymbolId;
use std::io::Write;
use std::net::TcpStream;
use std::time::{Duration, Instant};

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
    command.price_ticks = 50_000 + id as i64;
    command.quantity = 1;
    command.trigger_type = TriggerType::None;
    command
}

#[test]
#[ignore = "load test; run explicitly"]
fn multi_gateway_tcp_fanout() {
    let path =
        std::env::temp_dir().join(format!("matching-engine-tcp-load-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let (producer, consumer) = create(&path, 1 << 14).unwrap();
    let middleware = MiddlewareSequencer::new(producer);
    let (server, events) = start("127.0.0.1:0").unwrap();
    let addr = server.addr();
    let gateway = GatewayRuntime::new(middleware, events);
    let spec = ContractSpec::new("BTCUSDT", 1, 1, 1_000_000);
    let core = ExchangeCore::new(spec, SymbolId::intern("BTCUSDT"));
    let runner = CoreRunner::new(core, consumer, NoPlugins);
    let subscriber = MarketDataSubscriber::bind("127.0.0.1:0").unwrap();
    let publisher =
        MarketDataPublisher::bind("127.0.0.1:0", subscriber.local_addr().unwrap()).unwrap();
    let mut runtime = ExchangeRuntime::new(gateway, runner, server).with_market_data(publisher);

    let clients = 8u64;
    let per_client = 500u64;
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
    for _ in 0..total {
        subscriber.recv().unwrap();
    }
    drop(streams);
    let _ = std::fs::remove_file(&path);
}
