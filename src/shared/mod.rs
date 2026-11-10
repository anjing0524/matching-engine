//! Shared types and utilities used by the exchange runtime.
//!
//! - [`protocol`]: order/event wire protocol messages
//! - [`collections`]: fast bitmap and other shared collections
//! - [`spsc_ring`]: shared-memory SPSC ring
//! - [`symbol`]: interned symbols
pub mod collections;
pub mod protocol;
pub mod spsc_ring;
pub mod symbol;

pub use protocol::{
    NewOrderRequest, OrderConfirmation, OrderStatus, OrderStatusType, OrderType, TradeNotification,
};
pub use symbol::SymbolId;
