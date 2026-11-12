//! Core-owned plan/consume seam for resting leg-book liquidity.
//!
//! A combo matcher must never mutate an ordinary Contract book directly. This
//! module defines the narrow vocabulary the core exposes instead: a
//! non-mutating [`LegLiquidityPlan`] for one leg, its atomic consumption, and
//! the two-leg [`ComboLegExecution`] that proves both books moved together.
//!
//! A plan is sealed against external construction: callers obtain one from
//! [`TradingCore::plan_leg_liquidity`](crate::trading::core::TradingCore::plan_leg_liquidity)
//! and consumption re-validates it against the current book before any
//! mutation. A rejected plan therefore leaves the book, positions, and event
//! buffers untouched.

use crate::trading::core::combo_pair::ComboLeg;
use crate::trading::core::protocol::Side;
use std::error::Error;
use std::fmt::{Display, Formatter, Result as FmtResult};

/// One resting order selected by a [`LegLiquidityPlan`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PlannedLegFill {
    /// Exchange id of the resting order to consume.
    pub order_id: u64,
    /// Account that owns the resting order.
    pub account_id: u64,
    /// Resting price of the selected order.
    pub price_ticks: u64,
    /// Quantity selected from this order.
    pub quantity: u64,
}

/// Non-mutating, all-or-none selection of resting liquidity on one leg.
///
/// A plan covers exactly `quantity` and only orders that cross
/// `limit_price_ticks` for `taker_side`, in price then time priority. It is
/// produced by the core and consumed by the core; external code cannot forge a
/// plan, and consumption rejects a stale or tampered one.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct LegLiquidityPlan {
    /// Aggressive side the combo matcher would take on this leg book.
    pub taker_side: Side,
    /// Worst leg price the plan is allowed to cross.
    pub limit_price_ticks: u64,
    /// Total quantity the plan selects.
    pub quantity: u64,
    /// Resting orders consumed by the plan, in execution order.
    pub fills: Vec<PlannedLegFill>,
}

impl LegLiquidityPlan {
    /// Returns the worst price touched by the planned fills.
    #[must_use]
    pub fn worst_price_ticks(&self) -> Option<u64> {
        self.fills.last().map(|fill| fill.price_ticks)
    }
}

/// Identity attached to one leg execution for public trade projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct LegExecutionContext {
    /// Contract instrument id carried by public leg trades.
    pub instrument_id: u32,
    /// Combo or derived leg order reported as the aggressor on each leg trade.
    pub taker_order_id: u64,
}

impl LegExecutionContext {
    /// Creates the execution identity for one leg.
    #[must_use]
    pub const fn new(instrument_id: u32, taker_order_id: u64) -> Self {
        Self {
            instrument_id,
            taker_order_id,
        }
    }
}

/// One resting order consumed by a leg-liquidity execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct LegFill {
    /// Exchange id of the resting order that filled.
    pub order_id: u64,
    /// Account that owned the resting order.
    pub account_id: u64,
    /// Side of the resting order.
    pub side: Side,
    /// Price of the resting order.
    pub price_ticks: u64,
    /// Quantity filled on this order.
    pub quantity: u64,
    /// Quantity still resting on this order after the fill.
    pub remaining: u64,
    /// Public trade id assigned to this fill.
    pub trade_id: u64,
}

/// Result of consuming one [`LegLiquidityPlan`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct LegLiquidityExecution {
    /// Contract instrument id carried by the public trades.
    pub instrument_id: u32,
    /// Combo or derived order that took the liquidity.
    pub taker_order_id: u64,
    /// Aggressive side taken on this leg book.
    pub taker_side: Side,
    /// Total quantity consumed, equal to the plan quantity.
    pub quantity: u64,
    /// Resting-order fills in execution order.
    pub fills: Vec<LegFill>,
}

