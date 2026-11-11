//! Restore a full runtime snapshot through the `trading` binary.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Command as ProcessCommand, Stdio};
use std::time::Duration;
use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::shared::SymbolId;
use trading_system_engine::trading::core::TradingCore;
use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::gateway::{GATEWAY_FRAME_SIZE, GatewayFrame};
use trading_system_engine::trading::runtime::TradingSnapshot;

fn order_frame(user: u32, id: u64, price: i64) -> GatewayFrame {
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
    command.price_ticks = price;
    command.quantity = 1;
    command.trigger_type = TriggerType::None;
    GatewayFrame::Command(command)
}

#[test]
fn binary_restores_runtime_snapshot() {
    let dir = std::env::temp_dir().join(format!("trading-snapshot-binary-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let snapshot_path = dir.join("runtime.json");

    let mut core = TradingCore::new(
        ContractSpec::new("BTCUSDT", 1, 1, 1_000_000),
        SymbolId::intern("BTCUSDT"),
    );
    let mut existing = Command::zeroed();
    existing.command_type = CommandType::NewOrder;
    existing.client_order_id = 99;
    existing.user_id = 9;
    existing.instrument_id = 1;
    existing.price_type = PriceType::Limit;
    existing.time_condition = TimeCondition::Gfd;
    existing.volume_condition = VolumeCondition::Any;
    existing.side = Side::Sell;
    existing.offset = Offset::Open;
    existing.hedge = Hedge::Spec;
    existing.price_ticks = 60_000;
    existing.quantity = 1;
    let _ = core.execute(existing);
    let snapshot = TradingSnapshot {
        core: core.snapshot(),
        combo: None,
    };
    snapshot.save(&snapshot_path).unwrap();

    let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_trading-engine"))
        .args([
            "run",
            "--iterations",
            "0",
            "--listen",
            "127.0.0.1:0",
            "--metrics-listen",
            "127.0.0.1:0",
            "--snapshot",
            snapshot_path.to_str().unwrap(),
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
    stream
        .write_all(&order_frame(1, 1, 59_000).encode())
        .unwrap();
    let mut frame = [0u8; GATEWAY_FRAME_SIZE];
    stream.read_exact(&mut frame).unwrap();
    assert!(matches!(
        GatewayFrame::decode(&frame).unwrap(),
        GatewayFrame::PrivateEvent(_)
    ));

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
