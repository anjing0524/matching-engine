//! Deterministic trading state: book, orders, positions, auction, and risk plugins.

pub mod auction;
pub mod kernel;
pub mod plugins;
pub mod positions;
pub mod private_events;
pub mod protocol;
pub mod public_events;
pub mod registry;
pub mod schedule;

pub use self::kernel::{CoreEvent, CoreSnapshot, TradingCore};
pub use self::registry::{InstrumentRegistry, RegistryError};
