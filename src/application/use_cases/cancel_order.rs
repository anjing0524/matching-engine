/// Cancel Order Use Case
///
/// This use case handles the business logic for canceling an existing order
/// from the orderbook.
///
/// ## Responsibilities
/// - Validate cancel request
/// - Remove order from orderbook
/// - Generate cancel confirmation
/// - Handle errors (order not found, authorization, etc.)
///
/// ## Workflow
/// 1. Validate the cancel request
/// 2. Check authorization (optional - user owns order)
/// 3. Cancel the order via orderbook
/// 4. Return confirmation
///
/// ## Example
/// ```rust,ignore
/// use matching_engine::application::use_cases::CancelOrderUseCase;
/// use matching_engine::domain::orderbook::TickBasedOrderBook;
///
/// let orderbook = TickBasedOrderBook::new(spec);
/// let use_case = CancelOrderUseCase::new(orderbook);
///
/// let result = use_case.execute(cancel_request)?;
/// ```
use crate::domain::orderbook::OrderBook;
use crate::shared::protocol::CancelOrderRequest;

/// Result of canceling an order
#[derive(Debug)]
pub struct CancelOrderResult {
    /// Whether the cancel was successful
    pub success: bool,

    /// Optional error message if cancel failed
    pub error: Option<String>,

    /// Order ID that was canceled
    pub order_id: u64,
}

/// Error types for cancel order use case
#[derive(Debug)]
pub enum CancelOrderError {
    /// Order not found in orderbook
    OrderNotFound(u64),

    /// Authorization failed (user doesn't own order)
    Unauthorized { order_id: u64, user_id: u64 },

    /// Orderbook error
    OrderbookError(String),
}

impl std::fmt::Display for CancelOrderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CancelOrderError::OrderNotFound(id) => write!(f, "Order {id} not found"),
            CancelOrderError::Unauthorized { order_id, user_id } => {
                write!(
                    f,
                    "User {user_id} not authorized to cancel order {order_id}"
                )
            }
            CancelOrderError::OrderbookError(e) => write!(f, "Orderbook error: {e}"),
        }
    }
}

impl std::error::Error for CancelOrderError {}

/// Cancel Order Use Case
///
/// Generic over OrderBook implementation to support dependency injection.
///
/// # Type Parameters
/// * `OB` - OrderBook implementation (must implement `OrderBook` trait)
pub struct CancelOrderUseCase<OB: OrderBook> {
    orderbook: OB,
    /// Whether to check authorization (user owns order)
    #[allow(dead_code)]
    check_authorization: bool,
}

impl<OB: OrderBook> CancelOrderUseCase<OB> {
    /// Creates a new cancel order use case with authorization checking enabled
    ///
    /// # Arguments
    /// * `orderbook` - The orderbook implementation to use
    ///
    /// # Note
    /// Authorization checking is now always enabled for security.
    /// The orderbook verifies that the user requesting cancellation owns the order.
    pub fn new(orderbook: OB) -> Self {
        Self {
            orderbook,
            check_authorization: true, // Always enabled for production security
        }
    }

    /// Creates a new cancel order use case with authorization checking enabled
    ///
    /// # Arguments
    /// * `orderbook` - The orderbook implementation to use
    ///
    /// # Note
    /// This is an alias for `new()`. Authorization is always enabled.
    #[deprecated(
        since = "4.0.0",
        note = "Use `new()` instead. Authorization is always enabled."
    )]
    pub fn with_authorization(orderbook: OB) -> Self {
        Self::new(orderbook)
    }

    /// Executes the cancel order use case
    ///
    /// # Arguments
    /// * `request` - The cancel order request containing order_id and user_id
    ///
    /// # Returns
    /// * `Ok(CancelOrderResult)` if cancellation succeeded or failed with known reason
    /// * `Err(CancelOrderError)` for unexpected errors including authorization failures
    ///
    /// # Workflow
    /// 1. Cancel the order via orderbook (with authorization check)
    /// 2. Return result
    ///
    /// # Authorization
    /// The orderbook automatically verifies that the requesting user owns the order.
    /// Unauthorized cancellation attempts will result in an error.
    pub fn execute(
        &mut self,
        request: CancelOrderRequest,
    ) -> Result<CancelOrderResult, CancelOrderError> {
        // Cancel the order with authorization check (performed by orderbook)
        match self
            .orderbook
            .cancel_order(request.order_id, request.user_id)
        {
            Ok(()) => Ok(CancelOrderResult {
                success: true,
                error: None,
                order_id: request.order_id,
            }),
            Err(e) => {
                // Check if error indicates authorization failure
                if e.contains("not authorized") {
                    Err(CancelOrderError::Unauthorized {
                        order_id: request.order_id,
                        user_id: request.user_id,
                    })
                }
                // Check if error indicates order not found
                else if e.contains("not found") || e.contains("not yet implemented") {
                    Ok(CancelOrderResult {
                        success: false,
                        error: Some(e),
                        order_id: request.order_id,
                    })
                }
                // Other errors
                else {
                    Err(CancelOrderError::OrderbookError(e))
                }
            }
        }
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
    use crate::domain::orderbook::{ContractSpec, TickBasedOrderBook};

    #[test]
    fn test_cancel_order_use_case_creation() {
        let spec = ContractSpec::new("BTC/USD", 1, 10_000, 100_000);
        let orderbook = TickBasedOrderBook::new(spec);

        let _use_case = CancelOrderUseCase::new(orderbook);
    }

    #[test]
    fn test_cancel_order_not_implemented() {
        let spec = ContractSpec::new("BTC/USD", 1, 10_000, 100_000);
        let orderbook = TickBasedOrderBook::new(spec);

        let mut use_case = CancelOrderUseCase::new(orderbook);

        let cancel_request = CancelOrderRequest {
            order_id: 123,
            user_id: 1,
        };

        let result = use_case.execute(cancel_request);
        assert!(result.is_ok());

        let cancel_result = result.unwrap();
        assert!(!cancel_result.success);
        assert!(cancel_result.error.is_some());
        assert_eq!(cancel_result.order_id, 123);
    }
}
