/// Domain Layer - OrderBook Module
///
/// Contains the core orderbook implementations for the matching engine.
/// This is the heart of the domain logic - pure business rules with no
/// external dependencies.
///
/// ## Production Implementation
/// - `TickBasedOrderBook` (V3): Array-based O(1) tick orderbook with FastBitmap
///   - Performance: 9.34M ops/s
///   - Optimized for futures/derivatives with discrete price ticks
///   - Uses hardware instructions (POPCNT/TZCNT) for best bid/ask
///
/// ## Trait Abstraction
/// The `OrderBook` trait provides a common interface for different implementations,
/// enabling dependency injection and testing.

pub mod tick_based;

// Re-export the production implementation
pub use tick_based::{TickBasedOrderBook, ContractSpec, OrderNode};
