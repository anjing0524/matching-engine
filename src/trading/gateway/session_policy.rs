//! Gateway session policy: authentication, rate-limit configuration, and
//! redirect selection, separated from frame routing.

use crate::trading::gateway::auth::GatewayCredentials;
use std::time::{Duration, Instant};

/// Policy decisions that do not require a socket or connection state.
#[derive(Default)]
pub struct SessionPolicy {
    auth_token: Option<u64>,
    credentials: Option<GatewayCredentials>,
    rate_limit: Option<(f64, f64)>,
    redirect_target: Option<String>,
}

impl SessionPolicy {
    #[must_use]
    pub fn with_auth_token(mut self, token: u64) -> Self {
        self.auth_token = Some(token);
        self
    }

    #[must_use]
    pub fn with_credentials(mut self, credentials: GatewayCredentials) -> Self {
        self.credentials = Some(credentials);
        self
    }

    #[must_use]
    pub fn with_rate_limit(mut self, capacity: f64, refill_per_sec: f64) -> Self {
        self.rate_limit = Some((capacity, refill_per_sec));
        self
    }

    #[must_use]
    pub fn with_redirect_target(mut self, target: String) -> Self {
        self.redirect_target = Some(target);
        self
    }

    #[must_use]
    pub fn login_allowed(&self, member_id: u32, seat_id: u32, auth_token: u64) -> bool {
        if let Some(credentials) = &self.credentials {
            return credentials.verify_login(member_id, auth_token, seat_id);
        }
        self.auth_token
            .is_none_or(|expected| expected == auth_token)
    }

    #[must_use]
    pub fn rate_limit(&self) -> Option<(f64, f64)> {
        self.rate_limit
    }

    #[must_use]
    pub fn idle_expired(&self, last_seen: Instant, now: Instant, timeout: Duration) -> bool {
        now.duration_since(last_seen) >= timeout
    }

    #[must_use]
    pub fn redirect_target(&self) -> Option<&str> {
        self.redirect_target.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_token_policy_rejects_wrong_token() {
        let policy = SessionPolicy::default().with_auth_token(7);
        assert!(policy.login_allowed(1, 2, 7));
        assert!(!policy.login_allowed(1, 2, 8));
    }

    #[test]
    fn idle_expiry_is_a_policy_decision() {
        let policy = SessionPolicy::default();
        let now = Instant::now();
        assert!(policy.idle_expired(now - Duration::from_secs(2), now, Duration::from_secs(1)));
        assert!(!policy.idle_expired(now, now, Duration::from_secs(1)));
    }

    #[test]
    fn rate_limit_and_redirect_are_policy_values() {
        let policy = SessionPolicy::default()
            .with_rate_limit(10.0, 5.0)
            .with_redirect_target("127.0.0.1:9000".to_string());
        assert_eq!(policy.rate_limit(), Some((10.0, 5.0)));
        assert_eq!(policy.redirect_target(), Some("127.0.0.1:9000"));
    }
}
