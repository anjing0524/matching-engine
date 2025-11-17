/// Match Order Use Case
///
/// This use case handles the business logic for matching a new order against
/// the orderbook. It represents the primary operation of the matching engine.
///
/// ## Responsibilities
/// - Validate order parameters
/// - Execute matching logic via orderbook
/// - Generate trade confirmations
/// - Handle errors and edge cases
///
/// ## Workflow
/// 1. Validate the order request (price, quantity, symbol)
/// 2. Submit to orderbook for matching
/// 3. Return trades and confirmations
///
/// ## Example
/// ```rust,ignore
/// use matching_engine::application::use_cases::MatchOrderUseCase;
/// use matching_engine::domain::orderbook::TickBasedOrderBook;
/// use matching_engine::domain::validation::OrderValidator;
///
/// let validator = OrderValidator::new();
/// let orderbook = TickBasedOrderBook::new(spec);
/// let use_case = MatchOrderUseCase::new(orderbook, validator);
///
/// let result = use_case.execute(order_request)?;
/// ```

use crate::domain::orderbook::OrderBook;
use crate::domain::validation::{OrderValidator, ValidationError};
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

/// Error types for match order use case
#[derive(Debug)]
pub enum MatchOrderError {
    /// Validation failed
    ValidationFailed(ValidationError),

    /// Orderbook error
    OrderbookError(String),
}

impl std::fmt::Display for MatchOrderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MatchOrderError::ValidationFailed(e) => write!(f, "Validation failed: {}", e),
            MatchOrderError::OrderbookError(e) => write!(f, "Orderbook error: {}", e),
        }
    }
}

impl std::error::Error for MatchOrderError {}

impl From<ValidationError> for MatchOrderError {
    fn from(e: ValidationError) -> Self {
        MatchOrderError::ValidationFailed(e)
    }
}

/// Match Order Use Case
///
/// Generic over OrderBook implementation to support dependency injection.
///
/// # Type Parameters
/// * `OB` - OrderBook implementation (must implement `OrderBook` trait)
pub struct MatchOrderUseCase<OB: OrderBook> {
    orderbook: OB,
    validator: OrderValidator,
}

impl<OB: OrderBook> MatchOrderUseCase<OB> {
    /// Creates a new match order use case
    ///
    /// # Arguments
    /// * `orderbook` - The orderbook implementation to use
    /// * `validator` - The order validator for business rule validation
    pub fn new(orderbook: OB, validator: OrderValidator) -> Self {
        Self {
            orderbook,
            validator,
        }
    }

    /// Executes the match order use case
    ///
    /// # Arguments
    /// * `request` - The new order request
    ///
    /// # Returns
    /// * `Ok(MatchOrderResult)` containing trades and confirmation
    /// * `Err(MatchOrderError)` if validation or matching fails
    ///
    /// # Workflow
    /// 1. Validate the order request
    /// 2. Submit to orderbook for matching
    /// 3. Return results
    pub fn execute(&mut self, request: NewOrderRequest) -> Result<MatchOrderResult, MatchOrderError> {
        // Step 1: Validate the order
        self.validator.validate(&request)?;

        // Step 2: Match the order
        let (trades, confirmation) = self.orderbook.match_order(request);

        // Step 3: Return results
        Ok(MatchOrderResult {
            trades,
            confirmation,
        })
    }

    /// Gets a reference to the orderbook (for testing/inspection)
    pub fn orderbook(&self) -> &OB {
        &self.orderbook
    }

    /// Gets a mutable reference to the orderbook (for testing/inspection)
    pub fn orderbook_mut(&mut self) -> &mut OB {
        &mut self.orderbook
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::orderbook::{TickBasedOrderBook, ContractSpec};
    use crate::shared::protocol::OrderType;
    use std::sync::Arc;

    #[test]
    fn test_match_order_use_case_creation() {
        let spec = ContractSpec::new("BTC/USD", 1, 10000, 100000);
        let orderbook = TickBasedOrderBook::new(spec);
        let validator = OrderValidator::new();

        let _use_case = MatchOrderUseCase::new(orderbook, validator);
    }

    #[test]
    fn test_match_order_validation_failure() {
        let spec = ContractSpec::new("BTC/USD", 1, 10000, 100000);
        let orderbook = TickBasedOrderBook::new(spec);
        let validator = OrderValidator::new();

        let mut use_case = MatchOrderUseCase::new(orderbook, validator);

        // Invalid order (zero price)
        let invalid_order = NewOrderRequest {
            user_id: 1,
            symbol: Arc::from("BTC/USD"),
            order_type: OrderType::Buy,
            price: 0, // Invalid!
            quantity: 10,
        };

        let result = use_case.execute(invalid_order);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), MatchOrderError::ValidationFailed(_)));
    }

    #[test]
    fn test_match_order_success() {
        let spec = ContractSpec::new("BTC/USD", 1, 10000, 100000);
        let orderbook = TickBasedOrderBook::new(spec);
        let validator = OrderValidator::new();

        let mut use_case = MatchOrderUseCase::new(orderbook, validator);

        // Valid order
        let order = NewOrderRequest {
            user_id: 1,
            symbol: Arc::from("BTC/USD"),
            order_type: OrderType::Buy,
            price: 50000,
            quantity: 10,
        };

        let result = use_case.execute(order);
        assert!(result.is_ok());

        let match_result = result.unwrap();
        // First order should not generate trades (no opposite side)
        assert_eq!(match_result.trades.len(), 0);
        assert!(match_result.confirmation.is_some());
    }
}
