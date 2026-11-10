//! Multi-member, pipeline-aware load generator.
//!
//! Connects one member per TCP connection, keeps multiple commands in flight,
//! and reports aggregate throughput and private-response latency.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use clap::Parser;
use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::gateway::GatewayFrame;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8000")]
    addr: String,
    #[arg(long, default_value_t = 4)]
    members: usize,
    #[arg(long, default_value_t = 250_000)]
    orders_per_member: usize,
    #[arg(long, default_value_t = 32)]
    in_flight: usize,
}

fn command(id: u64, user_id: u32, side: Side, price: i64) -> Command {
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

fn login(addr: &str, member: usize) -> TcpStream {
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
    std::thread::sleep(Duration::from_millis(50));
    stream
}

fn run_member(
    addr: String,
    member: usize,
    orders: usize,
    in_flight: usize,
    start: Instant,
) -> Vec<u64> {
    let mut stream = login(&addr, member);
    stream.set_nonblocking(true).unwrap();
    let user_id = member as u32 + 1;
    let price = 50_000 + member as i64;
    let base = member.saturating_mul(orders);
    let mut sent_times = vec![u64::MAX; orders];
    let mut acked_flags = vec![false; orders];
    let mut sent = 0usize;
    let mut acked = 0usize;
    let mut pending = Vec::with_capacity(264 * in_flight.max(1));
    let mut pending_offset = 0usize;
    let mut read_buffer = Vec::with_capacity(264 * 4);
    let mut samples = Vec::with_capacity(orders);

    while acked < orders {
        while sent < orders && sent.saturating_sub(acked) < in_flight {
            let id = base as u64 + sent as u64 + 1;
            let side = if sent.is_multiple_of(2) {
                Side::Buy
            } else {
                Side::Sell
            };
            sent_times[sent] = start.elapsed().as_nanos() as u64;
            pending.extend_from_slice(
                &GatewayFrame::Command(command(id, user_id, side, price)).encode(),
            );
            sent += 1;
        }
        loop {
            if pending_offset >= pending.len() {
                pending.clear();
                pending_offset = 0;
                break;
            }
            match stream.write(&pending[pending_offset..]) {
                Ok(0) => panic!("gateway closed during write"),
                Ok(count) => pending_offset += count,
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => panic!("write failed: {error}"),
            }
        }
        loop {
            let mut chunk = [0u8; 1024];
            match stream.read(&mut chunk) {
                Ok(0) => panic!("gateway closed during read"),
                Ok(count) => read_buffer.extend_from_slice(&chunk[..count]),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => panic!("read failed: {error}"),
            }
        }
        while read_buffer.len() >= 264 {
            let mut bytes = [0u8; 264];
            bytes.copy_from_slice(&read_buffer[..264]);
            read_buffer.drain(..264);
            if let Ok(GatewayFrame::PrivateEvent(event)) = GatewayFrame::decode(&bytes) {
                let Some(index) = (event.client_order_id as usize)
                    .checked_sub(base + 1)
                    .filter(|index| *index < orders)
                else {
                    continue;
                };
                if index < sent && !acked_flags[index] && sent_times[index] != u64::MAX {
                    acked_flags[index] = true;
                    acked += 1;
                    samples.push(
                        (start.elapsed().as_nanos() as u64).saturating_sub(sent_times[index]),
                    );
                }
            }
        }
        std::hint::spin_loop();
    }
    samples
}

fn main() {
    let args = Args::parse();
    let start = Instant::now();
    let mut workers = Vec::with_capacity(args.members);
    for member in 0..args.members {
        let addr = args.addr.clone();
        workers.push(std::thread::spawn(move || {
            run_member(addr, member, args.orders_per_member, args.in_flight, start)
        }));
    }
    let mut samples = Vec::with_capacity(args.members * args.orders_per_member);
    for worker in workers {
        samples.extend(worker.join().unwrap());
    }
    let elapsed = start.elapsed();
    samples.sort_unstable();
    let percentile = |p: f64| {
        let index = ((samples.len().saturating_sub(1) as f64) * p / 100.0) as usize;
        samples[index]
    };
    println!(
        "aggregate: members={} orders={} elapsed={:?} rate={:.0}/s p50={}ns p99={}ns p999={}ns max={}ns",
        args.members,
        samples.len(),
        elapsed,
        samples.len() as f64 / elapsed.as_secs_f64(),
        percentile(50.0),
        percentile(99.0),
        percentile(99.9),
        samples.last().copied().unwrap_or(0)
    );
}
