//! Bounded per-user private-event replay log for session reconnect.
//!
//! Core and Combo effects share one replay log. On reconnect the gateway asks
//! for events after `last_private_seq`; middleware replays them before resuming
//! live delivery. The log and the per-user sequence that stamps it are part of
//! the runtime snapshot, so a restart cannot reset or reorder the stream.

use crate::shared::collections::U64MixMap;
use crate::trading::core::private_events::PrivateEffect;
use std::collections::VecDeque;

/// Per-user bounded replay log shared by core and combo producers.
#[derive(Debug, Default)]
pub struct ReplayBuffer {
    per_user: U64MixMap<VecDeque<PrivateEffect>>,
    capacity_per_user: usize,
}

impl ReplayBuffer {
    #[must_use]
    pub fn new(capacity_per_user: usize) -> Self {
        Self {
            per_user: U64MixMap::default(),
            capacity_per_user,
        }
    }

    pub fn push(&mut self, user_id: u64, event: PrivateEffect) {
        if self.capacity_per_user == 0 {
            return;
        }
        let queue = self.per_user.entry(user_id).or_default();
        if queue.len() == self.capacity_per_user {
            queue.pop_front();
        }
        queue.push_back(event);
    }

    /// Returns events with `seq > last_private_seq`, oldest first.
    #[must_use]
    pub fn replay(&self, user_id: u64, last_private_seq: u64) -> Vec<PrivateEffect> {
        self.per_user
            .get(&user_id)
            .map(|queue| {
                queue
                    .iter()
                    .filter(|event| event.seq() > last_private_seq)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    #[must_use]
    pub fn len(&self, user_id: u64) -> usize {
        self.per_user.get(&user_id).map_or(0, VecDeque::len)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.per_user
            .values()
            .all(std::collections::VecDeque::is_empty)
    }

    /// Number of events retained per user.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity_per_user
    }

    /// Captures every retained event, sorted by user id.
    #[must_use]
    pub fn snapshot(&self) -> Vec<(u64, Vec<PrivateEffect>)> {
        let mut entries: Vec<(u64, Vec<PrivateEffect>)> = self
            .per_user
            .iter()
            .filter(|(_, queue)| !queue.is_empty())
            .map(|(user_id, queue)| (*user_id, queue.iter().cloned().collect()))
            .collect();
        entries.sort_unstable_by_key(|(user_id, _)| *user_id);
        entries
    }

    /// Replaces the retained events with an already validated snapshot.
    pub fn restore(&mut self, entries: Vec<(u64, Vec<PrivateEffect>)>) {
        self.per_user.clear();
        for (user_id, events) in entries {
            self.per_user.insert(user_id, events.into_iter().collect());
        }
    }
}

/// Serializable per-user private stream retained by the gateway downlink.
///
/// The snapshot carries the next sequence for each user and the bounded replay
/// log, so a restarted gateway continues the same stream and can answer a
/// reconnect from a client that had not acknowledged its last events.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrivateDownlinkSnapshot {
    /// Next private sequence per user, sorted by user id.
    pub next_private_seq: Vec<(u64, u64)>,
    /// Retained replay effects per user, oldest first, sorted by user id.
    pub replay: Vec<(u64, Vec<PrivateEffect>)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trading::core::private_events::{PrivateEvent, PrivateStatus};

    fn effect(seq: u64) -> PrivateEffect {
        PrivateEffect::Core(PrivateEvent {
            seq,
            order_id: seq,
            client_order_id: seq,
            account_id: 1,
            status: PrivateStatus::Accepted,
            filled_delta: 0,
            remaining: 1,
            reason: None,
        })
    }

    #[test]
    fn replays_events_after_ack() {
        let mut buffer = ReplayBuffer::new(8);
        for seq in 1..=5 {
            buffer.push(1, effect(seq));
        }
        let replay = buffer.replay(1, 2);
        assert_eq!(
            replay.iter().map(PrivateEffect::seq).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
    }

    #[test]
    fn evicts_oldest_event() {
        let mut buffer = ReplayBuffer::new(2);
        buffer.push(1, effect(1));
        buffer.push(1, effect(2));
        buffer.push(1, effect(3));
        assert_eq!(buffer.len(1), 2);
        assert_eq!(buffer.replay(1, 0)[0].seq(), 2);
    }

    #[test]
    fn unknown_user_returns_empty() {
        let buffer = ReplayBuffer::new(2);
        assert!(buffer.is_empty());
        assert!(buffer.replay(9, 0).is_empty());
    }

    #[test]
    fn snapshot_roundtrips_retained_events() {
        let mut buffer = ReplayBuffer::new(4);
        buffer.push(2, effect(1));
        buffer.push(1, effect(2));
        let snapshot = buffer.snapshot();
        assert_eq!(
            snapshot
                .iter()
                .map(|(user_id, _)| *user_id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );

        let mut restored = ReplayBuffer::new(4);
        restored.restore(snapshot);
        assert_eq!(restored.replay(2, 0).len(), 1);
        assert_eq!(restored.replay(1, 1).len(), 1);
    }
}
