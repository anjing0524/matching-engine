//! Futures trading core.
//!
//! Ownership boundaries:
//!
//! - `core` / `auction` / `combo_*` / `positions`: deterministic business state;
//!   no sockets, no wall clock.
//! - `gateway_*` / `session` / `auth`: member sessions and command ingress.
//! - `middleware` / `runner` / `transport`: sequencing, ring transport, and
//!   command/result logs.
//! - `event_fanout` / `market_data` / `repair` / `metrics`: output projection.
//! - `runtime` / `combo_runtime`: composition and tick driving.
//!
//! See `docs/architecture-map.md` for the full map.

pub mod auction;
pub mod auth;
pub mod combo;
pub mod combo_engine;
pub mod combo_exec;
pub mod combo_market_data;
pub mod combo_runtime;
pub mod command_log;
pub mod config;
pub mod core;
pub mod event_fanout;
pub mod gateway;
pub mod gateway_directory;
pub mod gateway_proxy;
pub mod gateway_runtime;
pub mod gateway_server;
pub mod margin;
pub mod market_data;
pub mod market_data_tracker;
pub mod metrics;
pub mod metrics_server;
pub mod middleware;
pub mod plugins;
pub mod positions;
pub mod private_events;
pub mod private_replay;
pub mod protocol;
pub mod public_events;
pub mod rate_limit;
pub mod repair;
pub mod replay;
pub mod result_log;
pub mod runner;
pub mod runtime;
pub mod schedule;
pub mod session;
pub mod snapshot_file;
pub mod transport;
