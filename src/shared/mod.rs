//! Shared types and utilities used across layers.
//!
//! - [`protocol`]: wire protocol messages
//! - [`collections`]: fast bitmap and other shared collections
//! - [`timestamp`]: batched timestamp reads
//! - [`metrics`]: Prometheus metrics
//! - [`wire`]: compact binary codec used by the network and the WAL
//! - [`latency`]: nanosecond latency percentiles
pub mod collections;
pub mod latency;
pub mod metrics;
pub mod protocol;
pub mod symbol;
pub mod timestamp;
pub mod wire;

pub use protocol::{
    CancelOrderRequest, ClientMessage, ClientRequest, ErrorCode, NewOrderRequest,
    OrderConfirmation, OrderType, PROTOCOL_VERSION, ServerMessage, ServerResponse,
    TradeNotification,
};
pub use symbol::SymbolId;
pub use timestamp::get_fast_timestamp;
