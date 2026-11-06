//! Bounded private-event replay log for session reconnect.
//!
//! Private events are retained per user. On reconnect the gateway asks for
//! events after `last_private_seq`; middleware replays them before resuming live
//! delivery.

use crate::exchange::private_events::PrivateEvent;
use std::collections::{HashMap, VecDeque};

/// Per-user bounded replay log.
#[derive(Debug, Default)]
pub struct PrivateReplayBuffer {
    per_user: HashMap<u64, VecDeque<PrivateEvent>>,
    capacity_per_user: usize,
}

impl PrivateReplayBuffer {
    #[must_use]
    pub fn new(capacity_per_user: usize) -> Self {
        Self {
            per_user: HashMap::new(),
            capacity_per_user,
        }
    }

    pub fn push(&mut self, user_id: u64, event: PrivateEvent) {
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
    pub fn replay(&self, user_id: u64, last_private_seq: u64) -> Vec<PrivateEvent> {
        self.per_user
            .get(&user_id)
            .map(|queue| {
                queue
                    .iter()
                    .filter(|event| event.seq > last_private_seq)
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
        self.per_user.values().all(VecDeque::is_empty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::private_events::PrivateStatus;

    fn event(seq: u64) -> PrivateEvent {
        PrivateEvent {
            seq,
            order_id: seq,
            client_order_id: seq,
            account_id: 1,
            status: PrivateStatus::Accepted,
            filled_delta: 0,
            remaining: 1,
            reason: None,
        }
    }

    #[test]
    fn replays_events_after_ack() {
        let mut buffer = PrivateReplayBuffer::new(8);
        for seq in 1..=5 {
            buffer.push(1, event(seq));
        }
        let replay = buffer.replay(1, 2);
        assert_eq!(
            replay.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
    }

    #[test]
    fn evicts_oldest_event() {
        let mut buffer = PrivateReplayBuffer::new(2);
        buffer.push(1, event(1));
        buffer.push(1, event(2));
        buffer.push(1, event(3));
        assert_eq!(buffer.len(1), 2);
        assert_eq!(buffer.replay(1, 0)[0].seq, 2);
    }

    #[test]
    fn unknown_user_returns_empty() {
        let buffer = PrivateReplayBuffer::new(2);
        assert!(buffer.is_empty());
        assert!(buffer.replay(9, 0).is_empty());
    }
}
