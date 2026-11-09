//! Gateway/member session registry.
//!
//! Middleware owns the mapping from gateway connections to member/user/seat
//! identity and the last private sequence used for reconnect replay. This slice
//! is an in-memory registry; network connections and authentication are later.

use std::collections::HashMap;

/// Rejection for a zero or duplicate session id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionOpenError;

/// One authenticated exchange session.
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
    sessions: HashMap<u64, SessionInfo>,
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
        self.sessions.insert(info.session_id, info);
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
            .sessions
            .values()
            .find(|session| {
                session.active
                    && session.member_id == info.member_id
                    && session.user_id == info.user_id
                    && session.seat_id == info.seat_id
                    && session.session_id != info.session_id
            })
            .map(|session| session.session_id);
        if let Some(old) = replaced
            && let Some(session) = self.sessions.get_mut(&old)
        {
            session.active = false;
        }
        self.sessions.insert(info.session_id, info);
        Ok(replaced)
    }

    /// Marks every active session on `gateway_id` inactive.
    ///
    /// Returns the drained session ids; records are retained for audit/reconnect.
    pub fn drain_gateway(&mut self, gateway_id: u32) -> Vec<u64> {
        let drained: Vec<u64> = self
            .sessions
            .values()
            .filter(|session| session.active && session.gateway_id == gateway_id)
            .map(|session| session.session_id)
            .collect();
        for session_id in &drained {
            if let Some(session) = self.sessions.get_mut(session_id) {
                session.active = false;
            }
        }
        drained
    }

    /// Marks a session closed; the record is retained for audit/reconnect.
    pub fn close(&mut self, session_id: u64) -> bool {
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return false;
        };
        session.active = false;
        true
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
        self.sessions
            .values()
            .find(|session| session.active && session.user_id == user_id)
            .copied()
    }

    /// Removes oldest closed session records, keeping at most `keep_closed`.
    ///
    /// Returns the number of removed records. Active sessions are never pruned.
    pub fn prune_closed(&mut self, keep_closed: usize) -> usize {
        let mut closed: Vec<u64> = self
            .sessions
            .values()
            .filter(|session| !session.active)
            .map(|session| session.session_id)
            .collect();
        if closed.len() <= keep_closed {
            return 0;
        }
        closed.sort_unstable();
        let remove = closed.len() - keep_closed;
        for session_id in closed.into_iter().take(remove) {
            self.sessions.remove(&session_id);
        }
        remove
    }

    #[must_use]
    pub fn active_count(&self) -> usize {
        self.sessions
            .values()
            .filter(|session| session.active)
            .count()
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
}
