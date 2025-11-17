/// OrderBook Trait - Domain Layer Abstraction
///
/// This trait defines the core interface that all orderbook implementations must satisfy.
/// It enables dependency injection and allows different orderbook implementations to be
/// used interchangeably.
///
/// ## Design Principles
/// - **Domain-Driven**: Interface defined by business requirements, not implementation
/// - **Zero-Cost Abstraction**: Trait compiles to direct method calls (no vtable overhead)
/// - **Testability**: Easy to mock for unit testing
/// - **Flexibility**: Support multiple implementations (Array, BTreeMap, HashMap, etc.)
///
/// ## Implementations
/// - `TickBasedOrderBook`: Production implementation for futures/derivatives
/// - Future: `BTreeMapOrderBook`, `SpotOrderBook`, `MockOrderBook`
///
/// ## Example
/// ```rust
/// use matching_engine::domain::orderbook::{OrderBook, TickBasedOrderBook, ContractSpec};
/// use matching_engine::shared::protocol::{NewOrderRequest, OrderType};
/// use std::sync::Arc;
///
/// fn process_order<OB: OrderBook>(orderbook: &mut OB, request: NewOrderRequest) {
///     let (trades, confirmation) = orderbook.match_order(request);
///     println!("Generated {} trades", trades.len());
/// }
///
/// // Use with any implementation
/// let spec = ContractSpec::new("BTC/USD", 1, 10000, 100000);
/// let mut ob = TickBasedOrderBook::new(spec);
/// // process_order(&mut ob, request);
/// ```

use crate::shared::protocol::{NewOrderRequest, OrderConfirmation, TradeNotification};
use smallvec::SmallVec;

/// Core OrderBook trait
///
/// Defines the essential operations that any orderbook implementation must provide.
/// This trait is designed for high-performance matching engines where trait methods
/// are monomorphized at compile time for zero-cost abstraction.
pub trait OrderBook {
    /// Matches a new order against the orderbook
    ///
    /// This is the core operation of the matching engine. It attempts to match
    /// the incoming order against existing orders at compatible prices.
    ///
    /// # Arguments
    /// * `request` - The new order to match
    ///
    /// # Returns
    /// A tuple containing:
    /// * `SmallVec<TradeNotification>` - Trades generated from matching (0-8 trades typical)
    /// * `Option<OrderConfirmation>` - Confirmation for any remaining unmatched quantity
    ///
    /// # Matching Logic
    /// - **Buy orders**: Match against ask side (sellers), lowest price first
    /// - **Sell orders**: Match against bid side (buyers), highest price first
    /// - **Price-time priority**: Orders at same price matched in FIFO order
    /// - **Partial fills**: Remaining quantity becomes a resting order
    ///
    /// # Performance
    /// Implementations should optimize for:
    /// - O(1) or O(log n) order insertion
    /// - O(1) best price lookup
    /// - Minimal allocations (use SmallVec to avoid heap for common cases)
    ///
    /// # Example
    /// ```rust,ignore
    /// let (trades, confirmation) = orderbook.match_order(NewOrderRequest {
    ///     user_id: 123,
    ///     symbol: Arc::from("BTC/USD"),
    ///     order_type: OrderType::Buy,
    ///     price: 50000,
    ///     quantity: 10,
    /// });
    ///
    /// for trade in trades {
    ///     println!("Trade: {} @ {}", trade.matched_quantity, trade.matched_price);
    /// }
    /// ```
    fn match_order(
        &mut self,
        request: NewOrderRequest,
    ) -> (SmallVec<[TradeNotification; 8]>, Option<OrderConfirmation>);

    /// Cancels an existing order
    ///
    /// Removes an order from the orderbook by its order ID.
    ///
    /// # Arguments
    /// * `order_id` - The ID of the order to cancel
    ///
    /// # Returns
    /// * `Ok(())` if the order was successfully canceled
    /// * `Err(String)` if the order was not found or cannot be canceled
    ///
    /// # Future Implementation
    /// This is currently a placeholder. Full implementation will include:
    /// - Order ID to price level mapping
    /// - Efficient removal from price level queue
    /// - Bitmap updates for empty price levels
    ///
    /// # Example
    /// ```rust,ignore
    /// match orderbook.cancel_order(order_id) {
    ///     Ok(()) => println!("Order {} canceled", order_id),
    ///     Err(e) => eprintln!("Cancel failed: {}", e),
    /// }
    /// ```
    fn cancel_order(&mut self, order_id: u64) -> Result<(), String> {
        // Default implementation returns "not implemented"
        Err(format!("Cancel order {} not yet implemented", order_id))
    }

    /// Gets the best (highest) bid price
    ///
    /// # Returns
    /// * `Some(price)` if there are any buy orders
    /// * `None` if the bid side is empty
    ///
    /// # Performance
    /// Should be O(1) for efficient implementations using indexing or caching.
    fn get_best_bid(&self) -> Option<u64> {
        None // Default implementation
    }

    /// Gets the best (lowest) ask price
    ///
    /// # Returns
    /// * `Some(price)` if there are any sell orders
    /// * `None` if the ask side is empty
    ///
    /// # Performance
    /// Should be O(1) for efficient implementations using indexing or caching.
    fn get_best_ask(&self) -> Option<u64> {
        None // Default implementation
    }

    /// Gets the current spread (best_ask - best_bid)
    ///
    /// # Returns
    /// * `Some(spread)` if both bid and ask exist
    /// * `None` if either side is empty
    ///
    /// This is a convenience method with a default implementation.
    fn get_spread(&self) -> Option<u64> {
        match (self.get_best_bid(), self.get_best_ask()) {
            (Some(bid), Some(ask)) if ask > bid => Some(ask - bid),
            _ => None,
        }
    }

    /// Gets the midpoint price ((best_bid + best_ask) / 2)
    ///
    /// # Returns
    /// * `Some(midpoint)` if both bid and ask exist
    /// * `None` if either side is empty
    ///
    /// This is useful for market data and analytics.
    fn get_mid_price(&self) -> Option<u64> {
        match (self.get_best_bid(), self.get_best_ask()) {
            (Some(bid), Some(ask)) => Some((bid + ask) / 2),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::protocol::OrderType;
    use std::sync::Arc;

    // Mock implementation for testing
    struct MockOrderBook {
        best_bid: Option<u64>,
        best_ask: Option<u64>,
    }

    impl OrderBook for MockOrderBook {
        fn match_order(
            &mut self,
            _request: NewOrderRequest,
        ) -> (SmallVec<[TradeNotification; 8]>, Option<OrderConfirmation>) {
            (SmallVec::new(), None)
        }

        fn get_best_bid(&self) -> Option<u64> {
            self.best_bid
        }

        fn get_best_ask(&self) -> Option<u64> {
            self.best_ask
        }
    }

    #[test]
    fn test_trait_spread_calculation() {
        let mut mock = MockOrderBook {
            best_bid: Some(99),
            best_ask: Some(101),
        };

        assert_eq!(mock.get_spread(), Some(2));
        assert_eq!(mock.get_mid_price(), Some(100));
    }

    #[test]
    fn test_trait_empty_orderbook() {
        let mut mock = MockOrderBook {
            best_bid: None,
            best_ask: None,
        };

        assert_eq!(mock.get_spread(), None);
        assert_eq!(mock.get_mid_price(), None);
    }

    #[test]
    fn test_default_cancel_order() {
        let mut mock = MockOrderBook {
            best_bid: None,
            best_ask: None,
        };

        assert!(mock.cancel_order(123).is_err());
    }
}
