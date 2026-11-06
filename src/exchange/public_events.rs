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
    pub side: Option<Side>,
    pub price_ticks: u64,
    pub quantity: u64,
    pub volume: u64,
}
