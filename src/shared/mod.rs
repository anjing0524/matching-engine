/// Shared utilities and types used across all layers
///
/// This module contains:
/// - Protocol definitions (messages, orders, trades)
/// - Common data structures (symbol pool)
/// - Utilities (timestamp, collections)
/// - Error types

pub mod protocol;
pub mod symbol_pool;
pub mod timestamp;
pub mod collections;
pub mod metrics;

// Re-export commonly used types
pub use protocol::{
    ClientMessage, ServerMessage,
    NewOrderRequest, CancelOrderRequest,
    OrderType, OrderConfirmation, TradeNotification,
};

pub use symbol_pool::SymbolPool;
pub use timestamp::get_fast_timestamp;
