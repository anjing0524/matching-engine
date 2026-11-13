//! Composition of gateway, core, combo, and output sinks.

pub mod engine;
pub(crate) mod event_fanout;
pub mod snapshot;

pub use self::engine::{RuntimeOptions, TradingRuntime};
pub use self::snapshot::{TradingSnapshot, TradingSnapshotData};
