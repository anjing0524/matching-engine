//! Multi-member, pipeline-aware load generator.
//!
//! Connects one member per TCP connection, keeps multiple commands in flight,
//! and reports aggregate throughput and private-response latency.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
    /// Offset added to member/user ids; use distinct offsets per gateway.
    #[arg(long, default_value_t = 0)]
    member_offset: usize,
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
    member_offset: usize,
) -> Vec<u64> {
    let identity = member_offset + member;
    let stream = login(&addr, identity);
    stream.set_nodelay(true).unwrap();
    let mut reader_stream = stream.try_clone().unwrap();
    let mut writer_stream = stream;
    let user_id = identity as u32 + 1;
    let price = 50_000 + identity as i64;
    let base = identity.saturating_mul(orders);
    let sent_times: Arc<Vec<AtomicU64>> =
        Arc::new((0..orders).map(|_| AtomicU64::new(u64::MAX)).collect());
    let acked_flags: Arc<Vec<AtomicBool>> =
        Arc::new((0..orders).map(|_| AtomicBool::new(false)).collect());
    let acked = Arc::new(AtomicUsize::new(0));
    let sent_count = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let reader_sent = Arc::clone(&sent_times);
    let reader_flags = Arc::clone(&acked_flags);
    let reader_acked = Arc::clone(&acked);
    let window = in_flight.max(1);
    let reader = std::thread::spawn(move || {
        let mut samples = Vec::with_capacity(orders);
        let mut buffer = [0u8; 264];
        while samples.len() < orders {
            reader_stream.read_exact(&mut buffer).unwrap();
            let frame = GatewayFrame::decode(&buffer).unwrap();
            let GatewayFrame::PrivateEvent(event) = frame else {
                continue;
            };
            let Some(index) = (event.client_order_id as usize)
                .checked_sub(base + 1)
                .filter(|index| *index < orders)
            else {
                continue;
            };
            if index >= orders {
                continue;
            }
            let sent = reader_sent[index].load(Ordering::SeqCst);
            if sent == u64::MAX || reader_flags[index].swap(true, Ordering::SeqCst) {
                continue;
            }
            let now = start.elapsed().as_nanos() as u64;
            samples.push(now.saturating_sub(sent));
            reader_acked.fetch_add(1, Ordering::Relaxed);
        }
        samples
    });

    let monitor_sent = Arc::clone(&sent_count);
    let monitor_acked = Arc::clone(&acked);
    let monitor_flags = Arc::clone(&acked_flags);
    let monitor_done = Arc::clone(&done);
    let monitor = std::thread::spawn(move || {
        let mut last = 0;
        while !monitor_done.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let now = monitor_acked.load(Ordering::Relaxed);
            if now == last && now < orders {
                let sent = monitor_sent.load(Ordering::Relaxed);
                let missing: Vec<usize> = (0..sent)
                    .filter(|index| !monitor_flags[*index].load(Ordering::Relaxed))
                    .take(20)
                    .collect();
                eprintln!(
                    "load_stall member={member} sent={sent} acked={now} missing_first={missing:?}"
                );
                break;
            }
            last = now;
        }
    });
    let writer = std::thread::spawn(move || {
        for seq in 0..orders {
            while seq.saturating_sub(acked.load(Ordering::Relaxed)) >= window {
                std::hint::spin_loop();
            }
            let id = base as u64 + seq as u64 + 1;
            let side = if seq.is_multiple_of(2) {
                Side::Buy
            } else {
                Side::Sell
            };
            sent_times[seq].store(start.elapsed().as_nanos() as u64, Ordering::SeqCst);
            writer_stream
                .write_all(&GatewayFrame::Command(command(id, user_id, side, price)).encode())
                .unwrap();
            sent_count.fetch_add(1, Ordering::Relaxed);
        }
    });

    writer.join().unwrap();
    let samples = reader.join().unwrap();
    done.store(true, Ordering::Relaxed);
    monitor.join().unwrap();
    samples
}

fn main() {
    let args = Args::parse();
    let start = Instant::now();
    let args_offset = args.member_offset;
    let mut workers = Vec::with_capacity(args.members);
    for member in 0..args.members {
        let addr = args.addr.clone();
        let in_flight = args.in_flight;
        let orders = args.orders_per_member;
        workers.push(std::thread::spawn(move || {
            run_member(addr, member, orders, in_flight, start, args_offset)
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
