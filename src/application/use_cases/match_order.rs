/// Match Order Use Case
///
/// This use case handles the business logic for matching a new order against
/// the orderbook. It represents the primary operation of the matching engine.
///
/// ## Responsibilities
/// - Validate order parameters
/// - Execute matching logic via orderbook
/// - Generate trade confirmations
/// - Emit events/notifications
///
/// ## Future Enhancements
/// - Pre-trade risk checks
/// - Order validation rules
/// - Audit logging
/// - Market data updates

use crate::shared::protocol::{NewOrderRequest, OrderConfirmation, TradeNotification};
use smallvec::SmallVec;

/// Result of matching an order
#[derive(Debug)]
pub struct MatchOrderResult {
    /// Trades generated from matching
    pub trades: SmallVec<[TradeNotification; 8]>,

    /// Confirmation for any resting order
    pub confirmation: Option<OrderConfirmation>,
}

/// Match Order Use Case
///
/// Currently this is a thin wrapper around the orderbook's match logic.
/// In the future, this will include additional business logic like:
/// - Order validation
/// - Risk checks
/// - Event emission
/// - Metrics collection
pub struct MatchOrderUseCase;

impl MatchOrderUseCase {
    /// Creates a new match order use case
    pub fn new() -> Self {
        Self
    }

    /// Executes the match order use case
    ///
    /// # Arguments
    /// * `request` - The new order request
    ///
    /// # Returns
    /// Result containing trades and confirmation
    ///
    /// # Future Work
    /// This will be refactored to accept an orderbook trait and perform
    /// additional validation and business logic.
    pub fn execute(&self, _request: NewOrderRequest) -> Result<MatchOrderResult, String> {
        // TODO: Implement use case logic
        // 1. Validate order (price > 0, quantity > 0, etc.)
        // 2. Check risk limits
        // 3. Call orderbook.match_order()
        // 4. Generate events
        // 5. Update metrics

        Err("Not yet implemented - use MatchingService directly".to_string())
    }
}

impl Default for MatchOrderUseCase {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_match_order_use_case_creation() {
        let _use_case = MatchOrderUseCase::new();
        // Placeholder test
    }
}
