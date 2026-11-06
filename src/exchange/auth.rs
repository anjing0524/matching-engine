//! Per-member gateway credentials.

use std::collections::{HashMap, HashSet};

/// Member-id → shared-secret registry.
#[derive(Clone, Debug, Default)]
pub struct GatewayCredentials {
    tokens: HashMap<u32, u64>,
    seats: HashMap<u32, HashSet<u32>>,
}

impl GatewayCredentials {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, member_id: u32, token: u64) {
        self.tokens.insert(member_id, token);
    }

    /// Restricts a member to specific seats. An empty list allows any seat.
    pub fn allow_seats(&mut self, member_id: u32, seats: impl IntoIterator<Item = u32>) {
        self.seats.insert(member_id, seats.into_iter().collect());
    }

    /// Verifies member, token, and seat in one call.
    #[must_use]
    pub fn verify_login(&self, member_id: u32, token: u64, seat_id: u32) -> bool {
        if !self.verify(member_id, token) {
            return false;
        }
        self.seats
            .get(&member_id)
            .is_none_or(|allowed| allowed.is_empty() || allowed.contains(&seat_id))
    }

    /// Verifies a member login. Unknown members are rejected.
    #[must_use]
    pub fn verify(&self, member_id: u32, token: u64) -> bool {
        self.tokens
            .get(&member_id)
            .is_some_and(|expected| *expected == token)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_known_member_tokens() {
        let mut credentials = GatewayCredentials::new();
        credentials.insert(1, 11);
        credentials.insert(2, 22);
        assert!(credentials.verify(1, 11));
        assert!(credentials.verify(2, 22));
        assert!(!credentials.verify(1, 22));
        assert!(!credentials.verify(3, 11));
    }

    #[test]
    fn enforces_configured_seats() {
        let mut credentials = GatewayCredentials::new();
        credentials.insert(1, 11);
        credentials.allow_seats(1, [7, 8]);
        assert!(credentials.verify_login(1, 11, 7));
        assert!(credentials.verify_login(1, 11, 8));
        assert!(!credentials.verify_login(1, 11, 9));

        let mut unrestricted = GatewayCredentials::new();
        unrestricted.insert(2, 22);
        assert!(unrestricted.verify_login(2, 22, 99));
    }
}
