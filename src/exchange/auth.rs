//! Per-member gateway credentials.

use std::collections::HashMap;

/// Member-id → shared-secret registry.
#[derive(Clone, Debug, Default)]
pub struct GatewayCredentials {
    tokens: HashMap<u32, u64>,
}

impl GatewayCredentials {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, member_id: u32, token: u64) {
        self.tokens.insert(member_id, token);
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
}
