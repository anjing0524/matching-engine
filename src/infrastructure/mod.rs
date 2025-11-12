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
/// - `persistence`: Database, message queue (future)

pub mod network;
pub mod telemetry;

// Re-export key types
pub use network::{NetworkTransport, Connection, ZeroCopyBuffer};
pub use network::{BackendType, MiddlewareConfig, NetworkMiddleware};
