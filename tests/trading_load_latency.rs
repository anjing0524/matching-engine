//! Multi-member throughput + end-to-end latency profile.
//!
//! The sender threads push commands as fast as possible; the main thread drives
//! the runtime. Each reader thread timestamps the private response.
//!
//! Run:
//! `LATENCY_MEMBERS=8 LATENCY_ORDERS=12500 cargo test --release --test trading_load_latency -- --ignored --nocapture`

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::shared::SymbolId;
use trading_system_engine::trading::core::TradingCore;
use trading_system_engine::trading::core::plugins::NoPlugins;
use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::gateway::GatewayFrame;
use trading_system_engine::trading::gateway::runtime::GatewayRuntime;
use trading_system_engine::trading::gateway::server::start;
use trading_system_engine::trading::runtime::{RuntimeOptions, TradingRuntime};
use trading_system_engine::trading::transport::create;
use trading_system_engine::trading::transport::middleware::MiddlewareSequencer;
use trading_system_engine::trading::transport::runner::CoreRunner;

fn command(id: u64, user_id: u32, price: i64, side: Side) -> Command {
    let mut command = Command::zeroed();
    command.client_order_id = id;
    command.user_id = user_id;
    command.command_type = CommandType::NewOrder;
    command.price_type = PriceType::Limit;
    command.time_condition = TimeCondition::Gfd;
    command.volume_condition = VolumeCondition::Any;
    command.side = side;
    command.offset = Offset::Open;
    command.hedge = Hedge::Spec;
    command.price_ticks = price;
    command.quantity = 1;
    command.trigger_type = TriggerType::None;
    command
}

struct Shared {
    start: Instant,
    sent: Vec<AtomicU64>,
    received: AtomicUsize,
    total: usize,
}

#[test]
#[ignore = "throughput + latency profile; run explicitly"]
fn multi_member_load_latency_profile() {
    let members: usize = std::env::var("LATENCY_MEMBERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(8);
    let per_member: usize = std::env::var("LATENCY_ORDERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(12_500);
    let total = members * per_member;
    let path = std::env::temp_dir().join(format!(
        "trading-system-engine-load-latency-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let (producer, consumer) = create(&path, 1 << 20).unwrap();
    let spec = ContractSpec::new("LL", 1, 40_000, 60_000);
    let core = TradingCore::new(spec, SymbolId::intern("LL"));
    let runner = CoreRunner::new(core, consumer, NoPlugins);
    let (server, events) = start("127.0.0.1:0").unwrap();
    let addr = server.addr();
    let middleware = MiddlewareSequencer::new(producer);
    let gateway = GatewayRuntime::new(middleware, events);
    let mut runtime =
        TradingRuntime::new(gateway, runner, server).with_options(RuntimeOptions::default());

    let mut writers = Vec::with_capacity(members);
    let mut readers = Vec::with_capacity(members);
    for member in 0..members {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .write_all(
                &GatewayFrame::Login {
                    member_id: member as u32 + 1,
                    auth_token: 0,
                    user_id: member as u32 + 1,
                    seat_id: 1,
                    session_epoch: 1,
                    last_private_seq: 0,
                }
                .encode(),
            )
            .unwrap();
        writers.push(stream);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while runtime.metrics().active_sessions < members as u64 {
        runtime.tick();
        assert!(Instant::now() < deadline, "members did not login");
        std::thread::sleep(Duration::from_millis(1));
    }

    let shared = Arc::new(Shared {
        start: Instant::now(),
        sent: (0..members)
            .flat_map(|_| (0..per_member).map(|_| AtomicU64::new(u64::MAX)))
            .collect(),
        received: AtomicUsize::new(0),
        total,
    });

    for member in 0..members {
        let mut reader_stream = writers[member].try_clone().unwrap();
        reader_stream.set_nodelay(true).unwrap();
        let shared = Arc::clone(&shared);
        readers.push(std::thread::spawn(move || {
            let mut samples = Vec::with_capacity(per_member);
            let mut buffer = [0u8; 264];
            let base = member * per_member;
            while samples.len() < per_member {
                reader_stream.read_exact(&mut buffer).unwrap();
                let frame = GatewayFrame::decode(&buffer).unwrap();
                if let GatewayFrame::PrivateEvent(event) = frame {
                    let index = usize::try_from(event.client_order_id)
                        .unwrap()
                        .saturating_sub(base + 1);
                    if index < per_member {
                        let sent = shared.sent[base + index].load(Ordering::Relaxed);
                        if sent != u64::MAX {
                            let now = shared.start.elapsed().as_nanos() as u64;
                            samples.push(now.saturating_sub(sent));
                            shared.received.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
            samples
        }));
    }

    let mut senders = Vec::with_capacity(members);
    for (member, mut stream) in writers.into_iter().enumerate() {
        let shared = Arc::clone(&shared);
        senders.push(std::thread::spawn(move || {
            let base = member * per_member;
            let price = 50_000 + member as i64;
            for seq in 0..per_member {
                let side = if seq % 2 == 0 { Side::Buy } else { Side::Sell };
                let frame = GatewayFrame::Command(command(
                    base as u64 + seq as u64 + 1,
                    member as u32 + 1,
                    price,
                    side,
                ));
                let sent = shared.start.elapsed().as_nanos() as u64;
                shared.sent[base + seq].store(sent, Ordering::Relaxed);
                stream.write_all(&frame.encode()).unwrap();
            }
        }));
    }

    while shared.received.load(Ordering::Relaxed) < total {
        runtime.tick();
        assert!(
            shared.start.elapsed() < Duration::from_secs(30),
            "load did not finish"
        );
    }
    let elapsed = shared.start.elapsed();
    for sender in senders {
        sender.join().unwrap();
    }
    let mut samples: Vec<u64> = Vec::with_capacity(total);
    for reader in readers {
        samples.extend(reader.join().unwrap());
    }
    samples.sort_unstable();
    let percentile = |p: f64| {
        let index = ((samples.len().saturating_sub(1) as f64) * p / 100.0) as usize;
        samples[index]
    };
    println!(
        "load_latency: members={members} n={} elapsed={:?} rate={:.0}/s p50={}ns p99={}ns p999={}ns max={}ns",
        samples.len(),
        elapsed,
        samples.len() as f64 / elapsed.as_secs_f64(),
        percentile(50.0),
        percentile(99.0),
        percentile(99.9),
        samples.last().copied().unwrap_or(0)
    );
    let _ = std::fs::remove_file(&path);
}
