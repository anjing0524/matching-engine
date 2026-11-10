//! A high-performance futures exchange core.
//!
//! The production path is the [`exchange`] module: gateway/middleware/core,
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

pub mod domain;
pub mod exchange;
pub mod shared;

pub use domain::orderbook::{ContractSpec, TickBasedOrderBook};
pub use shared::protocol::{NewOrderRequest, OrderConfirmation, OrderStatus, OrderStatusType, OrderType, TradeNotification};
