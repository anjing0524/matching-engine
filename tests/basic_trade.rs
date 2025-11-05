use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use futures::{SinkExt, StreamExt};
use matching_engine::protocol::{NewOrderRequest, OrderType, TradeNotification, OrderConfirmation};
use serde_json;

#[tokio::test]
async fn test_basic_match() {
    // 确保服务器正在运行中
    let stream = TcpStream::connect("127.0.0.1:8080").await.expect("无法连接到服务器");
    let mut framed = Framed::new(stream, LengthDelimitedCodec::new());

    // 1. 发送一个买单 (限价单)
    let buy_order = NewOrderRequest {
        user_id: 101,
        symbol: "BTC/USD".to_string(),
        order_type: OrderType::Buy,
        price: 50000,
        quantity: 10,
    };
    let buy_order_json = serde_json::to_string(&buy_order).unwrap();
    framed.send(buy_order_json.into()).await.unwrap();

    // 2. 应该收到一个挂单确认
    let confirmation_msg = framed.next().await.unwrap().unwrap();
    let confirmation: OrderConfirmation = serde_json::from_slice(&confirmation_msg).unwrap();
    assert_eq!(confirmation.user_id, 101);
    println!("收到买单确认: {:?}", confirmation);

    // 3. 发送一个卖单，应该能与上面的买单撮合
    let sell_order = NewOrderRequest {
        user_id: 102,
        symbol: "BTC/USD".to_string(),
        order_type: OrderType::Sell,
        price: 50000, // 价格匹配
        quantity: 7,      // 数量小于买单
    };
    let sell_order_json = serde_json::to_string(&sell_order).unwrap();
    framed.send(sell_order_json.into()).await.unwrap();

    // 4. 应该收到一个成交回报
    // 注意：由于服务器是广播，我们可能会收到自己的挂单确认，需要处理
    // 在这个简化的测试中，我们假设成交回报会先到
    let trade_msg = framed.next().await.unwrap().unwrap();
    let trade: TradeNotification = serde_json::from_slice(&trade_msg).unwrap();

    assert_eq!(trade.matched_price, 50000);
    assert_eq!(trade.matched_quantity, 7);
    assert_eq!(trade.buyer_user_id, 101);
    assert_eq!(trade.seller_user_id, 102);
    println!("收到成交回报: {:?}", trade);
}
