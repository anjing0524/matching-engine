/// 网络客户端示例
///
/// 连接到匹配引擎服务器并发送订单
use matching_engine::infrastructure::network::{BincodeCodec, Codec, LengthDelimitedCodec};
use matching_engine::shared::protocol::{ClientMessage, NewOrderRequest, OrderType};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let server_addr = std::env::var("SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());

    println!("🔌 连接到服务器: {}", server_addr);
    let mut stream = TcpStream::connect(&server_addr).await?;
    println!("✅ 已连接到 {}", server_addr);

    let mut codec = LengthDelimitedCodec::new(BincodeCodec::<ClientMessage>::new());

    // 测试订单
    let orders = vec![
        // 买单
        NewOrderRequest {
            user_id: 1001,
            symbol: Arc::from("BTCUSDT"),
            order_type: OrderType::Buy,
            price: 50000,
            quantity: 10,
        },
        NewOrderRequest {
            user_id: 1002,
            symbol: Arc::from("BTCUSDT"),
            order_type: OrderType::Buy,
            price: 49500,
            quantity: 5,
        },
        // 卖单
        NewOrderRequest {
            user_id: 2001,
            symbol: Arc::from("BTCUSDT"),
            order_type: OrderType::Sell,
            price: 50100,
            quantity: 8,
        },
        NewOrderRequest {
            user_id: 2002,
            symbol: Arc::from("BTCUSDT"),
            order_type: OrderType::Sell,
            price: 50000, // 与买单价格匹配
            quantity: 3,
        },
    ];

    println!("\n📤 发送 {} 个订单...\n", orders.len());

    for (i, order) in orders.iter().enumerate() {
        let msg = ClientMessage::NewOrder(order.clone());

        // 编码
        let mut buf = vec![0u8; 4096];
        let size = codec.encode(&msg, &mut buf)?;

        // 发送
        stream.write_all(&buf[..size]).await?;
        stream.flush().await?;

        println!(
            "  📨 订单 #{}: {} {:?} @ {} x {}",
            i + 1,
            order.symbol,
            order.order_type,
            order.price,
            order.quantity
        );

        // 延迟以便观察
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    println!("\n✅ 所有订单已发送");
    println!("💤 保持连接 5 秒...");
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    println!("👋 断开连接");
    Ok(())
}
