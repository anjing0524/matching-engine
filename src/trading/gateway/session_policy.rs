//! Gateway session policy: authentication, rate-limit configuration, and
//! redirect selection, separated from frame routing.

use crate::trading::gateway::auth::GatewayCredentials;
use std::time::{Duration, Instant};

/// Token-bucket rate limit used by gateway command admission.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateLimit {
    /// Burst capacity in tokens.
    pub capacity: f64,
    /// Tokens replenished per second.
    pub refill_per_sec: f64,
}

/// Authentication mode configured for gateway logins.
///
/// The default is [`AuthMode::Deny`]: a runtime that was never given
/// credentials refuses every login instead of accepting anonymous traffic.
#[derive(Default)]
enum AuthMode {
    /// No credentials configured: every login is refused.
    #[default]
    Deny,
    /// Explicit development opt-in: every login is accepted.
    Anonymous,
    /// One shared token accepted for every member.
    SharedToken(u64),
    /// Per-member credentials.
    Credentials(GatewayCredentials),
}

/// Policy decisions that do not require a socket or connection state.
#[derive(Default)]
pub struct SessionPolicy {
    auth: AuthMode,
    rate_limit: Option<RateLimit>,
    redirect_target: Option<String>,
}

impl SessionPolicy {
    #[must_use]
    pub fn with_auth_token(mut self, token: u64) -> Self {
        self.auth = AuthMode::SharedToken(token);
        self
    }

    #[must_use]
    pub fn with_credentials(mut self, credentials: GatewayCredentials) -> Self {
        self.auth = AuthMode::Credentials(credentials);
        self
    }

    /// Accepts every login without credentials.
    ///
    /// This is a development and test escape hatch; production gateways must
    /// configure a token or per-member credentials instead.
    #[must_use]
    pub fn with_anonymous_access(mut self) -> Self {
        self.auth = AuthMode::Anonymous;
        self
    }

    #[must_use]
    pub fn with_rate_limit(mut self, capacity: f64, refill_per_sec: f64) -> Self {
        self.rate_limit = Some(RateLimit {
            capacity,
            refill_per_sec,
        });
        self
    }

    #[must_use]
    pub fn with_redirect_target(mut self, target: String) -> Self {
        self.redirect_target = Some(target);
        self
    }

    #[must_use]
    pub fn login_allowed(&self, member_id: u32, seat_id: u32, auth_token: u64) -> bool {
        match &self.auth {
            AuthMode::Deny => false,
            AuthMode::Anonymous => true,
            AuthMode::SharedToken(expected) => *expected == auth_token,
            AuthMode::Credentials(credentials) => {
                credentials.verify_login(member_id, auth_token, seat_id)
            }
        }
    }

    #[must_use]
    pub fn rate_limit(&self) -> Option<RateLimit> {
        self.rate_limit
    }

    #[must_use]
    pub fn idle_expired(last_seen: Instant, now: Instant, timeout: Duration) -> bool {
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
    fn default_policy_refuses_every_login() {
        let policy = SessionPolicy::default();
        assert!(!policy.login_allowed(1, 2, 0));
        assert!(!policy.login_allowed(1, 2, 7));
    }

    #[test]
    fn anonymous_access_is_an_explicit_opt_in() {
        let policy = SessionPolicy::default().with_anonymous_access();
        assert!(policy.login_allowed(1, 2, 0));
    }

    #[test]
    fn shared_token_policy_rejects_wrong_token() {
        let policy = SessionPolicy::default().with_auth_token(7);
        assert!(policy.login_allowed(1, 2, 7));
        assert!(!policy.login_allowed(1, 2, 8));
    }

    #[test]
    fn idle_expiry_is_a_policy_decision() {
        let now = Instant::now();
        assert!(SessionPolicy::idle_expired(
            now - Duration::from_secs(2),
            now,
            Duration::from_secs(1)
        ));
        assert!(!SessionPolicy::idle_expired(
            now,
            now,
            Duration::from_secs(1)
        ));
    }

    #[test]
    fn rate_limit_and_redirect_are_policy_values() {
        let policy = SessionPolicy::default()
            .with_rate_limit(10.0, 5.0)
            .with_redirect_target("127.0.0.1:9000".to_string());
        assert_eq!(
            policy.rate_limit(),
            Some(RateLimit {
                capacity: 10.0,
                refill_per_sec: 5.0
            })
        );
        assert_eq!(policy.redirect_target(), Some("127.0.0.1:9000"));
    }
}
