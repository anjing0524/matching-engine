//! Static Combo Pair definitions owned by the instrument registry.
//!
//! A pair names two ordinary Contract instrument ids. M1 pairs are fixed at
//! startup, carry a 1:1 quantity ratio, and add spread and margin parameters.
//! Pair definitions are startup configuration: protocol commands never create
//! or replace one while matching runs.

use std::error::Error;
use std::fmt::{Display, Formatter, Result as FmtResult};

/// One of the two legs of a Combo Pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComboLeg {
    /// First leg: the near month for cross-term, exchange-defined product otherwise.
    First,
    /// Second leg: the far month for cross-term, exchange-defined product otherwise.
    Second,
}

impl Display for ComboLeg {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::First => formatter.write_str("first"),
            Self::Second => formatter.write_str("second"),
        }
    }
}

/// Margin parameters for the two ordinary leg Contracts of a Combo Pair.
///
/// The pair itself has no position. Each leg freezes margin from its own
/// notional using its configured rate. The registry rejects a zero multiplier
/// and rates above 10000 basis points.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ComboPairMargin {
    /// Contract multiplier applied to leg prices.
    pub multiplier: u64,
    /// First-leg margin rate in basis points of leg notional.
    pub leg1_rate_bps: u64,
    /// Second-leg margin rate in basis points of leg notional.
    pub leg2_rate_bps: u64,
}

impl ComboPairMargin {
    /// Creates margin parameters; the registry validates them at registration.
    #[must_use]
    pub const fn new(multiplier: u64, leg1_rate_bps: u64, leg2_rate_bps: u64) -> Self {
        Self {
            multiplier,
            leg1_rate_bps,
            leg2_rate_bps,
        }
    }
}

/// Exchange classification of a Combo Pair.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ComboPairKind {
    /// Same product, different months.
    #[default]
    CrossTerm,
    /// Different products, same month.
    CrossProduct,
}

impl ComboPairKind {
    /// Lower value sorts first when implied priorities tie on price.
    #[must_use]
    pub const fn priority(self) -> u8 {
        match self {
            Self::CrossTerm => 0,
            Self::CrossProduct => 1,
        }
    }
}

impl Display for ComboPairKind {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::CrossTerm => formatter.write_str("cross-term"),
            Self::CrossProduct => formatter.write_str("cross-product"),
        }
    }
}

/// One static exchange-defined two-leg spread contract.
///
/// M1 pairs have a fixed 1:1 quantity ratio. The spread price is quoted in
/// [`Self::spread_tick`] units and may be negative.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ComboPair {
    /// Identifier used by Combo Orders.
    pub pair_id: u32,
    /// First ordinary Contract leg.
    pub leg1_instrument_id: u32,
    /// Second ordinary Contract leg.
    pub leg2_instrument_id: u32,
    /// Positive spread price increment.
    pub spread_tick: u64,
    /// Leg-level margin parameters.
    pub margin: ComboPairMargin,
    /// Cross-term or cross-product classification.
    pub kind: ComboPairKind,
    /// Inclusive start of the pair's valid trading window, in ns.
    #[serde(default)]
    pub valid_from_ns: u64,
    /// Exclusive end of the pair's valid trading window, in ns.
    #[serde(default = "default_valid_until")]
    pub valid_until_ns: u64,
}

fn default_valid_until() -> u64 {
    u64::MAX
}

impl ComboPair {
    /// Creates a Combo Pair definition; the registry validates it at registration.
    #[must_use]
    pub const fn new(
        pair_id: u32,
        leg1_instrument_id: u32,
        leg2_instrument_id: u32,
        spread_tick: u64,
        margin: ComboPairMargin,
    ) -> Self {
        Self {
            pair_id,
            leg1_instrument_id,
            leg2_instrument_id,
            spread_tick,
            margin,
            kind: ComboPairKind::CrossTerm,
            valid_from_ns: 0,
            valid_until_ns: u64::MAX,
        }
    }

    /// Sets the pair classification used for same-price implied priority.
    #[must_use]
    pub const fn with_kind(mut self, kind: ComboPairKind) -> Self {
        self.kind = kind;
        self
    }

    /// Sets the pair's valid trading window.
    #[must_use]
    pub const fn with_valid_window(mut self, from_ns: u64, until_ns: u64) -> Self {
        self.valid_from_ns = from_ns;
        self.valid_until_ns = until_ns;
        self
    }

    /// Returns whether `trading_ts_ns` is inside the pair window.
    #[must_use]
    pub const fn is_valid_at(&self, trading_ts_ns: u64) -> bool {
        self.valid_from_ns <= trading_ts_ns && trading_ts_ns < self.valid_until_ns
    }
}

/// Why the registry refused a Combo Pair definition.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ComboPairError {
    /// Pair id zero is reserved as the non-combo command sentinel.
    InvalidPairId {
        /// Rejected pair id.
        pair_id: u32,
    },
    /// A pair with this id is already registered.
    DuplicatePairId {
        /// Pair id that was already present.
        pair_id: u32,
    },
    /// A leg names an instrument id that is not a registered ordinary Contract.
    UnknownLegInstrument {
        /// Pair being registered.
        pair_id: u32,
        /// Which leg names the unknown instrument.
        leg: ComboLeg,
        /// Instrument id that is not registered.
        instrument_id: u32,
    },
    /// Both legs name the same ordinary Contract.
    IdenticalLegs {
        /// Pair being registered.
        pair_id: u32,
        /// Instrument id used for both legs.
        instrument_id: u32,
    },
    /// The two leg Contracts do not share a compatible tick size.
    TickMismatch {
        /// Pair being registered.
        pair_id: u32,
        /// First leg tick.
        leg1_tick: u64,
        /// Second leg tick.
        leg2_tick: u64,
    },
    /// The spread tick cannot define a valid spread price increment.
    InvalidSpreadTick {
        /// Pair being registered.
        pair_id: u32,
        /// Rejected tick value.
        spread_tick: u64,
    },
    /// The margin parameters cannot reserve risk on the two legs.
    InvalidMargin {
        /// Pair being registered.
        pair_id: u32,
        /// Human-readable reason the margin is unusable.
        reason: &'static str,
    },
}

impl Display for ComboPairError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::InvalidPairId { pair_id } => {
                write!(formatter, "combo pair id {pair_id} is reserved")
            }
            Self::DuplicatePairId { pair_id } => {
                write!(formatter, "combo pair {pair_id} is already registered")
            }
            Self::UnknownLegInstrument {
                pair_id,
                leg,
                instrument_id,
            } => write!(
                formatter,
                "combo pair {pair_id} {leg} leg instrument {instrument_id} is not registered"
            ),
            Self::IdenticalLegs {
                pair_id,
                instrument_id,
            } => write!(
                formatter,
                "combo pair {pair_id} names instrument {instrument_id} as both legs"
            ),
            Self::TickMismatch {
                pair_id,
                leg1_tick,
                leg2_tick,
            } => write!(
                formatter,
                "combo pair {pair_id} has incompatible leg ticks {leg1_tick} and {leg2_tick}"
            ),
            Self::InvalidSpreadTick {
                pair_id,
                spread_tick,
            } => write!(
                formatter,
                "combo pair {pair_id} has invalid spread tick {spread_tick}: tick must be positive"
            ),
            Self::InvalidMargin { pair_id, reason } => {
                write!(
                    formatter,
                    "combo pair {pair_id} has invalid margin: {reason}"
                )
            }
        }
    }
}

impl Error for ComboPairError {}
