//! Gateway/member session registry.
//!
//! Middleware owns the mapping from gateway connections to member/user/seat
//! identity and the last private sequence used for reconnect replay. This slice
//! is an in-memory registry; network connections and authentication are later.

use std::collections::HashMap;

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
    fn unknown_session_is_rejected() {
        let mut registry = SessionRegistry::new();
        assert!(!registry.close(9));
        assert!(!registry.acknowledge_private(9, 1));
        assert!(registry.is_empty());
    }
}
