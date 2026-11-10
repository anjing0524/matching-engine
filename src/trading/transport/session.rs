//! Gateway/member session registry.
//!
//! Middleware owns the mapping from gateway connections to member/user/seat
//! identity and the last private sequence used for reconnect replay. This slice
//! is an in-memory registry; network connections and authentication are later.

use crate::shared::collections::{U64MixMap, U128MixMap};
use std::collections::VecDeque;

/// Rejection for a zero or duplicate session id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionOpenError;

fn identity_key(info: &SessionInfo) -> u128 {
    (u128::from(info.member_id) << 64) | (u128::from(info.user_id) << 32) | u128::from(info.seat_id)
}

/// One authenticated trading session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub session_id: u64,
    pub gateway_id: u32,
    pub member_id: u32,
    pub user_id: u32,
    pub seat_id: u32,
    pub last_private_seq: u64,
    pub active: bool,
}

/// Registry keyed by session id.
#[derive(Debug, Default)]
pub struct SessionRegistry {
    sessions: U64MixMap<SessionInfo>,
    active_count: usize,
    closed_count: usize,
    /// user_id -> active session id
    by_user: U64MixMap<u64>,
    /// gateway_id -> active session ids
    by_gateway: U64MixMap<Vec<u64>>,
    /// (member, user, seat) -> active session id
    by_identity: U128MixMap<u128, u64>,
    /// Closed session ids in close order, for bounded pruning.
    closed_order: VecDeque<u64>,
}

