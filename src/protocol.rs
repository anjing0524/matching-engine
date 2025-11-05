use serde::{Deserialize, Serialize};

/// 订单类型，区分买单和卖单
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    Buy,
    Sell,
}

/// 新订单请求，由客户端发起
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewOrderRequest {
    pub user_id: u64,
    pub symbol: String,
    pub order_type: OrderType,
    pub price: u64, // 使用 u64 避免浮点数精度问题，例如价格 123.45 可以表示为 12345
    pub quantity: u64,
}

/// 取消订单请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelOrderRequest {
    pub user_id: u64,
    pub order_id: u64,
}

/// 订单确认回报，发送给下单用户
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderConfirmation {
    pub order_id: u64,
    pub user_id: u64,
}

/// 成交回报，发送给交易双方
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeNotification {
    pub trade_id: u64,
    pub symbol: String,
    // 撮合价格
    pub matched_price: u64,
    // 撮合数量
    pub matched_quantity: u64,
    // 买方信息
    pub buyer_user_id: u64,
    pub buyer_order_id: u64,
    // 卖方信息
    pub seller_user_id: u64,
    pub seller_order_id: u64,
    // 时间戳
    pub timestamp: u64,
}