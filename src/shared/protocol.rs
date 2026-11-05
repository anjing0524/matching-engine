//! Wire protocol messages.
//!
//! Messages use the compact binary format from [`crate::shared::wire`] on the
//! network and in the WAL. `serde` derives are kept for JSON-based tooling and
//! tests; they are not used in the hot path.
use crate::shared::symbol::SymbolId;
use crate::shared::wire::{WireDecode, WireEncode, WireError, WireReader, WireWrite};
use serde::{Deserialize, Serialize};

/// Order side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum OrderType {
    #[default]
    Buy,
    Sell,
}

impl OrderType {
    fn tag(self) -> u8 {
        match self {
            Self::Buy => 0,
            Self::Sell => 1,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, WireError> {
        match tag {
            0 => Ok(Self::Buy),
            1 => Ok(Self::Sell),
            _ => Err(WireError::InvalidTag {
                type_name: "OrderType",
                tag,
            }),
        }
    }
}

/// Resting-order state reported by [`OrderStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderStatusType {
    /// Order is in the book with its full quantity.
    Resting,
    /// Order is in the book with part of its quantity already filled.
    PartiallyFilled,
}

impl OrderStatusType {
    fn tag(self) -> u8 {
        match self {
            Self::Resting => 0,
            Self::PartiallyFilled => 1,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, WireError> {
        match tag {
            0 => Ok(Self::Resting),
            1 => Ok(Self::PartiallyFilled),
            _ => Err(WireError::InvalidTag {
                type_name: "OrderStatusType",
                tag,
            }),
        }
    }
}

/// Full status of a resting order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderStatus {
    pub order_id: u64,
    pub user_id: u64,
    pub side: OrderType,
    pub price: u64,
    pub original_quantity: u64,
    pub remaining_quantity: u64,
    pub status: OrderStatusType,
}

impl WireEncode for OrderStatus {
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        out.put_u64(self.order_id);
        out.put_u64(self.user_id);
        out.put_u8(self.side.tag());
        out.put_u64(self.price);
        out.put_u64(self.original_quantity);
        out.put_u64(self.remaining_quantity);
        out.put_u8(self.status.tag());
        Ok(())
    }
}

impl WireDecode for OrderStatus {
    fn decode(reader: &mut WireReader<'_>) -> Result<Self, WireError> {
        Ok(Self {
            order_id: reader.read_u64()?,
            user_id: reader.read_u64()?,
            side: OrderType::from_tag(reader.read_u8()?)?,
            price: reader.read_u64()?,
            original_quantity: reader.read_u64()?,
            remaining_quantity: reader.read_u64()?,
            status: OrderStatusType::from_tag(reader.read_u8()?)?,
        })
    }
}

/// A new limit order.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NewOrderRequest {
    #[serde(default)]
    pub user_id: u64,
    /// Client-assigned idempotency key. `0` disables duplicate detection.
    #[serde(default)]
    pub client_order_id: u64,
    #[serde(default)]
    pub symbol: SymbolId,
    #[serde(default)]
    pub order_type: OrderType,
    /// Price in integer ticks (for example `12345` for `123.45`).
    #[serde(default)]
    pub price: u64,
    #[serde(default)]
    pub quantity: u64,
}

impl WireEncode for NewOrderRequest {
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        out.put_u64(self.user_id);
        out.put_u64(self.client_order_id);
        write_symbol(self.symbol, out)?;
        out.put_u8(self.order_type.tag());
        out.put_u64(self.price);
        out.put_u64(self.quantity);
        Ok(())
    }
}

impl WireDecode for NewOrderRequest {
    fn decode(reader: &mut WireReader<'_>) -> Result<Self, WireError> {
        Ok(Self {
            user_id: reader.read_u64()?,
            client_order_id: reader.read_u64()?,
            symbol: read_symbol(reader)?,
            order_type: OrderType::from_tag(reader.read_u8()?)?,
            price: reader.read_u64()?,
            quantity: reader.read_u64()?,
        })
    }
}

fn write_symbol(symbol: SymbolId, out: &mut Vec<u8>) -> Result<(), WireError> {
    match symbol.with_name(|name| out.put_str(name)) {
        Some(result) => result,
        None => Err(WireError::UnknownSymbol(symbol.raw())),
    }
}