impl SessionRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens a session. Returns `false` for a duplicate or zero session id.
    pub fn open(&mut self, info: SessionInfo) -> bool {
        if info.session_id == 0 || self.sessions.contains_key(&info.session_id) {
            return false;
        }
        if info.active {
            self.activate(info);
        } else {
            self.sessions.insert(info.session_id, info);
            self.closed_count += 1;
            self.closed_order.push_back(info.session_id);
        }
        true
    }

    fn activate(&mut self, info: SessionInfo) {
        self.sessions.insert(info.session_id, info);
        self.active_count += 1;
        self.by_user
            .insert(u64::from(info.user_id), info.session_id);
        self.by_gateway
            .entry(u64::from(info.gateway_id))
            .or_default()
            .push(info.session_id);
        self.by_identity
            .insert(identity_key(&info), info.session_id);
    }

    fn deactivate(&mut self, session_id: u64) -> bool {
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return false;
        };
        if !session.active {
            return false;
        }
        session.active = false;
        let user_id = u64::from(session.user_id);
        let gateway_id = u64::from(session.gateway_id);
        let identity = identity_key(session);
        self.active_count -= 1;
        self.closed_count += 1;
        if self.by_user.get(&user_id) == Some(&session_id) {
            self.by_user.remove(&user_id);
        }
        if let Some(ids) = self.by_gateway.get_mut(&gateway_id) {
            ids.retain(|id| *id != session_id);
            if ids.is_empty() {
                self.by_gateway.remove(&gateway_id);
            }
        }
        if self.by_identity.get(&identity) == Some(&session_id) {
            self.by_identity.remove(&identity);
        }
        self.closed_order.push_back(session_id);
        true
    }

    /// Opens a new session for the same member/user/seat and closes the old one.
    ///
    /// Returns the replaced session id, or `None` when no active session matched.
    ///
    /// # Errors
    /// Returns `false` for a zero or duplicate session id.
    pub fn open_or_takeover(&mut self, info: SessionInfo) -> Result<Option<u64>, SessionOpenError> {
        if info.session_id == 0 || self.sessions.contains_key(&info.session_id) {
            return Err(SessionOpenError);
        }
        let replaced = self
            .by_identity
            .get(&identity_key(&info))
            .copied()
            .filter(|session_id| *session_id != info.session_id);
        if let Some(old) = replaced {
            self.deactivate(old);
        }
        self.activate(info);
        Ok(replaced)
    }

    /// Marks every active session on `gateway_id` inactive.
    ///
    /// Returns the drained session ids; records are retained for audit/reconnect.
    pub fn drain_gateway(&mut self, gateway_id: u32) -> Vec<u64> {
        let drained = self
            .by_gateway
            .remove(&u64::from(gateway_id))
            .unwrap_or_default();
        for session_id in &drained {
            self.deactivate(*session_id);
        }
        drained
    }

    /// Marks a session closed; the record is retained for audit/reconnect.
    pub fn close(&mut self, session_id: u64) -> bool {
        self.deactivate(session_id)
    }

    #[must_use]
    pub fn get(&self, session_id: u64) -> Option<SessionInfo> {
        self.sessions.get(&session_id).copied()
    }

    /// Updates the last successfully delivered private sequence.
    pub fn acknowledge_private(&mut self, session_id: u64, private_seq: u64) -> bool {
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return false;
        };
        if private_seq < session.last_private_seq {
            return false;
        }
        session.last_private_seq = private_seq;
        true
    }

    #[must_use]
    pub fn find_active_by_user(&self, user_id: u32) -> Option<SessionInfo> {
        let session_id = self.by_user.get(&u64::from(user_id))?;
        self.sessions
            .get(session_id)
            .filter(|session| session.active)
            .copied()
    }

    /// Removes oldest closed session records, keeping at most `keep_closed`.
    ///
    /// Returns the number of removed records. Active sessions are never pruned.
    pub fn prune_closed(&mut self, keep_closed: usize) -> usize {
        let mut removed = 0;
        while self.closed_order.len() > keep_closed {
            let Some(session_id) = self.closed_order.pop_front() else {
                break;
            };
            if self.sessions.remove(&session_id).is_some() {
                self.closed_count -= 1;
                removed += 1;
            }
        }
        removed
    }

    #[must_use]
    pub fn closed_count(&self) -> usize {
        self.closed_count
    }

    #[must_use]
    pub fn active_count(&self) -> usize {
        self.active_count
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: u64) -> SessionInfo {
        SessionInfo {
            session_id: id,
            gateway_id: 1,
            member_id: 10,
            user_id: 20,
            seat_id: 30,
            last_private_seq: 0,
            active: true,
        }
    }

    #[test]
    fn opens_and_closes_sessions() {
        let mut registry = SessionRegistry::new();
        assert!(registry.open(session(1)));
        assert!(!registry.open(session(1)));
        assert_eq!(registry.active_count(), 1);
        assert!(registry.close(1));
        assert!(!registry.get(1).unwrap().active);
        assert_eq!(registry.active_count(), 0);
    }

    #[test]
    fn private_ack_is_monotonic() {
        let mut registry = SessionRegistry::new();
        registry.open(session(1));
        assert!(registry.acknowledge_private(1, 5));
        assert!(!registry.acknowledge_private(1, 4));
        assert_eq!(registry.get(1).unwrap().last_private_seq, 5);
    }

    #[test]
    fn new_login_takes_over_same_member_session() {
        let mut registry = SessionRegistry::new();
        registry.open(session(1));
        let mut replacement = session(2);
        replacement.gateway_id = 2;
        assert_eq!(registry.open_or_takeover(replacement), Ok(Some(1)));
        assert!(!registry.get(1).unwrap().active);
        assert!(registry.get(2).unwrap().active);
        assert_eq!(registry.active_count(), 1);
    }

    #[test]
    fn draining_gateway_closes_only_its_sessions() {
        let mut registry = SessionRegistry::new();
        registry.open(session(1));
        let mut other = session(2);
        other.gateway_id = 2;
        registry.open(other);
        assert_eq!(registry.drain_gateway(1), vec![1]);
        assert!(!registry.get(1).unwrap().active);
        assert!(registry.get(2).unwrap().active);
        assert_eq!(registry.active_count(), 1);
    }

    #[test]
    fn closed_sessions_are_pruned() {
        let mut registry = SessionRegistry::new();
        for id in 1..=3 {
            registry.open(session(id));
            registry.close(id);
        }
        assert_eq!(registry.prune_closed(1), 2);
        assert_eq!(registry.len(), 1);
        assert!(registry.get(3).is_some());
    }

    #[test]
    fn unknown_session_is_rejected() {
        let mut registry = SessionRegistry::new();
        assert!(!registry.close(9));
        assert!(!registry.acknowledge_private(9, 1));
        assert!(registry.is_empty());
    }
    #[test]
    fn secondary_indexes_follow_takeover_and_close() {
        let mut registry = SessionRegistry::new();
        assert!(registry.open(session(1)));
        assert_eq!(
            registry.find_active_by_user(20).map(|s| s.session_id),
            Some(1)
        );
        assert_eq!(registry.closed_count(), 0);

        let replacement = session(2);
        assert_eq!(registry.open_or_takeover(replacement), Ok(Some(1)));
        assert_eq!(
            registry.find_active_by_user(20).map(|s| s.session_id),
            Some(2)
        );
        assert_eq!(registry.active_count(), 1);
        assert_eq!(registry.closed_count(), 1);

        assert!(registry.close(2));
        assert!(registry.find_active_by_user(20).is_none());
        assert_eq!(registry.active_count(), 0);
        assert_eq!(registry.closed_count(), 2);

        assert_eq!(registry.prune_closed(1), 1);
        assert_eq!(registry.closed_count(), 1);
        assert_eq!(registry.len(), 1);
    }
}
