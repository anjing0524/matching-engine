//! Shared types and utilities used by the exchange runtime.
//!
//! - [`protocol`]: order/event wire protocol messages
//! - [`collections`]: fast bitmap and other shared collections
//! - [`wire`]: compact binary codec
//! - [`spsc_ring`]: shared-memory SPSC ring
//! - [`symbol`]: interned symbols
pub mod collections;
pub mod protocol;
pub mod spsc_ring;
pub mod symbol;
pub mod wire;

pub use protocol::{
    CancelOrderRequest, ClientMessage, ClientRequest, ErrorCode, NewOrderRequest,
    OrderConfirmation, OrderType, PROTOCOL_VERSION, ServerMessage, ServerResponse,
    TradeNotification,
};
pub use symbol::SymbolId;
