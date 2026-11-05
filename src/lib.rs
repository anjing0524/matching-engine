//! A high-performance futures matching engine.
//!
//! The crate is organized in layers with a one-way dependency flow:
//!
//! ```text
//! interfaces → application → domain ← infrastructure
//! ```
//!
//! - [`domain`]: the order book and order validation (no I/O)
//! - [`application`]: services that combine the order book with persistence
//! - [`infrastructure`]: networking, WAL persistence, and observability
//! - [`interfaces`]: CLI entry points
//! - [`shared`]: protocol types and low-level utilities

// Global allocator: jemalloc is typically 8-15% faster than the system
// allocator under high concurrency.
#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod interfaces;
pub mod shared;

pub use application::services::PersistentMatchingService;
pub use domain::orderbook::{ContractSpec, TickBasedOrderBook};
pub use infrastructure::network::{TcpConnection, TcpTransport};
pub use shared::protocol::{
    CancelOrderRequest, ClientMessage, ErrorCode, NewOrderRequest, OrderConfirmation, OrderType,
    ServerMessage, TradeNotification,
};
