//! End-to-end smoke test for the split middleware/core binaries.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};
use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::gateway::{GATEWAY_FRAME_SIZE, GatewayFrame};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
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

#[test]
fn split_core_and_middleware_deliver_private_event() {
    let dir = std::env::temp_dir().join(format!("trading-split-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let command_ring = dir.join("command.ring");
    let public_ring = dir.join("public.ring");
    let private_ring = dir.join("private.ring");

    let free = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    let listen = format!("127.0.0.1:{port}");

    let mut middleware = ChildGuard(
        ProcessCommand::new(env!("CARGO_BIN_EXE_trading-middleware"))
            .args([
                "--listen",
                &listen,
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

    let mut stream = connect(&listen);
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(
            &GatewayFrame::Login {
                member_id: 1,
                user_id: 1,
                seat_id: 1,
                session_epoch: 1,
                last_private_seq: 0,
                auth_token: 0,
            }
            .encode(),
        )
        .unwrap();
    let mut command = Command::zeroed();
    command.command_type = CommandType::NewOrder;
    command.client_order_id = 1;
    command.user_id = 1;
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
    stream
        .write_all(&GatewayFrame::Command(command).encode())
        .unwrap();

    let mut frame = [0u8; GATEWAY_FRAME_SIZE];
    stream.read_exact(&mut frame).unwrap();
    assert!(matches!(
        GatewayFrame::decode(&frame).unwrap(),
        GatewayFrame::PrivateEvent(_)
    ));

    let _ = middleware.0.kill();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn split_middleware_applies_redirect_effect() {
    let dir = std::env::temp_dir().join(format!("trading-split-redirect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let command_ring = dir.join("command.ring");
    let public_ring = dir.join("public.ring");
    let private_ring = dir.join("private.ring");

    let free = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    let listen = format!("127.0.0.1:{port}");

    let _middleware = ChildGuard(
        ProcessCommand::new(env!("CARGO_BIN_EXE_trading-middleware"))
            .args([
                "--listen",
                &listen,
                "--command-ring",
                command_ring.to_str().unwrap(),
                "--public-ring",
                public_ring.to_str().unwrap(),
                "--private-ring",
                private_ring.to_str().unwrap(),
                "--redirect-target",
                "127.0.0.1:9999",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn middleware"),
    );
    wait_for(&command_ring);
    let login = GatewayFrame::Login {
        member_id: 1,
        user_id: 1,
        seat_id: 1,
        session_epoch: 1,
        last_private_seq: 0,
        auth_token: 0,
    };
    let mut displaced = connect(&listen);
    displaced
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    displaced.write_all(&login.clone().encode()).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let mut replacement = connect(&listen);
    replacement.write_all(&login.encode()).unwrap();
    let mut frame = [0u8; GATEWAY_FRAME_SIZE];
    displaced.read_exact(&mut frame).unwrap();
    assert_eq!(
        GatewayFrame::decode(&frame).unwrap(),
        GatewayFrame::Redirect {
            address: "127.0.0.1:9999".to_string()
        }
    );
}
