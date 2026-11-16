//! Buffers and sequences for combo private fills and combo public trades.
//!
//! The journal owns the per-venue combo private sequence, the undrained combo
//! private buffer, and the undrained combo public-trade buffer. Matching and
//! margin code produce effects; they do not own storage, draining, replay, or
//! snapshot fields.

use crate::trading::core::combo_book::ComboPrivateFill;
use crate::trading::core::domain_ids::EventSequence;
use crate::trading::core::public_events::PublicEvent;

/// Undrained combo effects plus their private sequence.
#[derive(Debug, Default)]
pub struct ComboEventJournal {
    private: Vec<ComboPrivateFill>,
    public: Vec<PublicEvent>,
    next_private_seq: EventSequence,
}

impl ComboEventJournal {
    /// Creates an empty journal whose first private fill receives sequence 1.
    #[must_use]
    pub fn new() -> Self {
        Self {
            private: Vec::new(),
            public: Vec::new(),
            next_private_seq: EventSequence::new(1),
        }
    }

    /// Buffers one private fill and assigns its sequence.
    pub fn push_private(&mut self, mut fill: ComboPrivateFill) {
        fill.seq = self.next_private_seq.get();
        self.next_private_seq = self.next_private_seq.next();
        self.private.push(fill);
    }

    /// Buffers one combo public trade.
    pub fn push_public(&mut self, event: PublicEvent) {
        self.public.push(event);
    }

    /// Returns undrained private fills.
    #[must_use]
    pub fn private(&self) -> &[ComboPrivateFill] {
        &self.private
    }

    /// Drains undrained private fills.
    pub fn drain_private(&mut self) -> Vec<ComboPrivateFill> {
        std::mem::take(&mut self.private)
    }

    /// Drains undrained public combo trades.
    pub fn drain_public(&mut self) -> Vec<PublicEvent> {
        std::mem::take(&mut self.public)
    }

    /// Returns the sequence the next private fill will receive.
    #[must_use]
    pub fn next_private_seq(&self) -> u64 {
        self.next_private_seq.get()
    }

    /// Typed form of [`Self::next_private_seq`] for the new domain seam.
    #[must_use]
    pub fn next_private_sequence(&self) -> EventSequence {
        self.next_private_seq
    }

    /// Returns private fills newer than `last_private_seq`, oldest first.
    #[must_use]
    pub fn replay_after(&self, last_private_seq: u64) -> Vec<ComboPrivateFill> {
        self.private
            .iter()
            .copied()
            .filter(|fill| fill.seq > last_private_seq)
            .collect()
    }

    /// Captures the private buffer and next sequence for a snapshot.
    #[must_use]
    pub fn private_snapshot(&self) -> (Vec<ComboPrivateFill>, u64) {
        (self.private.clone(), self.next_private_seq.get())
    }

    /// Restores a snapshot's private buffer and sequence.
    pub fn restore_private(&mut self, fills: Vec<ComboPrivateFill>, next_private_seq: u64) {
        self.private = fills;
        self.next_private_seq = EventSequence::new(next_private_seq);
        self.public.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(pair_id: u32) -> ComboPrivateFill {
        ComboPrivateFill {
            seq: 0,
            pair_id,
            order_id: crate::trading::core::domain_ids::OrderId::new(1),
            client_order_id: crate::trading::core::domain_ids::ClientOrderId::new(1),
            user_id: crate::trading::core::domain_ids::AccountId::new(1),
            session_id: crate::trading::core::domain_ids::SessionId::new(1),
            side: crate::trading::core::protocol::Side::Buy,
            quantity: 1,
            spread_price: 10,
            leg1_price_ticks: 100,
            leg2_price_ticks: 90,
        }
    }

    #[test]
    fn private_sequence_is_assigned_and_drained_in_order() {
        let mut journal = ComboEventJournal::new();
        journal.push_private(fill(1));
        journal.push_private(fill(2));
        assert_eq!(journal.private()[0].seq, 1);
        assert_eq!(journal.private()[1].seq, 2);
        assert_eq!(journal.next_private_seq(), 3);
        let drained = journal.drain_private();
        assert_eq!(drained.len(), 2);
        assert!(journal.private().is_empty());
    }

    #[test]
    fn replay_returns_fills_newer_than_last_sequence() {
        let mut journal = ComboEventJournal::new();
        journal.push_private(fill(1));
        journal.push_private(fill(2));
        let replayed = journal.replay_after(1);
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].seq, 2);
    }

    #[test]
    fn snapshot_restore_preserves_sequence_and_clears_public() {
        let mut journal = ComboEventJournal::new();
        journal.push_private(fill(1));
        let (fills, next) = journal.private_snapshot();
        let mut restored = ComboEventJournal::new();
        restored.restore_private(fills, next);
        assert_eq!(restored.next_private_seq(), 2);
        assert_eq!(restored.private().len(), 1);
        assert!(restored.drain_public().is_empty());
    }
}
