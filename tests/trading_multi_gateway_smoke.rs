//! Two standalone gateway relays → one middleware/core split process.

use trading_system_engine::trading::gateway::{GATEWAY_FRAME_SIZE, GatewayFrame};
use trading_system_engine::trading::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for(path: &Path) {
    let started = Instant::now();
    while !path.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn connect(addr: &str) -> TcpStream {
    let started = Instant::now();
    loop {
        match TcpStream::connect(addr) {
            Ok(stream) => return stream,
            Err(error) if started.elapsed() < Duration::from_secs(5) => {
                let _ = error;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("connect failed: {error}"),
        }
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn login(member: u32) -> GatewayFrame {
    GatewayFrame::Login {
        member_id: member,
        user_id: member,
        seat_id: 1,
        session_epoch: 1,
        last_private_seq: 0,
        auth_token: 0,
    }
}

fn order(user: u32, id: u64) -> GatewayFrame {
    let mut command = Command::zeroed();
    command.command_type = CommandType::NewOrder;
    command.client_order_id = id;
    command.user_id = user;
    command.instrument_id = 1;
    command.price_type = PriceType::Limit;
    command.time_condition = TimeCondition::Gfd;
    command.volume_condition = VolumeCondition::Any;
    command.side = Side::Buy;
    command.offset = Offset::Open;
    command.hedge = Hedge::Spec;
    command.price_ticks = 50_000;
    command.quantity = 1;
    command.trigger_type = TriggerType::None;
    GatewayFrame::Command(command)
}

fn send_order(addr: &str, member: u32, id: u64) {
    for attempt in 0..20 {
        let mut stream = connect(addr);
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        if stream.write_all(&login(member).encode()).is_err()
            || stream.write_all(&order(member, id).encode()).is_err()
        {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        let mut frame = [0u8; GATEWAY_FRAME_SIZE];
        if stream.read_exact(&mut frame).is_err() {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        if matches!(
            GatewayFrame::decode(&frame).unwrap(),
            GatewayFrame::PrivateEvent(_)
        ) {
            return;
        }
        assert!(attempt < 19, "unexpected gateway frame");
    }
    panic!("gateway relay did not deliver a private event");
}

#[test]
fn two_gateway_relays_share_middleware_and_core() {
    let dir = std::env::temp_dir().join(format!("trading-multi-gateway-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let command_ring = dir.join("command.ring");
    let public_ring = dir.join("public.ring");
    let private_ring = dir.join("private.ring");

    let middleware_port = free_port();
    let middleware_addr = format!("127.0.0.1:{middleware_port}");
    let _middleware = ChildGuard(
        ProcessCommand::new(env!("CARGO_BIN_EXE_trading-middleware"))
            .args([
                "--listen",
                &middleware_addr,
                "--command-ring",
                command_ring.to_str().unwrap(),
                "--public-ring",
                public_ring.to_str().unwrap(),
                "--private-ring",
                private_ring.to_str().unwrap(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn middleware"),
    );
    wait_for(&command_ring);
    wait_for(&public_ring);
    wait_for(&private_ring);

    let _core = ChildGuard(
        ProcessCommand::new(env!("CARGO_BIN_EXE_trading-core"))
            .args([
                "--command-ring",
                command_ring.to_str().unwrap(),
                "--public-ring",
                public_ring.to_str().unwrap(),
                "--private-ring",
                private_ring.to_str().unwrap(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn core"),
    );

    let gateway_a_port = free_port();
    let gateway_b_port = free_port();
    let gateway_a = format!("127.0.0.1:{gateway_a_port}");
    let gateway_b = format!("127.0.0.1:{gateway_b_port}");
    let _relay_a = ChildGuard(
        ProcessCommand::new(env!("CARGO_BIN_EXE_trading-gateway"))
            .args(["--listen", &gateway_a, "--middleware", &middleware_addr])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn gateway a"),
    );
    let _relay_b = ChildGuard(
        ProcessCommand::new(env!("CARGO_BIN_EXE_trading-gateway"))
            .args(["--listen", &gateway_b, "--middleware", &middleware_addr])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn gateway b"),
    );

    std::thread::sleep(Duration::from_millis(50));
    send_order(&gateway_a, 1, 1);
    send_order(&gateway_b, 2, 2);
    let _ = std::fs::remove_dir_all(&dir);
}
