//! Composition of gateway, core, combo, and output sinks.

pub mod event_fanout;
pub mod engine;

pub use self::engine::{RuntimeOptions, TradingRuntime, TradingSnapshot};
