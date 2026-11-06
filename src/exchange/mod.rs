//! Exchange core implementation slices.
//!
//! The current slice is a deterministic, single-contract continuous-auction
//! kernel. Gateway/middleware transport and shared-memory integration are
//! deliberately separate later slices; see `docs/exchange-core-architecture.md`.

pub mod auction;
pub mod combo;
pub mod combo_engine;
pub mod combo_exec;
pub mod combo_market_data;
pub mod combo_runtime;
pub mod config;
pub mod core;
pub mod exchange_runtime;
pub mod gateway;
pub mod gateway_runtime;
pub mod gateway_server;
pub mod margin;
pub mod market_data;
pub mod market_data_tracker;
pub mod middleware;
pub mod plugins;
pub mod positions;
pub mod private_events;
pub mod private_replay;
pub mod protocol;
pub mod public_events;
pub mod repair;
pub mod runner;
pub mod schedule;
pub mod session;
pub mod transport;
