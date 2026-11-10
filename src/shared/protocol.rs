//! Order-book protocol types shared by the exchange kernel and tests.

use crate::shared::SymbolId;
use serde::{Deserialize, Serialize};

/// Order side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum OrderType {
    #[default]
    Buy,
    Sell,
}

/// Resting-order state reported by [`OrderStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderStatusType {
    /// Order is in the book with its full quantity.
    Resting,
    /// Part of the order has already filled.
    PartiallyFilled,
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
    /// Price in integer ticks.
    #[serde(default)]
    pub price: u64,
    #[serde(default)]
    pub quantity: u64,
}

/// Confirmation that an order rests in the book.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderConfirmation {
    pub order_id: u64,
    pub user_id: u64,
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
