use serde::{Deserialize, Serialize};
use bincode::{Encode, Decode, enc::Encoder, de::Decoder, error::{EncodeError, DecodeError}};
use std::sync::Arc;

/// 订单类型，区分买单和卖单
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Encode, Decode)]
pub enum OrderType {
    Buy,
    Sell,
}

/// 新订单请求，由客户端发起
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewOrderRequest {
    pub user_id: u64,
    #[serde(with = "arc_str_serde")]
    pub symbol: Arc<str>,
    pub order_type: OrderType,
    pub price: u64, // 使用 u64 避免浮点数精度问题，例如价格 123.45 可以表示为 12345
    pub quantity: u64,
}

/// 取消订单请求
#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode)]
pub struct CancelOrderRequest {
    pub user_id: u64,
    pub order_id: u64,
}

/// 订单确认回报，发送给下单用户
#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode)]
pub struct OrderConfirmation {
    pub order_id: u64,
    pub user_id: u64,
}

/// 成交回报，发送给交易双方
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeNotification {
    pub trade_id: u64,
    #[serde(with = "arc_str_serde")]
    pub symbol: Arc<str>,
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

/// 客户端发送给服务器的所有消息的顶层枚举
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    NewOrder(NewOrderRequest),
    CancelOrder(CancelOrderRequest),
}

/// 服务器发送给客户端的所有消息的顶层枚举
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    Trade(TradeNotification),
    Confirmation(OrderConfirmation),
}

// Custom serde module for Arc<str>
mod arc_str_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::sync::Arc;

    pub fn serialize<S>(arc: &Arc<str>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        arc.as_ref().serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Arc<str>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Arc::from(s))
    }
}

// Manual Encode implementation for NewOrderRequest
impl Encode for NewOrderRequest {
    fn encode<E: Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        self.user_id.encode(encoder)?;
        self.symbol.as_ref().encode(encoder)?;
        self.order_type.encode(encoder)?;
        self.price.encode(encoder)?;
        self.quantity.encode(encoder)?;
        Ok(())
    }
}

// Manual Decode implementation for NewOrderRequest
impl Decode for NewOrderRequest {
    fn decode<D: Decoder>(decoder: &mut D) -> Result<Self, DecodeError> {
        let user_id = u64::decode(decoder)?;
        let symbol_string = String::decode(decoder)?;
        let symbol = Arc::from(symbol_string);
        let order_type = OrderType::decode(decoder)?;
        let price = u64::decode(decoder)?;
        let quantity = u64::decode(decoder)?;
        Ok(NewOrderRequest {
            user_id,
            symbol,
            order_type,
            price,
            quantity,
        })
    }
}

// Manual Encode implementation for TradeNotification
impl Encode for TradeNotification {
    fn encode<E: Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        self.trade_id.encode(encoder)?;
        self.symbol.as_ref().encode(encoder)?;
        self.matched_price.encode(encoder)?;
        self.matched_quantity.encode(encoder)?;
        self.buyer_user_id.encode(encoder)?;
        self.buyer_order_id.encode(encoder)?;
        self.seller_user_id.encode(encoder)?;
        self.seller_order_id.encode(encoder)?;
        self.timestamp.encode(encoder)?;
        Ok(())
    }
}

// Manual Decode implementation for TradeNotification
impl Decode for TradeNotification {
    fn decode<D: Decoder>(decoder: &mut D) -> Result<Self, DecodeError> {
        let trade_id = u64::decode(decoder)?;
        let symbol_string = String::decode(decoder)?;
        let symbol = Arc::from(symbol_string);
        let matched_price = u64::decode(decoder)?;
        let matched_quantity = u64::decode(decoder)?;
        let buyer_user_id = u64::decode(decoder)?;
        let buyer_order_id = u64::decode(decoder)?;
        let seller_user_id = u64::decode(decoder)?;
        let seller_order_id = u64::decode(decoder)?;
        let timestamp = u64::decode(decoder)?;
        Ok(TradeNotification {
            trade_id,
            symbol,
            matched_price,
            matched_quantity,
            buyer_user_id,
            buyer_order_id,
            seller_user_id,
            seller_order_id,
            timestamp,
        })
    }
}

// Manual Encode implementation for ClientMessage
impl Encode for ClientMessage {
    fn encode<E: Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        match self {
            ClientMessage::NewOrder(req) => {
                0u32.encode(encoder)?;
                req.encode(encoder)?;
            }
            ClientMessage::CancelOrder(req) => {
                1u32.encode(encoder)?;
                req.encode(encoder)?;
            }
        }
        Ok(())
    }
}

// Manual Decode implementation for ClientMessage
impl Decode for ClientMessage {
    fn decode<D: Decoder>(decoder: &mut D) -> Result<Self, DecodeError> {
        let tag = u32::decode(decoder)?;
        match tag {
            0 => Ok(ClientMessage::NewOrder(NewOrderRequest::decode(decoder)?)),
            1 => Ok(ClientMessage::CancelOrder(CancelOrderRequest::decode(decoder)?)),
            _ => Err(DecodeError::UnexpectedVariant {
                found: tag,
                type_name: "ClientMessage",
                allowed: &bincode::error::AllowedEnumVariants::Range { min: 0, max: 1 },
            }),
        }
    }
}

// Manual Encode implementation for ServerMessage
impl Encode for ServerMessage {
    fn encode<E: Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        match self {
            ServerMessage::Trade(notif) => {
                0u32.encode(encoder)?;
                notif.encode(encoder)?;
            }
            ServerMessage::Confirmation(conf) => {
                1u32.encode(encoder)?;
                conf.encode(encoder)?;
            }
        }
        Ok(())
    }
}

// Manual Decode implementation for ServerMessage
impl Decode for ServerMessage {
    fn decode<D: Decoder>(decoder: &mut D) -> Result<Self, DecodeError> {
        let tag = u32::decode(decoder)?;
        match tag {
            0 => Ok(ServerMessage::Trade(TradeNotification::decode(decoder)?)),
            1 => Ok(ServerMessage::Confirmation(OrderConfirmation::decode(decoder)?)),
            _ => Err(DecodeError::UnexpectedVariant {
                found: tag,
                type_name: "ServerMessage",
                allowed: &bincode::error::AllowedEnumVariants::Range { min: 0, max: 1 },
            }),
        }
    }
}