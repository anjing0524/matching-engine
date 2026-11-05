//! Exchange core implementation slices.
//!
//! The current slice is a deterministic, single-contract continuous-auction
//! kernel. Gateway/middleware transport and shared-memory integration are
//! deliberately separate later slices; see `docs/exchange-core-architecture.md`.

pub mod core;
pub mod plugins;
pub mod positions;
pub mod protocol;
