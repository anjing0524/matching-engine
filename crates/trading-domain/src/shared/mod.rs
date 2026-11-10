//! Shared types and utilities used by the trading runtime.
//!
//! - [`protocol`]: order/event wire protocol messages
//! - [`collections`]: fast bitmap and other shared collections
//! - [`symbol`]: interned symbols
pub mod collections;
pub mod protocol;
pub mod symbol;

pub use protocol::{
    NewOrderRequest, OrderConfirmation, OrderStatus, OrderStatusType, OrderType, TradeNotification,
};
pub use symbol::SymbolId;
