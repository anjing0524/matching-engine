//! Fire-and-forget load generator for the matching engine.
//!
//! Uses blocking OS threads and raw TCP sockets: no async runtime is involved,
//! which matches the production data path. Reports throughput plus the
//! server-side latency percentiles scraped from `/metrics`.
use matching_engine::shared::protocol::{
    ClientMessage, ClientRequest, NewOrderRequest, OrderType, PROTOCOL_VERSION, ServerMessage,
    ServerResponse,
};
use matching_engine::shared::wire::{MAX_FRAME_LEN, decode, encode_frame};
use rand::Rng;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_CLIENTS: u32 = 64;
const TEST_DURATION: Duration = Duration::from_secs(10);
const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:8080";
const DEFAULT_METRICS_ADDR: &str = "127.0.0.1:9090";

fn main() {
    let server_addr =
        std::env::var("SERVER_ADDR").unwrap_or_else(|_| DEFAULT_SERVER_ADDR.to_string());
    let metrics_addr =
        std::env::var("METRICS_ADDR").unwrap_or_else(|_| DEFAULT_METRICS_ADDR.to_string());
    let num_clients = std::env::var("CLIENTS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_CLIENTS);

    println!("启动吞吐量测试...");
    println!("服务器: {server_addr}");
    println!("模拟客户端数量: {num_clients}");
    println!("测试持续时间: {TEST_DURATION:?}");

    let trade_counter = Arc::new(AtomicU64::new(0));
    let reject_counter = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    for client_id in 0..num_clients {
        let server_addr = server_addr.clone();
        let trade_counter = Arc::clone(&trade_counter);
        let reject_counter = Arc::clone(&reject_counter);
        let stop = Arc::clone(&stop);
        thread::Builder::new()
            .name(format!("load-{client_id}"))
            .spawn(move || run_client(client_id, server_addr, trade_counter, reject_counter, stop))
            .expect("failed to spawn load client");
    }

    let started = Instant::now();
    thread::sleep(TEST_DURATION);
    stop.store(true, Ordering::Release);
    let elapsed = started.elapsed();

    let total_trades = trade_counter.load(Ordering::Relaxed);
    let total_rejects = reject_counter.load(Ordering::Relaxed);
    let throughput = total_trades as f64 / elapsed.as_secs_f64();

    println!("\n--- 测试结果 ---");
    println!("总撮合交易数: {total_trades}");
    println!("被拒绝订单数: {total_rejects}");
    println!("吞吐量 (TPS): {throughput:.2}");

    match scrape_metrics(&metrics_addr) {
        Ok(body) => {
            if let Some(request_count) =
                histogram_count(&body, "matching_engine_request_latency_nanoseconds")
            {
                println!("引擎请求数: {request_count}");
                println!(
                    "引擎请求速率: {:.2}/s",
                    request_count as f64 / elapsed.as_secs_f64()
                );
            }
            let series = [
                (
                    "请求(engine)",
                    "matching_engine_request_latency_nanoseconds",
                ),
                (
                    "socket-to-socket",
                    "matching_engine_socket_to_socket_nanoseconds",
                ),
                ("WAL commit", "matching_engine_commit_latency_nanoseconds"),
                ("decode", "matching_engine_network_decode_nanoseconds"),
                ("encode", "matching_engine_network_encode_nanoseconds"),
                ("socket send", "matching_engine_socket_send_nanoseconds"),
            ];
            for (label, metric) in series {
                match histogram_percentiles(&body, metric, &[0.50, 0.90, 0.99, 0.999, 0.9999]) {
                    Some(percentiles) => {
                        let quantiles = ["p50", "p90", "p99", "p99.9", "p99.99"];
                        print!("{label}: ");
                        for (quantile, value) in quantiles.iter().zip(percentiles) {
                            if value == u64::MAX {
                                print!("{quantile}=overflow ");
                            } else {
                                print!("{quantile}={value}ns ");
                            }
                        }
                        println!();
                    }
                    None => println!("{label}: 未找到 {metric}"),
                }
            }
        }
        Err(error) => println!("无法读取 {metrics_addr}/metrics: {error}"),
    }
}

/// Reads one length-prefixed frame.
fn read_frame<R: Read>(stream: &mut R) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame length {len} exceeds maximum {MAX_FRAME_LEN}"),
        ));
    }
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;
    Ok(payload)
}

