//! Private order/execution events emitted by the trading core.
//!
//! These events are consumed by middleware, then routed by session back to the
//! originating gateway. They are separate from public market-data events.

use crate::trading::core::combo_book::ComboPrivateFill;

/// Lifecycle status carried by a private event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PrivateStatus {
    Accepted,
    PartiallyFilled,
    Filled,
    Canceled,
    Rejected,
}

/// One private event in per-session order.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// One private effect produced by the core, the Combo Book, or the legacy combo
/// engine.
///
/// Core and combo effects are kept together so the gateway can assign one
/// per-user private sequence and retain them in one replay log. The `seq` field
/// of the carried event is overwritten with that per-user sequence when the
/// effect is dispatched.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum PrivateEffect {
    /// Private order/execution event from an ordinary Contract core.
    Core(PrivateEvent),
    /// Private fill from a Combo Pair.
    Combo(ComboPrivateFill),
}

impl PrivateEffect {
    /// Returns the sequence already carried by the effect.
    ///
    /// The gateway replaces it with the per-user sequence on dispatch; a
    /// restored replay log therefore carries the assigned sequence.
    #[must_use]
    pub fn seq(&self) -> u64 {
        match self {
            Self::Core(event) => event.seq,
            Self::Combo(fill) => fill.seq,
        }
    }

    /// Returns the account that owns the effect.
    #[must_use]
    pub fn user_id(&self) -> u64 {
        match self {
            Self::Core(event) => event.account_id,
            Self::Combo(fill) => fill.user_id.get(),
        }
    }

    /// Overwrites the carried sequence.
    pub fn set_seq(&mut self, seq: u64) {
        match self {
            Self::Core(event) => event.seq = seq,
            Self::Combo(fill) => fill.seq = seq,
        }
    }
}
