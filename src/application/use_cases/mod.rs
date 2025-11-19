pub mod cancel_order;
/// Use Cases - High-level business operations
///
/// Each use case represents a specific business operation that the
/// application can perform. Use cases orchestrate domain entities and
/// services to achieve application goals.
///
/// ## Available Use Cases
/// - `MatchOrderUseCase`: Handles new order matching logic
/// - `CancelOrderUseCase`: Handles order cancellation logic
///
/// ## Future Use Cases
/// - Query depth/orderbook state
/// - Batch order operations
/// - Market data queries
pub mod match_order;

// Re-export key types
pub use cancel_order::{CancelOrderResult, CancelOrderUseCase};
pub use match_order::{MatchOrderResult, MatchOrderUseCase};
