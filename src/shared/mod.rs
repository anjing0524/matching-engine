pub mod collections;
pub mod metrics;
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

// Re-export commonly used types
pub use protocol::{
    CancelOrderRequest, ClientMessage, NewOrderRequest, OrderConfirmation, OrderType,
    ServerMessage, TradeNotification,
};

pub use symbol_pool::SymbolPool;
pub use timestamp::get_fast_timestamp;