fn read_symbol(reader: &mut WireReader<'_>) -> Result<SymbolId, WireError> {
    Ok(SymbolId::intern(reader.read_str()?))
}

/// A cancel request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelOrderRequest {
    pub user_id: u64,
    pub order_id: u64,
}

impl WireEncode for CancelOrderRequest {
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        out.put_u64(self.user_id);
        out.put_u64(self.order_id);
        Ok(())
    }
}

impl WireDecode for CancelOrderRequest {
    fn decode(reader: &mut WireReader<'_>) -> Result<Self, WireError> {
        Ok(Self {
            user_id: reader.read_u64()?,
            order_id: reader.read_u64()?,
        })
    }
}

/// Confirmation that an order rests in the book.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderConfirmation {
    pub order_id: u64,
    pub user_id: u64,
}

impl WireEncode for OrderConfirmation {
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        out.put_u64(self.order_id);
        out.put_u64(self.user_id);
        Ok(())
    }
}

impl WireDecode for OrderConfirmation {
    fn decode(reader: &mut WireReader<'_>) -> Result<Self, WireError> {
        Ok(Self {
            order_id: reader.read_u64()?,
            user_id: reader.read_u64()?,
        })
    }
}

/// A trade produced by the matching engine.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TradeNotification {
    #[serde(default)]
    pub trade_id: u64,
    #[serde(default)]
    pub symbol: SymbolId,
    #[serde(default)]
    pub matched_price: u64,
    #[serde(default)]
    pub matched_quantity: u64,
    #[serde(default)]
    pub buyer_user_id: u64,
    #[serde(default)]
    pub buyer_order_id: u64,
    #[serde(default)]
    pub seller_user_id: u64,
    #[serde(default)]
    pub seller_order_id: u64,
    #[serde(default)]
    pub timestamp: u64,
}

impl WireEncode for TradeNotification {
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        out.put_u64(self.trade_id);
        write_symbol(self.symbol, out)?;
        out.put_u64(self.matched_price);
        out.put_u64(self.matched_quantity);
        out.put_u64(self.buyer_user_id);
        out.put_u64(self.buyer_order_id);
        out.put_u64(self.seller_user_id);
        out.put_u64(self.seller_order_id);
        out.put_u64(self.timestamp);
        Ok(())
    }
}

impl WireDecode for TradeNotification {
    fn decode(reader: &mut WireReader<'_>) -> Result<Self, WireError> {
        Ok(Self {
            trade_id: reader.read_u64()?,
            symbol: read_symbol(reader)?,
            matched_price: reader.read_u64()?,
            matched_quantity: reader.read_u64()?,
            buyer_user_id: reader.read_u64()?,
            buyer_order_id: reader.read_u64()?,
            seller_user_id: reader.read_u64()?,
            seller_order_id: reader.read_u64()?,
            timestamp: reader.read_u64()?,
        })
    }
}

/// Client-to-server message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    NewOrder(NewOrderRequest),
    CancelOrder(CancelOrderRequest),
}

impl WireEncode for ClientMessage {
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        match self {
            Self::NewOrder(order) => {
                out.put_u8(0);
                order.encode(out)
            }
            Self::CancelOrder(cancel) => {
                out.put_u8(1);
                cancel.encode(out)
            }
        }
    }
}

impl WireDecode for ClientMessage {
    fn decode(reader: &mut WireReader<'_>) -> Result<Self, WireError> {
        match reader.read_u8()? {
            0 => Ok(Self::NewOrder(NewOrderRequest::decode(reader)?)),
            1 => Ok(Self::CancelOrder(CancelOrderRequest::decode(reader)?)),
            tag => Err(WireError::InvalidTag {
                type_name: "ClientMessage",
                tag,
            }),
        }
    }
}

/// Current wire protocol version.
pub const PROTOCOL_VERSION: u16 = 2;

/// Correlation envelope for one client request.
#[derive(Debug, Clone)]
pub struct ClientRequest {
    pub version: u16,
    pub request_id: u64,
    pub message: ClientMessage,
}

