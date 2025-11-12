use futures::{SinkExt, StreamExt};
use matching_engine::shared::protocol::{ClientMessage, NewOrderRequest, OrderType, ServerMessage};
use rand::Rng;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use bincode::config;

// --- 配置 ---
const NUM_CLIENTS: u32 = 8; // 模拟的并发客户端数量
const TEST_DURATION: Duration = Duration::from_secs(10); // 测试持续时间
const SERVER_ADDR: &str = "127.0.0.1:8080";

#[tokio::main]
async fn main() {
    println!("启动吞吐量测试...");
    println!("模拟客户端数量: {}", NUM_CLIENTS);
    println!("测试持续时间: {:?}", TEST_DURATION);

    let trade_counter = Arc::new(AtomicU64::new(0));
    let (latency_tx, mut latency_rx) = mpsc::channel(NUM_CLIENTS as usize * 100);

    let mut handles = Vec::new();

    for i in 0..NUM_CLIENTS {
        let trade_counter = trade_counter.clone();
        let latency_tx = latency_tx.clone();
        let handle = tokio::spawn(async move {
            run_client(i, trade_counter, latency_tx).await;
        });
        handles.push(handle);
    }

    // 等待测试结束
    tokio::time::sleep(TEST_DURATION).await;

    // 测试结束，计算结果
    let total_trades = trade_counter.load(Ordering::Relaxed);
    let throughput = total_trades as f64 / TEST_DURATION.as_secs_f64();

    // 收集并计算平均延迟
    let mut latencies = Vec::new();
    while let Ok(latency) = latency_rx.try_recv() {
        latencies.push(latency);
    }
    let avg_latency = if !latencies.is_empty() {
        latencies.iter().sum::<u128>() as f64 / latencies.len() as f64
    } else {
        0.0
    };

    println!("\n--- 测试结果 ---");
    println!("总撮合交易数: {}", total_trades);
    println!("吞吐量 (TPS): {:.2}", throughput);
    println!("平均端到端延迟: {:.2} µs", avg_latency / 1000.0);

    // 可以在这里中止所有任务，但为了简单起见，我们直接退出进程
    std::process::exit(0);
}



async fn run_client(
    client_id: u32,
    trade_counter: Arc<AtomicU64>,
    latency_tx: mpsc::Sender<u128>,
) {
    let addr: SocketAddr = SERVER_ADDR.parse().unwrap();
    let stream = match TcpStream::connect(addr).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[客户端 {}] 连接失败: {}", client_id, e);
            return;
        }
    };

    let framed = Framed::new(stream, LengthDelimitedCodec::new());
    let (mut writer, mut reader) = framed.split();

    let (order_time_tx, mut order_time_rx) = mpsc::channel::<(u64, Instant)>(1000);
    let config = config::standard();

    // 监听服务器响应的任务
    tokio::spawn(async move {
        let mut sent_orders = std::collections::HashMap::new();
        loop {
            tokio::select! {
                Some((order_id, time)) = order_time_rx.recv() => {
                    sent_orders.insert(order_id, time);
                }
                Some(Ok(buf)) = reader.next() => {
                    match bincode::decode_from_slice(&buf, config) {
                        Ok((decoded, _len)) => {
                            match decoded {
                                ServerMessage::Trade(trade) => {
                                    trade_counter.fetch_add(1, Ordering::Relaxed);
                                    // 估算延迟
                                    if let Some(start_time) = sent_orders.get(&trade.buyer_order_id).or_else(|| sent_orders.get(&trade.seller_order_id)) {
                                        let latency = start_time.elapsed().as_nanos();
                                        let _ = latency_tx.send(latency).await;
                                    }
                                }
                                ServerMessage::Confirmation(_conf) => {
                                    // 可以在这里处理挂单确认的延迟
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("Bincode decoding error in load_generator: {:?}", e);
                        }
                    }
                }
                else => break,
            }
        }
    });

    // 发送订单的任务
    let mut order_id_counter: u64 = (client_id as u64) << 32;
    loop {
        order_id_counter += 1;
        let (order, order_id) = {
            let mut rng = rand::thread_rng();
            let order_type = if rng.gen::<bool>() { OrderType::Buy } else { OrderType::Sell };
            let price = match order_type {
                OrderType::Buy => rng.gen_range(49990..=50000),
                OrderType::Sell => rng.gen_range(50000..=50010),
            };
            let order = NewOrderRequest {
                user_id: client_id as u64,
                symbol: Arc::from("BTC/USD"),
                order_type,
                price,
                quantity: rng.gen_range(1..=5),
            };
            (order, order_id_counter)
        };

        let client_message = ClientMessage::NewOrder(order);
        match bincode::encode_to_vec(client_message, config) {
            Ok(encoded_msg) => {
                if writer.send(encoded_msg.into()).await.is_ok() {
                    // 记录发送时间，用于计算延迟
                    let _ = order_time_tx.send((order_id, Instant::now())).await;
                } else {
                    break; // 连接断开
                }
            }
            Err(e) => {
                eprintln!("Bincode encoding error in load_generator: {:?}", e);
            }
        }
    }
}
