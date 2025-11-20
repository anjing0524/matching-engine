pub mod circuit_breaker;
pub mod collections;
pub mod metrics;
/// Shared utilities and types used across all layers
///
/// This module contains:
/// - Protocol definitions (messages, orders, trades)
/// - Common data structures (symbol pool)
/// - Utilities (timestamp, collections)
/// - Trace context for distributed tracing
/// - Retry mechanisms with exponential backoff
/// - Circuit breaker pattern for fault tolerance
/// - Error types
pub mod protocol;
pub mod retry;
pub mod symbol_pool;
pub mod timestamp;
pub mod trace_context;

// Re-export commonly used types
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};
pub use protocol::{
    CancelOrderRequest, ClientMessage, NewOrderRequest, OrderConfirmation, OrderType,
    ServerMessage, TradeNotification,
};

pub use retry::{retry_async, retry_with_condition, retry_with_policy, RetryPolicy};
pub use symbol_pool::SymbolPool;
pub use timestamp::get_fast_timestamp;
pub use trace_context::TraceContext;
