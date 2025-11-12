/// Domain Layer - Core Business Logic
///
/// This is the heart of the matching engine, containing pure business logic
/// with zero external dependencies. The domain layer is framework-agnostic
/// and can be tested in isolation.
///
/// ## Modules
/// - `orderbook`: Order book implementations (TickBasedOrderBook is production)
/// - `matching`: Matching algorithms and rules
/// - `entities`: Domain entities (Order, Trade, etc.)
///
/// ## Principles
/// 1. **Pure Business Logic**: No I/O, no frameworks, no infrastructure
/// 2. **Framework Independent**: Can be used with any I/O or framework
/// 3. **Testable**: Easy to unit test without mocks
/// 4. **Performance Critical**: Highly optimized, zero-allocation where possible

pub mod orderbook;
pub mod matching;
pub mod entities;

// Re-export key types
pub use orderbook::{TickBasedOrderBook, ContractSpec, OrderNode};
