//! Split-process gateway -> middleware -> core latency/throughput profile.
//!
//! Starts the real `trading-gateway`, `trading-middleware`, and `trading-core`
//! binaries, then drives one member through the proxy.
//!
//! `LATENCY_ITERS=100000 cargo test --release --test trading_split_latency -- --ignored --nocapture`

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::gateway::GatewayFrame;

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

fn free_addr() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().to_string()
}

fn wait_connect(addr: &str) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match TcpStream::connect(addr) {
            Ok(stream) => return stream,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("could not connect to {addr}: {error}"),
        }
    }
}

fn spawn(binary: &str, args: &[String]) -> Child {
    ProcessCommand::new(binary)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn {binary}: {error}"))
}

fn read_frame(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> GatewayFrame {
    loop {
        if buffer.len() >= 264 {
            let mut bytes = [0u8; 264];
            bytes.copy_from_slice(&buffer[..264]);
            buffer.drain(..264);
            return GatewayFrame::decode(&bytes).expect("decode private frame");
        }
        let mut chunk = [0u8; 1024];
        match stream.read(&mut chunk) {
            Ok(0) => panic!("gateway closed"),
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == ErrorKind::WouldBlock => std::hint::spin_loop(),
            Err(error) => panic!("read failed: {error}"),
        }
    }
}

#[test]
#[ignore = "split-process latency profile; run explicitly"]
fn split_process_single_member_latency_profile() {
    let iterations: usize = std::env::var("LATENCY_ITERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(100_000);
    let warmup = iterations / 10;
    let dir = std::env::temp_dir().join(format!("trading-split-latency-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let rings = |name: &str| -> String {
        PathBuf::from(&dir)
            .join(name)
            .to_string_lossy()
            .into_owned()
    };
    let command_ring = rings("command.ring");
    let public_ring = rings("public.ring");
    let private_ring = rings("private.ring");
    let middleware_addr = free_addr();
    let proxy_addr = free_addr();
    let mut children = Vec::new();
    children.push(spawn(
        env!("CARGO_BIN_EXE_trading-middleware"),
        &[
            "--listen".to_string(),
            middleware_addr.clone(),
            "--command-ring".to_string(),
            command_ring.clone(),
            "--public-ring".to_string(),
            public_ring.clone(),
            "--private-ring".to_string(),
            private_ring.clone(),
            "--iterations".to_string(),
            "0".to_string(),
            "--idle-timeout-secs".to_string(),
            "0".to_string(),
        ],
    ));
    let _ = wait_connect(&middleware_addr);
    children.push(spawn(
        env!("CARGO_BIN_EXE_trading-core"),
        &[
            "--command-ring".to_string(),
            command_ring,
            "--public-ring".to_string(),
            public_ring,
            "--private-ring".to_string(),
            private_ring,
            "--iterations".to_string(),
            "0".to_string(),
        ],
    ));
    std::thread::sleep(Duration::from_millis(100));
    children.push(spawn(
        env!("CARGO_BIN_EXE_trading-gateway"),
        &[
            "--listen".to_string(),
            proxy_addr.clone(),
            "--middleware".to_string(),
            middleware_addr,
        ],
    ));
    let mut stream = wait_connect(&proxy_addr);
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
    std::thread::sleep(Duration::from_millis(100));
    stream.set_nonblocking(true).unwrap();
    let mut buffer = Vec::with_capacity(1024);
    let mut samples = Vec::with_capacity(iterations);
    for index in 0..iterations + warmup {
        let id = index as u64 + 1;
        stream
            .write_all(&GatewayFrame::Command(command(id)).encode())
            .unwrap();
        let start = Instant::now();
        loop {
            if let GatewayFrame::PrivateEvent(_) = read_frame(&mut stream, &mut buffer) {
                break;
            }
        }
        if index >= warmup {
            samples.push(u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX));
        }
    }
    for child in &mut children {
        let _ = child.kill();
        let _ = child.wait();
    }
    samples.sort_unstable();
    let percentile = |p: f64| {
        let index = ((samples.len().saturating_sub(1) as f64) * p / 100.0) as usize;
        samples[index]
    };
    println!(
        "split_latency: n={} p50={}ns p99={}ns p999={}ns max={}ns",
        samples.len(),
        percentile(50.0),
        percentile(99.0),
        percentile(99.9),
        samples.last().copied().unwrap_or(0)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
