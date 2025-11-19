/// Infrastructure Layer - Technical Implementations
///
/// This layer contains all technical implementations that interact with
/// external systems: network I/O, persistence, telemetry, etc.
///
/// The infrastructure layer depends on the domain layer but the domain
/// layer does not depend on infrastructure (dependency inversion).
///
/// ## Modules
/// - `network`: High-performance network middleware with multiple backends
/// - `telemetry`: Metrics, tracing, logging
/// - `persistence`: WAL, snapshots, and crash recovery
/// - `observability`: HTTP metrics server and health checks
pub mod network;
pub mod observability;
pub mod persistence;
pub mod telemetry;

// Re-export key types
pub use network::{BackendType, MiddlewareConfig, NetworkMiddleware};
pub use network::{Connection, NetworkTransport, ZeroCopyBuffer};
pub use persistence::{PersistenceConfig, SnapshotData, WalEntry};
pub use persistence::{RecoveryManager, SnapshotReader, SnapshotWriter, WalWriter};
