//! Domain layer: core business logic with no I/O.
//!
//! - [`orderbook`]: the production order book implementation
//!
//! Order validity is enforced by [`orderbook::TickBasedOrderBook::validate_order`]
//! before anything is persisted.
pub mod orderbook;

pub use orderbook::{ContractSpec, OrderNode, TickBasedOrderBook};
