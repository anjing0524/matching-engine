//! Private order/execution events emitted by the exchange core.
//!
//! These events are consumed by middleware, then routed by session back to the
//! originating gateway. They are separate from public market-data events.

/// Lifecycle status carried by a private event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivateStatus {
    Accepted,
    PartiallyFilled,
    Filled,
    Canceled,
    Rejected,
}

/// One private event in per-session order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivateEvent {
    pub seq: u64,
    pub order_id: u64,
    pub client_order_id: u64,
    pub account_id: u64,
    pub status: PrivateStatus,
    pub filled_delta: u64,
    pub remaining: u64,
    pub reason: Option<String>,
}
