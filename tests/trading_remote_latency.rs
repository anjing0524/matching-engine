//! Out-of-process end-to-end latency profile.
//!
//! The trading runtime runs in a child process; the test process is only the
//! member client. This avoids same-process client/runtime CPU contention.
//!
//! `LATENCY_ITERS=100000 cargo test --release --test trading_remote_latency -- --ignored --nocapture`

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
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
    let addr = listener.local_addr().unwrap();
    format!("{addr}")
}

fn wait_for_server(addr: &str) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match TcpStream::connect(addr) {
            Ok(stream) => return stream,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("server did not start: {error}"),
        }
    }
}

fn read_frame(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> GatewayFrame {
    loop {
        if buffer.len() >= 264 {
            let mut bytes = [0u8; 264];
            bytes.copy_from_slice(&buffer[..264]);
            buffer.drain(..264);
            return GatewayFrame::decode(&bytes).expect("decode response");
        }
        let mut chunk = [0u8; 1024];
        match stream.read(&mut chunk) {
            Ok(0) => panic!("server closed"),
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_micros(10));
            }
            Err(error) => panic!("read failed: {error}"),
        }
    }
}

#[test]
#[ignore = "out-of-process latency profile; run explicitly"]
fn remote_single_member_latency_profile() {
    let iterations: usize = std::env::var("LATENCY_ITERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(100_000);
    let warmup = iterations / 10;
    let addr = free_addr();
    let mut server: Child = ProcessCommand::new(env!("CARGO_BIN_EXE_trading-engine"))
        .args([
            "run",
            "--listen",
            &addr,
            "--metrics-listen",
            "127.0.0.1:0",
            "--iterations",
            "0",
            "--idle-timeout-secs",
            "0",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn trading-engine");

    let mut stream = wait_for_server(&addr);
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
    // Wait for the first accepted response; retry the login-like readiness by
    // sending the first real command only after the server has ticked.
    let mut samples = Vec::with_capacity(iterations);
    for index in 0..iterations + warmup {
        let id = index as u64 + 1;
        let frame = GatewayFrame::Command(command(id));
        stream.write_all(&frame.encode()).unwrap();
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
    let _ = server.kill();
    let _ = server.wait();
    samples.sort_unstable();
    let percentile = |p: f64| {
        let index = ((samples.len().saturating_sub(1) as f64) * p / 100.0) as usize;
        samples[index]
    };
    println!(
        "remote_latency: n={} p50={}ns p99={}ns p999={}ns max={}ns",
        samples.len(),
        percentile(50.0),
        percentile(99.0),
        percentile(99.9),
        samples.last().copied().unwrap_or(0)
    );
}
