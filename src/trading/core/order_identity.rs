//! Shared order identity.
//!
//! Order identity is the account/session/client triple attached to every order
//! lifecycle. Matching cores and combo books may store it differently, but the
//! fields have one canonical definition here.

use crate::trading::core::domain_ids::{AccountId, ClientOrderId, SessionId};

/// Account, session, and client identity of one order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct OrderIdentity {
    /// Owning user account.
    pub user_id: AccountId,
    /// Gateway session that submitted the order.
    pub session_id: SessionId,
    /// Client-assigned order id preserved for private fill reporting.
    pub client_order_id: ClientOrderId,
}

impl OrderIdentity {
    /// Creates identity metadata from wire-level ids.
    #[must_use]
    pub const fn new(user_id: u64, session_id: u64, client_order_id: u64) -> Self {
        Self {
            user_id: AccountId::new(user_id),
            session_id: SessionId::new(session_id),
            client_order_id: ClientOrderId::new(client_order_id),
        }
    }
}
