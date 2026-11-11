//! Composition of gateway, core, combo, and output sinks.

pub mod engine;
pub(crate) mod event_fanout;

pub use self::engine::{RuntimeOptions, TradingRuntime, TradingSnapshot};
