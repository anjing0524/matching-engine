/// Domain Layer - OrderBook Module
///
/// Contains the core orderbook implementations for the matching engine.
/// This is the heart of the domain logic - pure business rules with no
/// external dependencies.
///
/// ## Trait Abstraction
/// - `OrderBook` trait: Common interface for all orderbook implementations
///   - Enables dependency injection and polymorphism
///   - Zero-cost abstraction (monomorphized at compile time)
///   - Easy to mock for testing
///
/// ## Production Implementation
/// - `TickBasedOrderBook` (V3): Array-based O(1) tick orderbook with FastBitmap
///   - Performance: 9.34M ops/s
///   - Optimized for futures/derivatives with discrete price ticks
///   - Uses hardware instructions (POPCNT/TZCNT) for best bid/ask
///   - Implements the `OrderBook` trait
///
/// ## Usage Example
/// ```rust
/// use matching_engine::domain::orderbook::{OrderBook, TickBasedOrderBook, ContractSpec};
///
/// // Generic function that works with any OrderBook implementation
/// fn process_orders<OB: OrderBook>(orderbook: &mut OB) {
///     // ...
/// }
///
/// // Use with concrete implementation
/// let spec = ContractSpec::new("BTC/USD", 1, 10000, 100000);
/// let mut ob = TickBasedOrderBook::new(spec);
/// process_orders(&mut ob);
/// ```

pub mod tick_based;
pub mod traits;

// Re-export the trait
pub use traits::OrderBook;

// Re-export the production implementation
pub use tick_based::{TickBasedOrderBook, ContractSpec, OrderNode};
