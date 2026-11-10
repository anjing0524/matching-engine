//! Deployment smoke test for the `trading` binary.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Command as ProcessCommand, Stdio};
use std::time::Duration;
use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::gateway::GatewayFrame;
use trading_system_engine::trading::transport::command_log::read_segment;
use trading_system_engine::trading::transport::result_log::{read_private_log, read_public_log};

#[test]
fn trading_binary_accepts_order_and_serves_metrics() {
    let binary = env!("CARGO_BIN_EXE_trading-engine");
    let dir = std::env::temp_dir().join(format!("trading-binary-smoke-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let mut child = ProcessCommand::new(binary)
        .args([
            "run",
            "--iterations",
            "5000",
            "--listen",
            "127.0.0.1:0",
            "--metrics-listen",
            "127.0.0.1:0",
            "--command-log",
            dir.join("commands.log").to_str().unwrap(),
            "--public-log",
            dir.join("public.log").to_str().unwrap(),
            "--private-log",
            dir.join("private.log").to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn trading");
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let line = lines.next().unwrap().unwrap();
    let mut parts = line.split_whitespace();
    let gateway = parts
        .next()
        .unwrap()
        .strip_prefix("gateway=")
        .unwrap()
        .to_string();
    let metrics = parts
        .next()
        .unwrap()
        .strip_prefix("metrics=")
        .unwrap()
        .to_string();

    let mut stream = TcpStream::connect(&gateway).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
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

    let mut frame = [0u8; trading_system_engine::trading::gateway::GATEWAY_FRAME_SIZE];
    stream.read_exact(&mut frame).unwrap();
    assert!(matches!(
        GatewayFrame::decode(&frame).unwrap(),
        GatewayFrame::PrivateEvent(_)
    ));

    let mut http = TcpStream::connect(&metrics).unwrap();
    http.write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    http.read_to_string(&mut response).unwrap();
    assert!(response.contains("trading_commands_total"));
    assert!(response.contains("trading_tick_latency_seconds_count"));

    let _ = child.kill();
    let _ = child.wait();
    assert!(!read_segment(&dir.join("commands.log")).unwrap().is_empty());
    assert!(!read_public_log(&dir.join("public.log")).unwrap().is_empty());
    assert!(
        !read_private_log(&dir.join("private.log"))
            .unwrap()
            .is_empty()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
