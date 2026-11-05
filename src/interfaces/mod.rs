//! Interfaces Layer - External Entry Points.
//!
//! This layer contains all external interfaces to the system:
//! CLI, REST API, gRPC, WebSocket, etc.
//!
//! ## Modules
//! - [`cli`]: configuration, wiring, and signals for the `matching-engine` binary
//! - [`gateway`]: accept loop, connection registry, limits, shutdown/fatal control
//! - [`session`]: wire framing, order submission, reply encoding, latency metrics
pub mod cli;
pub mod engine_ipc;
pub mod gateway;
pub mod replication;
pub mod session;
