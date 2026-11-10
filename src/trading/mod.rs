//! Trading system engine.
//!
//! Grouped by responsibility:
//!
//! - `core`: deterministic book/positions/auction/risk state.
//! - `gateway`: member sessions and private downlink.
//! - `transport`: sequencing, shared-memory rings, and logs.
//! - `market_data`: public publication, gap tracking, and repair.
//! - `combo`: combination queue, leg execution, margin, and runtime.
//! - `snapshot`: versioned persistence and replay.
//! - `runtime`: composition and tick driving.
//!
//! See `docs/architecture-map.md` for the full map.

pub mod combo;
pub mod config;
pub mod core;
pub mod gateway;
pub mod market_data;
pub mod runtime;
pub mod snapshot;
pub mod transport;
