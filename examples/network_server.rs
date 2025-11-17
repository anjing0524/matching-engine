/// 网络中间件 + 匹配引擎集成示例
///
/// 演示如何将网络中间件与高性能匹配引擎集成
/// 支持多种后端：Tokio、io_uring、DPDK

use matching_engine::network_middleware::{
    BackendType, BincodeCodec, Codec, Connection, LengthDelimitedCodec, MiddlewareConfig,
    NetworkTransport,
};
use matching_engine::protocol::ClientMessage;
use matching_engine::orderbook_tick::{TickBasedOrderBook, ContractSpec};
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::main]
async fn main() {
    // 初始化日志
    tracing_subscriber::fmt::init();

    // 创建 Tick-based 订单簿（9.34M ops/s性能）
    let spec = ContractSpec {
        symbol: "BTCUSDT".to_string(),
        tick_size: 1,          // 1个最小价格单位
        min_price: 40000,      // BTC最低价: $40,000
        max_price: 70000,      // BTC最高价: $70,000
        queue_capacity: 10000, // 每个价位队列容量
    };

    let orderbook = Arc::new(RwLock::new(TickBasedOrderBook::new(spec.clone())));
    tracing::info!("订单簿初始化完成: BTCUSDT, tick_size={}, range=[{}, {}]",
        spec.tick_size, spec.min_price, spec.max_price);

    // 选择网络后端
    let backend = std::env::var("NETWORK_BACKEND")
        .ok()
        .and_then(|s| match s.as_str() {
            "tokio" => Some(BackendType::Tokio),
            #[cfg(feature = "io-uring")]
            "io_uring" => Some(BackendType::IoUring),
            #[cfg(feature = "dpdk")]
            "dpdk" => Some(BackendType::Dpdk),
            _ => None,
        })
        .unwrap_or(BackendType::Tokio);

    tracing::info!("使用网络后端: {:?}", backend);

    // 创建网络中间件配置
    let middleware_config = MiddlewareConfig {
        backend,
        listen_addr: "0.0.0.0:8080".parse().unwrap(),
        buffer_size: 65536,
        rx_queue_depth: 2048,
        tx_queue_depth: 2048,
        cpu_affinity: None,
    };

    // 创建编解码器（目前未使用，在 handle_connection 中创建）
    let _codec = LengthDelimitedCodec::new(BincodeCodec::<ClientMessage>::new());

    // 创建传输层（手动实现，因为我们需要自定义服务逻辑）
    let mut transport: Box<dyn NetworkTransport> = match backend {
        BackendType::Tokio => {
            Box::new(matching_engine::network_middleware::tokio_backend::TokioTransport::new().unwrap())
        }
        #[cfg(feature = "io-uring")]
        BackendType::IoUring => {
            let io_uring_config = matching_engine::network_middleware::io_uring_backend::IoUringConfig {
                queue_depth: middleware_config.rx_queue_depth as u32,
                buffer_size: middleware_config.buffer_size,
                buffer_pool_size: middleware_config.rx_queue_depth,
                ..Default::default()
            };
            Box::new(matching_engine::network_middleware::io_uring_backend::IoUringTransport::new(io_uring_config).unwrap())
        }
        #[cfg(feature = "dpdk")]
        BackendType::Dpdk => {
            let dpdk_config = matching_engine::network_middleware::dpdk_backend::DpdkConfig::default();
            Box::new(matching_engine::network_middleware::dpdk_backend::DpdkTransport::new(dpdk_config).unwrap())
        }
        _ => panic!("Unsupported backend"),
    };

    // 绑定并监听
    transport
        .bind(middleware_config.listen_addr)
        .await
        .unwrap();

    let listen_addr = transport.local_addr().unwrap();
    tracing::info!("服务器启动，监听地址: {}", listen_addr);
    println!("✅ 匹配引擎服务器已启动");
    println!("📡 监听地址: {}", listen_addr);
    println!("⚡ 网络后端: {:?}", backend);
    println!("💾 订单簿: BTCUSDT (Tick-based, 9.34M ops/s)");
    println!("\n等待客户端连接...\n");

    // 接受连接循环
    loop {
        match transport.accept().await {
            Ok(conn) => {
                let peer_addr = conn.peer_addr().ok();
                tracing::info!("接受新连接: {:?}", peer_addr);
                println!("🔗 新连接: {:?}", peer_addr);

                let orderbook_clone = Arc::clone(&orderbook);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(conn, orderbook_clone).await {
                        tracing::error!("连接处理错误: {}", e);
                    }
                });
            }
            Err(e) => {
                tracing::error!("接受连接失败: {}", e);
            }
        }
    }
}

/// 处理单个客户端连接
async fn handle_connection(
    mut conn: Box<dyn Connection>,
    orderbook: Arc<RwLock<TickBasedOrderBook>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let peer_addr = conn.peer_addr().ok();
    tracing::info!("开始处理连接: {:?}", peer_addr);

    let mut codec = LengthDelimitedCodec::new(BincodeCodec::<ClientMessage>::new());

    loop {
        // 接收数据（零拷贝）
        let buf = conn.recv().await?;
        let data = buf.as_slice();

        // 解码消息
        match codec.decode(data)? {
            Some(ClientMessage::NewOrder(order)) => {
                tracing::debug!(
                    "收到订单: symbol={}, type={:?}, price={}, qty={}",
                    order.symbol,
                    order.order_type,
                    order.price,
                    order.quantity
                );

                // 提交到订单簿
                let mut ob = orderbook.write().await;
                let (trades, _confirmation) = ob.match_order(order.clone());
                drop(ob);

                if !trades.is_empty() {
                    tracing::info!("订单撮合成功，产生 {} 笔成交", trades.len());
                    println!("  ✅ 订单撮合成功，产生 {} 笔成交", trades.len());
                } else {
                    tracing::info!("订单已挂单");
                    println!("  📋 订单已挂单");
                }
            }
            Some(ClientMessage::CancelOrder(_cancel)) => {
                tracing::warn!("取消订单功能当前不支持");
                println!("  ⚠️ 取消订单功能当前不支持");
            }
            None => {
                // 不完整的消息，继续接收
                continue;
            }
        }
    }
}
