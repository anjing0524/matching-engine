//! Infrastructure layer: technical implementations.
//!
//! - [`network`]: blocking TCP transport (`shared::wire` for framing)
//! - [`persistence`]: write-ahead log and crash recovery
//! - [`observability`]: HTTP metrics server and health checks
pub mod network;
pub mod observability;
pub mod persistence;

pub use network::{TcpConnection, TcpTransport};
pub use persistence::{Journal, JournalRecovery, PersistenceConfig, WalEntry};
