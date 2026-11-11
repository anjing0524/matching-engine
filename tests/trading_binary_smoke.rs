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

fn wait_nonempty(path: &std::path::Path) -> bool {
    for _ in 0..200 {
        if std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

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
    assert!(wait_nonempty(&dir.join("commands.log")));
    assert!(wait_nonempty(&dir.join("public.log")));
    assert!(wait_nonempty(&dir.join("private.log")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn trading_binary_combo_config_completes_combo_trade() {
    let binary = env!("CARGO_BIN_EXE_trading-engine");
    let dir =
        std::env::temp_dir().join(format!("trading-binary-combo-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("combo.json");
    std::fs::write(
        &config_path,
        r#"
{
  "instruments": [
    {"instrument_id": 1, "symbol": "IF2501", "tick_size": 1, "min_price": 30000, "max_price": 70000},
    {"instrument_id": 2, "symbol": "IF2502", "tick_size": 1, "min_price": 30000, "max_price": 70000}
  ],
  "pairs": [
    {"pair_id": 10, "leg1_instrument_id": 1, "leg2_instrument_id": 2, "spread_tick": 1,
     "margin": {"multiplier": 300, "leg1_rate_bps": 1200, "leg2_rate_bps": 1000}}
  ],
  "prior_settlements": [
    {"instrument_id": 1, "price_ticks": 45000},
    {"instrument_id": 2, "price_ticks": 40000}
  ],
  "margin_balances": [
    {"user_id": 1, "balance": 1000000000000},
    {"user_id": 2, "balance": 1000000000000}
  ]
}
"#,
    )
    .unwrap();

    let mut child = ProcessCommand::new(binary)
        .args([
            "run",
            "--iterations",
            "0",
            "--listen",
            "127.0.0.1:0",
            "--metrics-listen",
            "127.0.0.1:0",
            "--combo-config",
            config_path.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn trading");
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let line = lines.next().unwrap().unwrap();
    let gateway = line
        .split_whitespace()
        .next()
        .unwrap()
        .strip_prefix("gateway=")
        .unwrap()
        .to_string();

    let combo_order = |user_id: u32, side: Side, price_ticks: i64| {
        let mut command = Command::zeroed();
        command.command_type = CommandType::NewOrder;
        command.combo_id = 10;
        command.client_order_id = u64::from(user_id);
        command.user_id = user_id;
        command.price_type = PriceType::Limit;
        command.time_condition = TimeCondition::Gfd;
        command.volume_condition = VolumeCondition::Any;
        command.side = side;
        command.offset = Offset::Open;
        command.hedge = Hedge::Spec;
        command.price_ticks = price_ticks;
        command.quantity = 1;
        command.trigger_type = TriggerType::None;
        command
    };
    let login = |stream: &mut TcpStream, user_id: u32| {
        stream
            .write_all(
                &GatewayFrame::Login {
                    member_id: 1,
                    user_id,
                    seat_id: 1,
                    session_epoch: 1,
                    last_private_seq: 0,
                    auth_token: 0,
                }
                .encode(),
            )
            .unwrap();
    };

    let mut maker = TcpStream::connect(&gateway).unwrap();
    maker
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    login(&mut maker, 1);
    maker
        .write_all(&GatewayFrame::Command(combo_order(1, Side::Sell, 100)).encode())
        .unwrap();

    let mut taker = TcpStream::connect(&gateway).unwrap();
    taker
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    login(&mut taker, 2);
    taker
        .write_all(&GatewayFrame::Command(combo_order(2, Side::Buy, 100)).encode())
        .unwrap();

    let mut fill = None;
    for _ in 0..500 {
        let mut frame = [0u8; trading_system_engine::trading::gateway::GATEWAY_FRAME_SIZE];
        if taker.read_exact(&mut frame).is_err() {
            continue;
        }
        if let GatewayFrame::ComboPrivate(private) = GatewayFrame::decode(&frame).unwrap() {
            fill = Some(private);
            break;
        }
    }
    let fill = fill.expect("combo private fill through the binary");
    assert_eq!(fill.pair_id, 10);
    assert_eq!(fill.user_id, 2);
    assert_eq!(fill.quantity, 1);
    assert_eq!(fill.spread_price, 100);

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn trading_binary_rejects_combo_config_without_margin_balances() {
    let binary = env!("CARGO_BIN_EXE_trading-engine");
    let dir = std::env::temp_dir().join(format!(
        "trading-binary-combo-reject-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("combo.json");
    std::fs::write(
        &config_path,
        r#"
{
  "instruments": [
    {"instrument_id": 1, "symbol": "IF2501", "tick_size": 1, "min_price": 30000, "max_price": 70000},
    {"instrument_id": 2, "symbol": "IF2502", "tick_size": 1, "min_price": 30000, "max_price": 70000}
  ],
  "pairs": [
    {"pair_id": 10, "leg1_instrument_id": 1, "leg2_instrument_id": 2, "spread_tick": 1,
     "margin": {"multiplier": 300, "leg1_rate_bps": 1200, "leg2_rate_bps": 1000}}
  ],
  "margin_balances": []
}
"#,
    )
    .unwrap();
    let output = ProcessCommand::new(binary)
        .args([
            "run",
            "--iterations",
            "1",
            "--listen",
            "127.0.0.1:0",
            "--metrics-listen",
            "127.0.0.1:0",
            "--combo-config",
            config_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn trading");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Margin"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