fn run_client(
    client_id: u32,
    server_addr: String,
    trade_counter: Arc<AtomicU64>,
    reject_counter: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
) {
    let addr: SocketAddr = match server_addr.parse() {
        Ok(addr) => addr,
        Err(error) => {
            eprintln!("[客户端 {client_id}] 服务器地址无效: {error}");
            return;
        }
    };

    let stream = match TcpStream::connect(addr) {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("[客户端 {client_id}] 连接失败: {error}");
            return;
        }
    };
    if let Err(error) = stream.set_nodelay(true) {
        eprintln!("[客户端 {client_id}] 设置 TCP_NODELAY 失败: {error}");
        return;
    }

    let mut reader = match stream.try_clone() {
        Ok(reader) => reader,
        Err(error) => {
            eprintln!("[客户端 {client_id}] 克隆 socket 失败: {error}");
            return;
        }
    };
    let reader_trade_counter = Arc::clone(&trade_counter);
    let reader_reject_counter = Arc::clone(&reject_counter);
    thread::Builder::new()
        .name(format!("load-reader-{client_id}"))
        .spawn(move || {
            while let Ok(payload) = read_frame(&mut reader) {
                match decode::<ServerResponse>(&payload) {
                    Ok(response) if response.version == PROTOCOL_VERSION => {
                        match response.message {
                            ServerMessage::Trade(_) => {
                                reader_trade_counter.fetch_add(1, Ordering::Relaxed);
                            }
                            ServerMessage::Confirmation(_)
                            | ServerMessage::CancelAcknowledged(_) => {}
                            ServerMessage::Rejected { .. } => {
                                reader_reject_counter.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                    Ok(response) => {
                        eprintln!("[客户端 {client_id}] 协议版本不匹配: {}", response.version);
                        break;
                    }
                    Err(error) => {
                        eprintln!("[客户端 {client_id}] 解码失败: {error}");
                        break;
                    }
                }
            }
        })
        .expect("failed to spawn reader");

    let mut writer = stream;
    let mut client_order_id = 0u64;
    while !stop.load(Ordering::Acquire) {
        client_order_id += 1;
        let order = {
            let mut rng = rand::thread_rng();
            let order_type = if rng.r#gen::<bool>() {
                OrderType::Buy
            } else {
                OrderType::Sell
            };
            let price = match order_type {
                OrderType::Buy => rng.gen_range(49_990..=50_000),
                OrderType::Sell => rng.gen_range(50_000..=50_010),
            };
            NewOrderRequest {
                user_id: client_id as u64,
                client_order_id,
                symbol: matching_engine::shared::SymbolId::intern("BTCUSDT"),
                order_type,
                price,
                quantity: rng.gen_range(1..=5),
            }
        };

        let frame = match encode_frame(&ClientRequest {
            version: PROTOCOL_VERSION,
            request_id: client_order_id,
            message: ClientMessage::NewOrder(order),
        }) {
            Ok(frame) => frame,
            Err(error) => {
                eprintln!("[客户端 {client_id}] 编码失败: {error}");
                break;
            }
        };
        if writer.write_all(&frame).is_err() {
            break;
        }
    }
}

/// Fetches `/metrics` with a minimal HTTP/1.1 request.
fn scrape_metrics(addr: &str) -> io::Result<String> {
    let mut stream = TcpStream::connect(addr)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(
        format!("GET /metrics HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let body_start = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed HTTP response"))?;
    String::from_utf8(response[body_start..].to_vec())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Returns the total count of a Prometheus histogram.
fn histogram_count(body: &str, metric: &str) -> Option<u64> {
    let count_prefix = format!("{metric}_count");
    let mut total = 0u64;
    let mut found = false;
    for line in body.lines() {
        if !line.starts_with(&count_prefix) {
            continue;
        }
        if let Some(value) = line.split_whitespace().last()
            && let Ok(value) = value.parse::<u64>()
        {
            total = total.saturating_add(value);
            found = true;
        }
    }
    found.then_some(total)
}

/// Computes approximate quantiles from Prometheus cumulative histogram buckets.
fn histogram_percentiles(body: &str, metric: &str, quantiles: &[f64]) -> Option<Vec<u64>> {
    let bucket_prefix = format!("{metric}_bucket{{");
    let mut buckets: Vec<(f64, u64)> = Vec::new();

    for line in body.lines() {
        let Some(rest) = line.strip_prefix(bucket_prefix.as_str()) else {
            continue;
        };
        let Some((labels, value)) = rest.rsplit_once('}') else {
            continue;
        };
        let Some(value) = value.trim().parse::<u64>().ok() else {
            continue;
        };
        let Some(le) = labels
            .split(',')
            .find_map(|label| label.trim().strip_prefix("le=\""))
            .and_then(|le| le.trim_end_matches('"').parse::<f64>().ok())
        else {
            continue;
        };
        buckets.push((le, value));
    }

    if buckets.is_empty() {
        return None;
    }
    buckets.sort_by(|left, right| left.0.total_cmp(&right.0));
    let total = buckets.last()?.1;
    if total == 0 {
        return Some(vec![0; quantiles.len()]);
    }

    Some(
        quantiles
            .iter()
            .map(|quantile| {
                let target = (total as f64 * quantile).ceil().max(1.0) as u64;
                for (upper_bound, cumulative) in &buckets {
                    if *cumulative >= target {
                        return if upper_bound.is_infinite() {
                            u64::MAX
                        } else {
                            *upper_bound as u64
                        };
                    }
                }
                u64::MAX
            })
            .collect(),
    )
}
