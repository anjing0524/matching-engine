//! Deterministic trading state: book, orders, positions, auction, and risk plugins.

pub mod auction;
pub mod combo_book;
pub mod combo_margin;
pub mod combo_pair;
pub mod kernel;
pub mod leg_liquidity;
pub mod plugins;
pub mod positions;
pub mod private_events;
pub mod protocol;
pub mod public_events;
pub mod registry;
pub mod schedule;

pub use self::combo_book::{
    ComboBook, ComboBookError, ComboBookSnapshot, ComboCommandError, ComboEvent, ComboFill,
    ComboOrder, ComboOrderOwner, ComboPrivateFill, ImpliedComboFill,
};
pub use self::combo_margin::{
    ComboMarginError, ComboMarginFreeze, ComboMarginSnapshot, ComboMarginSnapshotError, LegMargin,
};
pub use self::combo_pair::{ComboLeg, ComboPair, ComboPairError, ComboPairMargin};
pub use self::kernel::{CoreEvent, CoreSnapshot, TradingCore};
pub use self::leg_liquidity::{
    ComboLegError, ComboLegExecution, LegExecutionContext, LegFill, LegLiquidityError,
    LegLiquidityExecution, LegLiquidityPlan, PlannedLegFill,
};
pub use self::registry::{
    InstrumentRegistry, RegistryError, RegistrySnapshot, RegistrySnapshotError,
};
