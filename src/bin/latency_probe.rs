//! Closed-loop end-to-end latency probe.
//!
//! Each client sends one resting buy order, waits for the confirmation, then
//! cancels it. The measured interval is gateway→engine→gateway round trip for
//! the order. This keeps the book clean and creates exactly one in-flight
//! request per client, so latency is not inflated by artificial queueing.
use clap::Parser;
use matching_engine::shared::protocol::{
    CancelOrderRequest, ClientMessage, ClientRequest, NewOrderRequest, OrderType, PROTOCOL_VERSION,
    ServerMessage, ServerResponse,
};
use matching_engine::shared::wire::{MAX_FRAME_LEN, decode, encode_frame};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(name = "latency_probe")]
struct Config {
    #[arg(long, default_value = "127.0.0.1:8080")]
    server: String,
    #[arg(long, default_value_t = 4)]
    clients: usize,
    #[arg(long, default_value_t = 10)]
    seconds: u64,
    #[arg(long, default_value_t = 100)]
    warmup: u64,
    #[arg(long, default_value_t = 49_000)]
    price: u64,
}

fn main() {
    let config = Config::parse();
    let stop = Arc::new(AtomicBool::new(false));
    let rejects = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::with_capacity(config.clients);
    for client_id in 0..config.clients {
        let server = config.server.clone();
        let stop = Arc::clone(&stop);
        let rejects = Arc::clone(&rejects);
        let price = config.price;
        handles.push(
            thread::Builder::new()
                .name(format!("latency-client-{client_id}"))
                .spawn(move || {
                    run_client(
                        client_id as u64 + 1,
                        &server,
                        price,
                        config.warmup,
                        stop,
                        rejects,
                    )
                })
                .expect("spawn client"),
        );
    }
    thread::sleep(Duration::from_secs(config.seconds));
    stop.store(true, Ordering::Release);
    let mut samples = Vec::new();
    for handle in handles {
        if let Ok(mut client_samples) = handle.join() {
            samples.append(&mut client_samples);
        }
    }
    if samples.is_empty() {
        eprintln!("no latency samples");
        std::process::exit(1);
    }
    samples.sort_unstable();
    let n = samples.len() as f64;
    let pct = |q: f64| samples[((samples.len() as f64 * q).ceil() as usize).min(samples.len()) - 1];
    println!(
        "samples={} rejects={} throughput={:.0}/s",
        samples.len(),
        rejects.load(Ordering::Relaxed),
        n / config.seconds as f64
    );
    println!(
        "order RTT ns: p50={} p90={} p99={} p99.9={} max={}",
        pct(0.50),
        pct(0.90),
        pct(0.99),
        pct(0.999),
        samples[samples.len() - 1]
    );
}

fn run_client(
    user_id: u64,
    server: &str,
    price: u64,
    warmup: u64,
    stop: Arc<AtomicBool>,
    rejects: Arc<AtomicU64>,
) -> Vec<u64> {
    let mut stream = loop {
        match TcpStream::connect(server) {
            Ok(stream) => break stream,
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    };
    stream.set_nodelay(true).expect("nodelay");
    let mut reader = stream.try_clone().expect("clone");
    let symbol = matching_engine::shared::SymbolId::intern("BTCUSDT");
    let mut iteration = 0u64;
    let mut request_id = 0u64;
    let mut samples = Vec::with_capacity(256 * 1024);

    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        iteration += 1;
        let client_order_id = iteration;
        request_id += 1;
        let order = ClientRequest {
            version: PROTOCOL_VERSION,
            request_id,
            message: ClientMessage::NewOrder(NewOrderRequest {
                user_id,
                client_order_id,
                symbol,
                order_type: OrderType::Buy,
                price,
                quantity: 1,
            }),
        };
        let started = Instant::now();
        stream
            .write_all(&encode_frame(&order).expect("encode order"))
            .expect("write order");
        let response = read_until_id(&mut reader, request_id);
        let elapsed = started.elapsed().as_nanos() as u64;
        let ServerMessage::Confirmation(confirmation) = response.message else {
            if matches!(response.message, ServerMessage::Rejected { .. }) {
                rejects.fetch_add(1, Ordering::Relaxed);
            }
            continue;
        };

        request_id += 1;
        let cancel = ClientRequest {
            version: PROTOCOL_VERSION,
            request_id,
            message: ClientMessage::CancelOrder(CancelOrderRequest {
                user_id,
                order_id: confirmation.order_id,
            }),
        };
        stream
            .write_all(&encode_frame(&cancel).expect("encode cancel"))
            .expect("write cancel");
        let _ = read_until_id(&mut reader, request_id);

        if iteration >= warmup {
            samples.push(elapsed);
        }
    }
    samples
}

fn read_until_id(reader: &mut TcpStream, request_id: u64) -> ServerResponse {
    loop {
        let mut len = [0u8; 4];
        reader.read_exact(&mut len).expect("read length");
        let len = u32::from_be_bytes(len) as usize;
        if len > MAX_FRAME_LEN {
            panic!("frame too large: {len}");
        }
        let mut payload = vec![0u8; len];
        reader.read_exact(&mut payload).expect("read payload");
        let response = decode::<ServerResponse>(&payload).expect("decode response");
        if response.request_id == request_id {
            return response;
        }
    }
}
