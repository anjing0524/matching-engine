//! Private-event downlink: sequencing, replay retention, session routing, and
//! batched frame delivery for core and combo private effects.
//!
//! The downlink owns the one per-user private sequence. Core events and Combo
//! Private Fills are stamped from the same counter in the order the runtime
//! produced them, retained in one per-user replay log, and delivered to the
//! session that owns each account.

use crate::shared::collections::U64MixMap;
use crate::trading::core::combo_book::ComboPrivateFill;
use crate::trading::core::domain_ids::EventSequence;
use crate::trading::core::private_events::{PrivateEffect, PrivateEvent};
use crate::trading::gateway::frame::GatewayFrame;
use crate::trading::gateway::private_replay::{PrivateDownlinkSnapshot, ReplayBuffer};
use crate::trading::gateway::server::GatewayServerHandle;
use crate::trading::transport::session::SessionRegistry;
use std::collections::HashSet;

/// Owns the private sequence space and replay log; routes encoded frames to the
/// session that owns each account.
pub struct PrivateDownlink {
    replay: ReplayBuffer,
    next_private_seq: U64MixMap<EventSequence>,
    unrouted: u64,
    send_errors: u64,
}

impl PrivateDownlink {
    #[must_use]
    pub fn new(replay_capacity: usize) -> Self {
        Self {
            replay: ReplayBuffer::new(replay_capacity),
            next_private_seq: U64MixMap::default(),
            unrouted: 0,
            send_errors: 0,
        }
    }

    /// Returns replay effects newer than `last_private_seq`, ordered by seq.
    #[must_use]
    pub fn replay_for(&self, user_id: u64, last_private_seq: u64) -> Vec<PrivateEffect> {
        self.replay.replay(user_id, last_private_seq)
    }

    /// Routes core private events to the session owning each account.
    pub fn dispatch_private(
        &mut self,
        sessions: &SessionRegistry,
        server: &GatewayServerHandle,
        events: impl IntoIterator<Item = PrivateEvent>,
    ) -> usize {
        self.dispatch_events(
            sessions,
            server,
            events.into_iter().map(PrivateEffect::Core),
        )
    }

    /// Routes combo private fills to the session owning each user.
    pub fn dispatch_combo_private(
        &mut self,
        sessions: &SessionRegistry,
        server: &GatewayServerHandle,
        fills: impl IntoIterator<Item = ComboPrivateFill>,
    ) -> usize {
        self.dispatch_events(
            sessions,
            server,
            fills.into_iter().map(PrivateEffect::Combo),
        )
    }

    /// Routes an ordered core/combo effect stream through one sequence space.
    pub fn dispatch_events(
        &mut self,
        sessions: &SessionRegistry,
        server: &GatewayServerHandle,
        events: impl IntoIterator<Item = PrivateEffect>,
    ) -> usize {
        let mut batches: U64MixMap<(Vec<u8>, usize)> = U64MixMap::default();
        for mut effect in events {
            let user_id = effect.user_id();
            effect.set_seq(self.assign_private_seq(user_id));
            let frame = match &effect {
                PrivateEffect::Core(event) => GatewayFrame::PrivateEvent(event.clone()),
                PrivateEffect::Combo(fill) => GatewayFrame::ComboPrivate(*fill),
            };
            self.replay.push(user_id, effect);
            let Ok(session_user) = u32::try_from(user_id) else {
                self.unrouted += 1;
                continue;
            };
            let Some(session) = sessions.find_active_by_user(session_user) else {
                self.unrouted += 1;
                continue;
            };
            let batch = batches.entry(session.session_id).or_default();
            batch.0.extend_from_slice(&frame.encode());
            batch.1 += 1;
        }
        let mut sent = 0;
        for (session_id, (bytes, frames)) in batches {
            match server.send_encoded(session_id, &bytes, frames) {
                Ok(()) => sent += frames,
                Err(_) => self.send_errors += frames as u64,
            }
        }
        sent
    }

    fn assign_private_seq(&mut self, user_id: u64) -> u64 {
        let next = self
            .next_private_seq
            .entry(user_id)
            .or_insert(EventSequence::new(1));
        let seq = next.get();
        *next = next.next();
        seq
    }

    /// Captures the per-user sequence and replay log.
    #[must_use]
    pub fn snapshot(&self) -> PrivateDownlinkSnapshot {
        let mut next_private_seq: Vec<(u64, u64)> = self
            .next_private_seq
            .iter()
            .map(|(user_id, next)| (*user_id, next.get()))
            .collect();
        next_private_seq.sort_unstable_by_key(|(user_id, _)| *user_id);
        PrivateDownlinkSnapshot {
            next_private_seq,
            replay: self.replay.snapshot(),
        }
    }

    /// Validates a snapshot without mutating the downlink.
    ///
    /// # Errors
    /// Returns a reason when the snapshot repeats a user, carries a replay
    /// effect that does not belong to its user, has a non-advancing sequence,
    /// or retains more events than this downlink's replay capacity.
    pub fn validate_snapshot(&self, snapshot: &PrivateDownlinkSnapshot) -> Result<(), String> {
        let mut next_by_user: U64MixMap<u64> = U64MixMap::default();
        for &(user_id, next) in &snapshot.next_private_seq {
            if next == 0 {
                return Err(format!("user {user_id} has a zero next private sequence"));
            }
            if next_by_user.insert(user_id, next).is_some() {
                return Err(format!(
                    "snapshot repeats private sequence state for user {user_id}"
                ));
            }
        }
        let mut seen_users = HashSet::new();
        for (user_id, events) in &snapshot.replay {
            if !seen_users.insert(*user_id) {
                return Err(format!("snapshot repeats replay state for user {user_id}"));
            }
            let Some(&next) = next_by_user.get(user_id) else {
                return Err(format!(
                    "snapshot has replay for user {user_id} without a sequence"
                ));
            };
            if events.len() > self.replay.capacity() {
                return Err(format!(
                    "user {user_id} has {} replay events but capacity is {}",
                    events.len(),
                    self.replay.capacity()
                ));
            }
            let mut previous = 0;
            for effect in events {
                let seq = effect.seq();
                if seq == 0 || seq <= previous {
                    return Err(format!(
                        "user {user_id} replay sequence {seq} is not strictly increasing"
                    ));
                }
                if effect.user_id() != *user_id {
                    return Err(format!(
                        "user {user_id} replay carries an effect for user {}",
                        effect.user_id()
                    ));
                }
                previous = seq;
            }
            if previous >= next {
                return Err(format!(
                    "user {user_id} replay sequence {previous} does not advance past next {next}"
                ));
            }
        }
        Ok(())
    }

    /// Replaces the per-user sequence and replay log with a validated snapshot.
    pub fn restore_snapshot(&mut self, snapshot: PrivateDownlinkSnapshot) {
        self.next_private_seq = snapshot
            .next_private_seq
            .into_iter()
            .map(|(user_id, next)| (user_id, EventSequence::new(next)))
            .collect();
        self.replay.restore(snapshot.replay);
    }

    #[must_use]
    pub fn unrouted(&self) -> u64 {
        self.unrouted
    }

    #[must_use]
    pub fn send_errors(&self) -> u64 {
        self.send_errors
    }
}
