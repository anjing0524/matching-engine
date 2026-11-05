//! Closed-loop probe for the gateway→engine SPSC ring transport.
//!
//! This bypasses TCP and the gateway event loop: it measures the engine IPC
//! ring + matching path only. The gateway integration will reuse the same ring
//! protocol.
use clap::Parser;
use matching_engine::interfaces::engine_ipc::{
    IpcRequest, RING_REQUEST_FILE, RING_RESPONSE_FILE, decode_response, encode_request,
};
use matching_engine::shared::protocol::{
    CancelOrderRequest, ClientMessage, ClientRequest, NewOrderRequest, OrderType, PROTOCOL_VERSION,
    ServerMessage,
};
use matching_engine::shared::spsc_ring::{PushError, SpscRing};
use matching_engine::shared::wire::encode_to_vec;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
struct Config {
    #[arg(long, default_value = "./data/ipc")]
    ring_dir: PathBuf,
    #[arg(long, default_value_t = 10)]
    seconds: u64,
    #[arg(long, default_value_t = 49_000)]
    price: u64,
}

fn main() {
    let config = Config::parse();
    let mut samples = Vec::new();
    let stop = Instant::now() + Duration::from_secs(config.seconds);
    let (request_ring, response_ring) = loop {
        let req = config.ring_dir.join(RING_REQUEST_FILE);
        let resp = config.ring_dir.join(RING_RESPONSE_FILE);
        if req.exists()
            && resp.exists()
            && let (Ok(request), Ok(response)) = (SpscRing::open(&req), SpscRing::open(&resp))
        {
            break (request, response);
        }
        thread::sleep(Duration::from_millis(20));
        if Instant::now() > stop {
            eprintln!("timed out waiting for rings");
            std::process::exit(1);
        }
    };
    let mut out = Vec::with_capacity(16 * 1024);
    let symbol = matching_engine::shared::SymbolId::intern("BTCUSDT");
    let mut request_id = 0u64;
    let mut client_order_id = 0u64;
    while Instant::now() < stop {
        client_order_id += 1;
        request_id += 1;
        let order = ClientRequest {
            version: PROTOCOL_VERSION,
            request_id,
            message: ClientMessage::NewOrder(NewOrderRequest {
                user_id: 1,
                client_order_id,
                symbol,
                order_type: OrderType::Buy,
                price: config.price,
                quantity: 1,
            }),
        };
        let payload = encode_to_vec(&order).expect("encode");
        let request = IpcRequest {
            client_id: 1,
            payload,
        };
        let started = Instant::now();
        push(&request_ring, &encode_request(&request));
        let responses = pop(&response_ring, &mut out);
        let elapsed = started.elapsed().as_nanos() as u64;
        let order_id = responses.into_iter().find_map(|payload| {
            if let Ok(response) = matching_engine::shared::wire::decode::<
                matching_engine::shared::protocol::ServerResponse,
            >(&payload)
                && let ServerMessage::Confirmation(confirmation) = response.message
            {
                return Some(confirmation.order_id);
            }
            None
        });
        let Some(order_id) = order_id else {
            continue;
        };
        request_id += 1;
        let cancel = ClientRequest {
            version: PROTOCOL_VERSION,
            request_id,
            message: ClientMessage::CancelOrder(CancelOrderRequest {
                user_id: 1,
                order_id,
            }),
        };
        let payload = encode_to_vec(&cancel).expect("encode cancel");
        let request = IpcRequest {
            client_id: 1,
            payload,
        };
        push(&request_ring, &encode_request(&request));
        let _ = pop(&response_ring, &mut out);
        samples.push(elapsed);
    }
    samples.sort_unstable();
    let pct = |q: f64| samples[((samples.len() as f64 * q).ceil() as usize).min(samples.len()) - 1];
    println!(
        "ring samples={} throughput={:.0}/s p50={} p90={} p99={} p99.9={} max={}",
        samples.len(),
        samples.len() as f64 / config.seconds as f64,
        pct(0.5),
        pct(0.9),
        pct(0.99),
        pct(0.999),
        samples[samples.len() - 1]
    );
}

fn push(ring: &SpscRing, bytes: &[u8]) {
    loop {
        match ring.try_push(bytes) {
            Ok(()) => return,
            Err(PushError::Full) => std::hint::spin_loop(),
            Err(PushError::TooLarge) => panic!("request exceeds ring slot"),
        }
    }
}

fn pop(ring: &SpscRing, out: &mut Vec<u8>) -> Vec<Vec<u8>> {
    loop {
        if ring.try_pop_into(out) {
            let Some((response, _)) = decode_response(out) else {
                continue;
            };
            return response.payloads;
        }
        std::hint::spin_loop();
    }
}
