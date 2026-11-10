//! Composition of gateway, core, combo, and output sinks.

pub mod event_fanout;
pub mod runtime;

pub use self::runtime::{RuntimeOptions, TradingRuntime, TradingSnapshot};
