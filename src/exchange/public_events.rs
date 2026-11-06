//! Public market-data/control events emitted by the exchange core.
//!
//! This is the first slice: auction result and session phase events. Continuous
//! order/trade events will be added as the matching kernel expands.

/// Public event kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicEventType {
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
    pub price_ticks: u64,
    pub volume: u64,
}
