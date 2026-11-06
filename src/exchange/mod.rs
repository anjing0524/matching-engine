//! Exchange core implementation slices.
//!
//! The current slice is a deterministic, single-contract continuous-auction
//! kernel. Gateway/middleware transport and shared-memory integration are
//! deliberately separate later slices; see `docs/exchange-core-architecture.md`.

pub mod auction;
pub mod combo;
pub mod core;
pub mod middleware;
pub mod plugins;
pub mod positions;
pub mod private_events;
pub mod protocol;
pub mod public_events;
pub mod runner;
pub mod transport;
