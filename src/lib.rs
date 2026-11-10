//! A high-performance futures trading core.
//!
//! The production path is the [`trading`] module: gateway/middleware/core,
//! deterministic matching, combo execution, snapshots, and market-data
//! distribution. [`domain`] provides the order book; [`shared`] provides wire
//! types and the SPSC ring.

// Global allocator: jemalloc is typically 8-15% faster than the system
// allocator under high concurrency.
#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

pub mod trading;
pub use trading_domain::{domain, shared};

pub use domain::orderbook::{ContractSpec, TickBasedOrderBook};
pub use shared::protocol::{
    NewOrderRequest, OrderConfirmation, OrderStatus, OrderStatusType, OrderType, TradeNotification,
};
