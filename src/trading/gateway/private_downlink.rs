//! Private-event downlink: sequencing, replay retention, session routing, and
//! batched frame delivery for core and combo private events.

use crate::shared::collections::U64MixMap;
use crate::trading::combo::engine::ComboPrivateFill;
use crate::trading::core::private_events::PrivateEvent;
use crate::trading::gateway::frame::GatewayFrame;
use crate::trading::gateway::private_replay::{
    ComboPrivateReplayBuffer, PrivateReplayBuffer, ReplayEvent,
};
use crate::trading::gateway::server::GatewayServerHandle;
use crate::trading::transport::session::SessionRegistry;

enum DownlinkEvent {
    Core(PrivateEvent),
    Combo(ComboPrivateFill),
}

/// Owns the private sequence space and replay buffers; routes encoded frames to
/// the session that owns each account.
pub struct PrivateDownlink {
    private_replay: PrivateReplayBuffer,
    combo_replay: ComboPrivateReplayBuffer,
    next_private_seq: U64MixMap<u64>,
    unrouted: u64,
    send_errors: u64,
}

impl PrivateDownlink {
    #[must_use]
    pub fn new(replay_capacity: usize) -> Self {
        Self {
            private_replay: PrivateReplayBuffer::new(replay_capacity),
            combo_replay: ComboPrivateReplayBuffer::new(replay_capacity),
            next_private_seq: U64MixMap::default(),
            unrouted: 0,
            send_errors: 0,
        }
    }

    /// Returns replay events newer than `last_private_seq`, ordered by seq.
    #[must_use]
    pub fn replay_for(&self, user_id: u64, last_private_seq: u64) -> Vec<ReplayEvent> {
        let mut replay: Vec<ReplayEvent> = self
            .private_replay
            .replay(user_id, last_private_seq)
            .into_iter()
            .map(ReplayEvent::Core)
            .collect();
        replay.extend(
            self.combo_replay
                .replay(user_id, last_private_seq)
                .into_iter()
                .map(ReplayEvent::Combo),
        );
        replay.sort_by_key(ReplayEvent::seq);
        replay
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
            events.into_iter().map(DownlinkEvent::Core),
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
            fills.into_iter().map(DownlinkEvent::Combo),
        )
    }

    fn dispatch_events(
        &mut self,
        sessions: &SessionRegistry,
        server: &GatewayServerHandle,
        events: impl IntoIterator<Item = DownlinkEvent>,
    ) -> usize {
        let mut batches: U64MixMap<(Vec<u8>, usize)> = U64MixMap::default();
        for event in events {
            let (user_id, frame) = match event {
                DownlinkEvent::Core(mut event) => {
                    event.seq = self.assign_private_seq(event.account_id);
                    self.private_replay.push(event.account_id, event.clone());
                    (event.account_id, GatewayFrame::PrivateEvent(event))
                }
                DownlinkEvent::Combo(mut fill) => {
                    fill.seq = self.assign_private_seq(fill.user_id);
                    self.combo_replay.push(fill.user_id, fill);
                    (fill.user_id, GatewayFrame::ComboPrivate(fill))
                }
            };
            let Ok(user_id) = u32::try_from(user_id) else {
                self.unrouted += 1;
                continue;
            };
            let Some(session) = sessions.find_active_by_user(user_id) else {
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
        let next = self.next_private_seq.entry(user_id).or_insert(1);
        let seq = *next;
        *next += 1;
        seq
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