impl WireEncode for ClientRequest {
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        out.put_u16(self.version);
        out.put_u64(self.request_id);
        self.message.encode(out)
    }
}

impl WireDecode for ClientRequest {
    fn decode(reader: &mut WireReader<'_>) -> Result<Self, WireError> {
        Ok(Self {
            version: reader.read_u16()?,
            request_id: reader.read_u64()?,
            message: ClientMessage::decode(reader)?,
        })
    }
}

/// Correlation envelope for one server response.
#[derive(Debug, Clone)]
pub struct ServerResponse {
    pub version: u16,
    pub request_id: u64,
    pub message: ServerMessage,
}

impl WireEncode for ServerResponse {
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        out.put_u16(self.version);
        out.put_u64(self.request_id);
        self.message.encode(out)
    }
}

impl WireDecode for ServerResponse {
    fn decode(reader: &mut WireReader<'_>) -> Result<Self, WireError> {
        Ok(Self {
            version: reader.read_u16()?,
            request_id: reader.read_u64()?,
            message: ServerMessage::decode(reader)?,
        })
    }
}

/// Stable error category for rejected requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The order was rejected (validation, duplicate, queue full, ...).
    OrderRejected,
    /// The cancellation was rejected (unknown/unauthorized, ...).
    CancelRejected,
    /// The request used an unsupported protocol version.
    ProtocolVersion,
    /// Internal failure cannot be represented as a normal rejection.
    Internal,
}

impl ErrorCode {
    const fn tag(self) -> u8 {
        match self {
            Self::OrderRejected => 0,
            Self::CancelRejected => 1,
            Self::ProtocolVersion => 2,
            Self::Internal => 3,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, WireError> {
        match tag {
            0 => Ok(Self::OrderRejected),
            1 => Ok(Self::CancelRejected),
            2 => Ok(Self::ProtocolVersion),
            3 => Ok(Self::Internal),
            tag => Err(WireError::InvalidTag {
                type_name: "ErrorCode",
                tag,
            }),
        }
    }
}

/// Server-to-client message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    Trade(TradeNotification),
    Confirmation(OrderConfirmation),
    /// The request was rejected before matching.
    Rejected {
        code: ErrorCode,
        message: String,
    },
    /// A cancellation was accepted.
    CancelAcknowledged(OrderConfirmation),
}

impl WireEncode for ServerMessage {
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        match self {
            Self::Trade(trade) => {
                out.put_u8(0);
                trade.encode(out)
            }
            Self::Confirmation(confirmation) => {
                out.put_u8(1);
                confirmation.encode(out)
            }
            Self::Rejected { code, message } => {
                out.put_u8(2);
                out.put_u8(code.tag());
                out.put_str(message)
            }
            Self::CancelAcknowledged(confirmation) => {
                out.put_u8(3);
                confirmation.encode(out)
            }
        }
    }
}

impl WireDecode for ServerMessage {
    fn decode(reader: &mut WireReader<'_>) -> Result<Self, WireError> {
        match reader.read_u8()? {
            0 => Ok(Self::Trade(TradeNotification::decode(reader)?)),
            1 => Ok(Self::Confirmation(OrderConfirmation::decode(reader)?)),
            2 => Ok(Self::Rejected {
                code: ErrorCode::from_tag(reader.read_u8()?)?,
                message: reader.read_string()?,
            }),
            3 => Ok(Self::CancelAcknowledged(OrderConfirmation::decode(reader)?)),
            tag => Err(WireError::InvalidTag {
                type_name: "ServerMessage",
                tag,
            }),
        }
    }
}

