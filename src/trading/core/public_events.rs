//! Public market-data/control events emitted by the trading core.
//!
//! This is the first slice: auction result and session phase events. Continuous
//! order/trade events will be added as the matching kernel expands.

use crate::trading::core::protocol::Side;

/// Public event kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicEventType {
    TimeTick,
    OrderRested,
    OrderCanceled,
    Trade,
    /// One atomic Combo Fill's price statistics on a Combo Pair.
    ///
    /// The event carries the pair id and realized spread instead of a Contract
    /// instrument. Implied orders are derived matching candidates and never
    /// receive this or any other public order event.
    ComboTrade,
    AuctionResult,
    SessionSwitch,
}

/// Trading phase carried by session-switch events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TradingPhase {
    Closed,
    PreOpen,
    OpeningAuction,
    Continuous,
    Break,
}

impl TradingPhase {
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Closed),
            1 => Some(Self::PreOpen),
            2 => Some(Self::OpeningAuction),
            3 => Some(Self::Continuous),
            4 => Some(Self::Break),
            _ => None,
        }
    }

    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Closed => 0,
            Self::PreOpen => 1,
            Self::OpeningAuction => 2,
            Self::Continuous => 3,
            Self::Break => 4,
        }
    }
}

/// One public event in market-data sequence order.
///
/// Contract-scoped events use [`Self::instrument_id`]; a
/// [`PublicEventType::ComboTrade`] uses [`Self::pair_id`] and carries the
/// realized spread plus both leg reference prices. Fields that do not apply to
/// an event kind are zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicEvent {
    pub seq: u64,
    pub event_type: PublicEventType,
    pub trading_ts_ns: u64,
    pub phase: u8,
    pub instrument_id: u32,
    /// Combo Pair for [`PublicEventType::ComboTrade`]; zero otherwise.
    pub pair_id: u32,
    pub order_id: u64,
    pub trade_id: u64,
    pub buy_order_id: u64,
    pub sell_order_id: u64,
    pub side: Option<Side>,
    pub price_ticks: u64,
    pub quantity: u64,
    pub volume: u64,
    /// Realized spread of a [`PublicEventType::ComboTrade`]; zero otherwise.
    pub spread_price: i64,
    /// First-leg reference price of a Combo trade; zero otherwise.
    pub leg1_price_ticks: u64,
    /// Second-leg reference price of a Combo trade; zero otherwise.
    pub leg2_price_ticks: u64,
}
