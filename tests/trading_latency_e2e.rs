//! End-to-end single-member latency profile.
//!
//! Measures TCP command write -> gateway -> middleware -> core -> private
//! response read. Ignored by default; run on the target host:
//!
//! `cargo test --release --test trading_latency_e2e -- --ignored --nocapture`

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
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

fn command(id: u64) -> Command {
    let mut command = Command::zeroed();
    command.client_order_id = id;
    command.user_id = 1;
    command.command_type = CommandType::NewOrder;
    command.price_type = PriceType::Limit;
    command.time_condition = TimeCondition::Gfd;
    command.volume_condition = VolumeCondition::Any;
    command.side = Side::Buy;
    command.offset = Offset::Open;
    command.hedge = Hedge::Spec;
    command.price_ticks = 50_000;
    command.quantity = 1;
    command.trigger_type = TriggerType::None;
    command
}

fn write_frame(
    stream: &mut TcpStream,
    runtime: &mut TradingRuntime<NoPlugins>,
    frame: GatewayFrame,
) {
    let bytes = frame.encode();
    let mut written = 0;
    while written < bytes.len() {
        match stream.write(&bytes[written..]) {
            Ok(0) => panic!("gateway closed during write"),
            Ok(count) => written += count,
            Err(error) if error.kind() == ErrorKind::WouldBlock => runtime.tick(),
            Err(error) => panic!("write failed: {error}"),
        }
    }
}

struct FrameReader {
    stream: TcpStream,
    buffer: Vec<u8>,
}

impl FrameReader {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            buffer: Vec::with_capacity(1024),
        }
    }

    fn try_next(&mut self, runtime: &mut TradingRuntime<NoPlugins>) -> Option<GatewayFrame> {
        loop {
            if self.buffer.len() >= 264 {
                let mut bytes = [0u8; 264];
                bytes.copy_from_slice(&self.buffer[..264]);
                self.buffer.drain(..264);
                return Some(GatewayFrame::decode(&bytes).expect("decode private frame"));
            }
            let mut chunk = [0u8; 1024];
            match self.stream.read(&mut chunk) {
                Ok(0) => return None,
                Ok(count) => self.buffer.extend_from_slice(&chunk[..count]),
                Err(error) if error.kind() == ErrorKind::WouldBlock => runtime.tick(),
                Err(error) => panic!("read failed: {error}"),
            }
        }
    }
}

#[test]
#[ignore = "end-to-end latency profile; run explicitly"]
fn single_member_e2e_latency_profile() {
    let iterations: usize = std::env::var("LATENCY_ITERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(100_000);
    let warmup = iterations / 10;
    let path = std::env::temp_dir().join(format!(
        "trading-system-engine-latency-ring-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let (producer, consumer) = create(&path, 1 << 16).unwrap();
    let spec = ContractSpec::new("LAT", 1, 40_000, 60_000);
    let core = TradingCore::new(spec, SymbolId::intern("LAT"));
    let runner = CoreRunner::new(core, consumer, NoPlugins);
    let (server, events) = start("127.0.0.1:0").unwrap();
    let addr = server.addr();
    let middleware = MiddlewareSequencer::new(producer);
    let gateway = GatewayRuntime::new(middleware, events);
    let mut runtime =
        TradingRuntime::new(gateway, runner, server).with_options(RuntimeOptions::default());

    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_nodelay(true).unwrap();
    stream
        .write_all(
            &GatewayFrame::Login {
                member_id: 1,
                auth_token: 0,
                user_id: 1,
                seat_id: 1,
                session_epoch: 1,
                last_private_seq: 0,
            }
            .encode(),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while runtime.metrics().active_sessions == 0 {
        runtime.tick();
        assert!(Instant::now() < deadline, "login did not become active");
        std::thread::sleep(Duration::from_millis(1));
    }

    let reader_stream = stream.try_clone().unwrap();
    reader_stream.set_nonblocking(true).unwrap();
    stream.set_nonblocking(true).unwrap();
    let mut reader = FrameReader::new(reader_stream);
    let mut samples = Vec::with_capacity(iterations);
    for index in 0..iterations + warmup {
        let id = index as u64 + 1;
        write_frame(
            &mut stream,
            &mut runtime,
            GatewayFrame::Command(command(id)),
        );
        let start = Instant::now();
        loop {
            let frame = reader
                .try_next(&mut runtime)
                .expect("gateway stream closed during latency run");
            if matches!(frame, GatewayFrame::PrivateEvent(_)) {
                break;
            }
        }
        if index >= warmup {
            samples.push(u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX));
        }
    }
    samples.sort_unstable();
    let percentile = |p: f64| {
        let index = ((samples.len().saturating_sub(1) as f64) * p / 100.0) as usize;
        samples[index]
    };
    println!(
        "e2e_latency: n={} p50={}ns p99={}ns p999={}ns max={}ns",
        samples.len(),
        percentile(50.0),
        percentile(99.0),
        percentile(99.9),
        samples.last().copied().unwrap_or(0)
    );
    let _ = std::fs::remove_file(&path);
}