// Serde module for `Arc<str>` in JSON representations.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::wire::{decode, encode_to_vec};

    fn roundtrip<T>(value: &T) -> T
    where
        T: WireEncode + WireDecode,
    {
        let encoded = encode_to_vec(value).expect("encode failed");
        decode(&encoded).expect("decode failed")
    }

    fn trade() -> TradeNotification {
        TradeNotification {
            trade_id: 1,
            symbol: crate::shared::SymbolId::intern("BTCUSDT"),
            matched_price: 50_000,
            matched_quantity: 3,
            buyer_user_id: 1,
            buyer_order_id: 1,
            seller_user_id: 2,
            seller_order_id: 2,
            timestamp: 10,
        }
    }

    #[test]
    fn test_client_message_roundtrip() {
        let order = NewOrderRequest {
            user_id: 9,
            client_order_id: 77,
            symbol: crate::shared::SymbolId::intern("BTCUSDT"),
            order_type: OrderType::Sell,
            price: 50_001,
            quantity: 7,
        };

        match roundtrip(&ClientMessage::NewOrder(order)) {
            ClientMessage::NewOrder(decoded) => {
                assert_eq!(decoded.user_id, 9);
                assert_eq!(decoded.client_order_id, 77);
                assert_eq!(decoded.symbol, SymbolId::intern("BTCUSDT"));
                assert_eq!(decoded.order_type, OrderType::Sell);
                assert_eq!(decoded.price, 50_001);
                assert_eq!(decoded.quantity, 7);
            }
            ClientMessage::CancelOrder(_) => panic!("expected new order"),
        }

        let cancel = CancelOrderRequest {
            user_id: 9,
            order_id: 42,
        };
        match roundtrip(&ClientMessage::CancelOrder(cancel)) {
            ClientMessage::CancelOrder(decoded) => {
                assert_eq!(decoded.order_id, 42);
                assert_eq!(decoded.user_id, 9);
            }
            ClientMessage::NewOrder(_) => panic!("expected cancel"),
        }
    }

    #[test]
    fn test_server_message_roundtrip() {
        assert!(matches!(
            roundtrip(&ServerMessage::Trade(trade())),
            ServerMessage::Trade(_)
        ));

        let confirmation = OrderConfirmation {
            order_id: 7,
            user_id: 9,
        };
        assert!(matches!(
            roundtrip(&ServerMessage::Confirmation(confirmation)),
            ServerMessage::Confirmation(_)
        ));

        match roundtrip(&ServerMessage::Rejected {
            code: ErrorCode::OrderRejected,
            message: "bad price".to_string(),
        }) {
            ServerMessage::Rejected { code, message } => {
                assert_eq!(code, ErrorCode::OrderRejected);
                assert_eq!(message, "bad price");
            }
            _ => panic!("expected rejection"),
        }

        let cancel_ack = OrderConfirmation {
            order_id: 11,
            user_id: 12,
        };
        match roundtrip(&ServerMessage::CancelAcknowledged(cancel_ack)) {
            ServerMessage::CancelAcknowledged(ack) => {
                assert_eq!(ack.order_id, 11);
                assert_eq!(ack.user_id, 12);
            }
            _ => panic!("expected cancel acknowledgement"),
        }
    }

    #[test]
    fn test_invalid_tag_is_rejected() {
        let error = decode::<ClientMessage>(&[9]).unwrap_err();
        assert!(matches!(
            error,
            WireError::InvalidTag {
                type_name: "ClientMessage",
                tag: 9
            }
        ));
    }

    #[test]
    fn test_trailing_bytes_are_rejected() {
        let mut encoded = encode_to_vec(&ServerMessage::Trade(trade())).unwrap();
        encoded.push(0xFF);
        assert!(decode::<ServerMessage>(&encoded).is_err());
    }
    #[test]
    fn test_protocol_envelope_roundtrip() {
        let request = ClientRequest {
            version: PROTOCOL_VERSION,
            request_id: 42,
            message: ClientMessage::CancelOrder(CancelOrderRequest {
                user_id: 1,
                order_id: 9,
            }),
        };
        let decoded = roundtrip(&request);
        assert_eq!(decoded.version, PROTOCOL_VERSION);
        assert_eq!(decoded.request_id, 42);
        assert!(matches!(decoded.message, ClientMessage::CancelOrder(_)));

        let response = ServerResponse {
            version: PROTOCOL_VERSION,
            request_id: 42,
            message: ServerMessage::Rejected {
                code: ErrorCode::CancelRejected,
                message: "bad".to_string(),
            },
        };
        let decoded = roundtrip(&response);
        assert_eq!(decoded.request_id, 42);
        assert!(matches!(decoded.message, ServerMessage::Rejected { .. }));
    }
}
