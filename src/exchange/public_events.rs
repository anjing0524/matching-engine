//! Public market-data/control events emitted by the exchange core.
//!
//! This is the first slice: auction result and session phase events. Continuous
//! order/trade events will be added as the matching kernel expands.

use crate::exchange::protocol::Side;

/// Public event kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicEventType {
    OrderRested,
    OrderCanceled,
    Trade,
    AuctionResult,
    SessionSwitch,
}

/// Trading phase carried by session-switch events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExchangePhase {
    Closed,
    PreOpen,
    OpeningAuction,
    Continuous,
    Break,
}

impl ExchangePhase {
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicEvent {
    pub seq: u64,
    pub event_type: PublicEventType,
    pub exchange_ts_ns: u64,
    pub phase: u8,
    pub instrument_id: u32,
    pub order_id: u64,
    pub trade_id: u64,
    pub buy_order_id: u64,
    pub sell_order_id: u64,
    pub side: Option<Side>,
    pub price_ticks: u64,
    pub quantity: u64,
    pub volume: u64,
}
