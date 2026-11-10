//! A high-performance futures matching engine.
//!
//! The crate currently exposes two runtimes:
//!
//! - [`exchange`]: the futures exchange core (gateway/middleware/core,
//!   deterministic matching, combo, snapshots). This is the production path.
//! - the original persistent single-book matching service:
//!   `interfaces → application → domain ← infrastructure`. It remains for
//!   compatibility and its WAL/standby tooling.
//!
//! Do not mix the two stacks. Use [`exchange`] for the exchange deployment and
//! the legacy modules only for the persistent service described in `README.md`.

// Global allocator: jemalloc is typically 8-15% faster than the system
// allocator under high concurrency.
#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

#[cfg(feature = "legacy")]
pub mod application;
pub mod domain;
pub mod exchange;
#[cfg(feature = "legacy")]
pub mod infrastructure;
#[cfg(feature = "legacy")]
pub mod interfaces;
pub mod shared;

#[cfg(feature = "legacy")]
pub use application::services::PersistentMatchingService;
pub use domain::orderbook::{ContractSpec, TickBasedOrderBook};
#[cfg(feature = "legacy")]
pub use infrastructure::network::{TcpConnection, TcpTransport};
pub use shared::protocol::{
    CancelOrderRequest, ClientMessage, ErrorCode, NewOrderRequest, OrderConfirmation, OrderType,
    ServerMessage, TradeNotification,
};
