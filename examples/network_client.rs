//! Blocking network client example.
//!
//! Connects to the example server, sends a few orders, and prints the trades,
//! confirmations, or rejections it receives back.
use matching_engine::shared::protocol::{
    ClientMessage, ClientRequest, NewOrderRequest, OrderType, PROTOCOL_VERSION, ServerResponse,
};
use matching_engine::shared::wire::{MAX_FRAME_LEN, WireError, decode, encode_frame};
use std::io::{Read, Write};
use std::net::TcpStream;

#[derive(Debug, thiserror::Error)]
enum ExampleError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Wire(#[from] WireError),
}

fn main() -> Result<(), ExampleError> {
    tracing_subscriber::fmt::init();

    let server_addr = std::env::var("SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());

    println!("🔌 连接到服务器: {server_addr}");
    let mut stream = TcpStream::connect(&server_addr)?;
    stream.set_nodelay(true)?;
    println!("✅ 已连接到 {server_addr}");

    let orders = [
        NewOrderRequest {
            user_id: 1001,
            client_order_id: 1,
            symbol: matching_engine::shared::SymbolId::intern("BTCUSDT"),
            order_type: OrderType::Buy,
            price: 50000,
            quantity: 10,
        },
        NewOrderRequest {
            user_id: 2001,
            client_order_id: 2,
            symbol: matching_engine::shared::SymbolId::intern("BTCUSDT"),
            order_type: OrderType::Sell,
            price: 50000,
            quantity: 3,
        },
    ];

    for (index, order) in orders.iter().enumerate() {
        let request_id = (index + 1) as u64;
        let frame = encode_frame(&ClientRequest {
            version: PROTOCOL_VERSION,
            request_id,
            message: ClientMessage::NewOrder(order.clone()),
        })?;
        stream.write_all(&frame)?;
        stream.flush()?;

        println!(
            "  📨 订单 #{}: {} {:?} @ {} x {}",
            index + 1,
            order.symbol,
            order.order_type,
            order.price,
            order.quantity
        );

        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf)?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_FRAME_LEN {
            return Err(ExampleError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "server frame too large",
            )));
        }
        let mut payload = vec![0u8; len];
        stream.read_exact(&mut payload)?;
        let response: ServerResponse = decode(&payload)?;
        assert_eq!(response.version, PROTOCOL_VERSION);
        assert_eq!(response.request_id, request_id);
        println!("  📩 服务器响应: {:?}", response.message);
    }

    println!("\n✅ 所有订单已发送");
    Ok(())
}