impl LegLiquidityExecution {
    /// Returns the worst price touched by the fills.
    #[must_use]
    pub fn worst_price_ticks(&self) -> Option<u64> {
        self.fills.last().map(|fill| fill.price_ticks)
    }
}

/// Result of atomically consuming both legs of one combo execution.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ComboLegExecution {
    /// Equal quantity consumed from both leg books.
    pub quantity: u64,
    /// Combo or derived order that took the liquidity.
    pub taker_order_id: u64,
    /// First-leg resting fills.
    pub leg1: LegLiquidityExecution,
    /// Second-leg resting fills.
    pub leg2: LegLiquidityExecution,
}

/// Why a leg-liquidity plan could not be produced or consumed.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LegLiquidityError {
    /// A plan or execution requested zero quantity.
    ZeroQuantity,
    /// The book cannot cover the requested quantity through the price limit.
    InsufficientLiquidity {
        /// Quantity requested by the caller.
        requested: u64,
        /// Crossing quantity available in the book.
        available: u64,
    },
    /// The plan's fills do not match the liquidity currently in the book.
    StalePlan,
    /// The plan's fill quantities do not sum to its quantity.
    QuantityMismatch {
        /// Quantity declared by the plan.
        planned: u64,
        /// Quantity selected by the plan's fills.
        selected: u64,
    },
}

impl Display for LegLiquidityError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::ZeroQuantity => formatter.write_str("leg-liquidity quantity must be positive"),
            Self::InsufficientLiquidity {
                requested,
                available,
            } => write!(
                formatter,
                "leg book has {available} crossing quantity; {requested} requested"
            ),
            Self::StalePlan => formatter.write_str("leg-liquidity plan is stale"),
            Self::QuantityMismatch { planned, selected } => write!(
                formatter,
                "leg-liquidity plan names quantity {planned} but selects {selected}"
            ),
        }
    }
}

impl Error for LegLiquidityError {}

/// Why a two-leg combo execution was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ComboLegError {
    /// A leg names an instrument id that is not registered.
    UnknownInstrument {
        /// Which leg named the missing instrument.
        leg: ComboLeg,
        /// Instrument id that is not registered.
        instrument_id: u32,
    },
    /// Both legs name the same ordinary Contract.
    SameInstrument {
        /// Instrument id used for both legs.
        instrument_id: u32,
    },
    /// The two plans do not consume the same quantity.
    QuantityMismatch {
        /// Quantity selected on the first leg.
        leg1_quantity: u64,
        /// Quantity selected on the second leg.
        leg2_quantity: u64,
    },
    /// The two plans do not take opposite sides.
    TakerSidesNotOpposite {
        /// Aggressive side selected on the first leg.
        leg1_side: Side,
        /// Aggressive side selected on the second leg.
        leg2_side: Side,
    },
    /// One leg plan is stale or unusable; neither book was mutated.
    Plan {
        /// Which leg rejected its plan.
        leg: ComboLeg,
        /// Underlying rejection.
        error: LegLiquidityError,
    },
}

impl Display for ComboLegError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::UnknownInstrument { leg, instrument_id } => write!(
                formatter,
                "combo {leg} leg instrument {instrument_id} is not registered"
            ),
            Self::SameInstrument { instrument_id } => write!(
                formatter,
                "combo legs name the same instrument {instrument_id}"
            ),
            Self::QuantityMismatch {
                leg1_quantity,
                leg2_quantity,
            } => write!(
                formatter,
                "combo legs select different quantities: {leg1_quantity} and {leg2_quantity}"
            ),
            Self::TakerSidesNotOpposite {
                leg1_side,
                leg2_side,
            } => write!(
                formatter,
                "combo legs must take opposite sides, got {leg1_side:?} and {leg2_side:?}"
            ),
            Self::Plan { leg, error } => write!(formatter, "combo {leg} leg rejected: {error}"),
        }
    }
}

impl Error for ComboLegError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Plan { error, .. } => Some(error),
            _ => None,
        }
    }
}
