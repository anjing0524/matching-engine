/// Order Validator - Business Rule Validation
///
/// This module provides validation logic for order requests to ensure they
/// meet business requirements before being processed by the matching engine.
///
/// ## Validation Rules
/// - Price must be positive
/// - Quantity must be positive
/// - Symbol must not be empty
/// - Price and quantity should not exceed maximum limits
///
/// ## Usage
/// ```rust
/// use matching_engine::domain::validation::OrderValidator;
/// use matching_engine::shared::protocol::NewOrderRequest;
///
/// let validator = OrderValidator::new();
/// match validator.validate(&order_request) {
///     Ok(()) => println!("Order is valid"),
///     Err(e) => println!("Validation error: {}", e),
/// }
/// ```

use crate::shared::protocol::NewOrderRequest;
use std::sync::Arc;

/// Validation errors
#[derive(Debug, Clone, PartialEq)]
pub enum ValidationError {
    /// Price is zero or negative
    InvalidPrice(String),

    /// Quantity is zero or negative
    InvalidQuantity(String),

    /// Symbol is empty or invalid
    InvalidSymbol(String),

    /// Price exceeds maximum allowed
    PriceOutOfRange(String),

    /// Quantity exceeds maximum allowed
    QuantityOutOfRange(String),
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::InvalidPrice(msg) => write!(f, "Invalid price: {}", msg),
            ValidationError::InvalidQuantity(msg) => write!(f, "Invalid quantity: {}", msg),
            ValidationError::InvalidSymbol(msg) => write!(f, "Invalid symbol: {}", msg),
            ValidationError::PriceOutOfRange(msg) => write!(f, "Price out of range: {}", msg),
            ValidationError::QuantityOutOfRange(msg) => write!(f, "Quantity out of range: {}", msg),
        }
    }
}

impl std::error::Error for ValidationError {}

/// Order validation configuration
#[derive(Debug, Clone)]
pub struct ValidationConfig {
    /// Minimum price (inclusive)
    pub min_price: u64,

    /// Maximum price (inclusive)
    pub max_price: u64,

    /// Minimum quantity (inclusive)
    pub min_quantity: u64,

    /// Maximum quantity (inclusive)
    pub max_quantity: u64,

    /// Allowed symbols (empty means all symbols allowed)
    pub allowed_symbols: Vec<Arc<str>>,
}

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            min_price: 1,
            max_price: u64::MAX,
            min_quantity: 1,
            max_quantity: 1_000_000,
            allowed_symbols: Vec::new(),
        }
    }
}

/// Order validator
///
/// Validates order requests according to business rules and configuration.
pub struct OrderValidator {
    config: ValidationConfig,
}

impl OrderValidator {
    /// Creates a new validator with default configuration
    pub fn new() -> Self {
        Self {
            config: ValidationConfig::default(),
        }
    }

    /// Creates a new validator with custom configuration
    pub fn with_config(config: ValidationConfig) -> Self {
        Self { config }
    }

    /// Validates an order request
    ///
    /// # Arguments
    /// * `request` - The order request to validate
    ///
    /// # Returns
    /// * `Ok(())` if the order is valid
    /// * `Err(ValidationError)` if validation fails
    ///
    /// # Examples
    /// ```rust,ignore
    /// let validator = OrderValidator::new();
    /// let result = validator.validate(&order_request);
    /// ```
    pub fn validate(&self, request: &NewOrderRequest) -> Result<(), ValidationError> {
        // Validate price
        self.validate_price(request.price)?;

        // Validate quantity
        self.validate_quantity(request.quantity)?;

        // Validate symbol
        self.validate_symbol(&request.symbol)?;

        Ok(())
    }

    /// Validates the price
    fn validate_price(&self, price: u64) -> Result<(), ValidationError> {
        if price == 0 {
            return Err(ValidationError::InvalidPrice(
                "Price must be greater than zero".to_string()
            ));
        }

        if price < self.config.min_price {
            return Err(ValidationError::PriceOutOfRange(
                format!("Price {} is below minimum {}", price, self.config.min_price)
            ));
        }

        if price > self.config.max_price {
            return Err(ValidationError::PriceOutOfRange(
                format!("Price {} exceeds maximum {}", price, self.config.max_price)
            ));
        }

        Ok(())
    }

    /// Validates the quantity
    fn validate_quantity(&self, quantity: u64) -> Result<(), ValidationError> {
        if quantity == 0 {
            return Err(ValidationError::InvalidQuantity(
                "Quantity must be greater than zero".to_string()
            ));
        }

        if quantity < self.config.min_quantity {
            return Err(ValidationError::QuantityOutOfRange(
                format!("Quantity {} is below minimum {}", quantity, self.config.min_quantity)
            ));
        }

        if quantity > self.config.max_quantity {
            return Err(ValidationError::QuantityOutOfRange(
                format!("Quantity {} exceeds maximum {}", quantity, self.config.max_quantity)
            ));
        }

        Ok(())
    }

    /// Validates the symbol
    fn validate_symbol(&self, symbol: &Arc<str>) -> Result<(), ValidationError> {
        if symbol.is_empty() {
            return Err(ValidationError::InvalidSymbol(
                "Symbol cannot be empty".to_string()
            ));
        }

        // If allowed_symbols is configured, check if symbol is in the list
        if !self.config.allowed_symbols.is_empty() {
            if !self.config.allowed_symbols.iter().any(|s| s.as_ref() == symbol.as_ref()) {
                return Err(ValidationError::InvalidSymbol(
                    format!("Symbol '{}' is not in allowed list", symbol)
                ));
            }
        }

        Ok(())
    }
}

impl Default for OrderValidator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::protocol::OrderType;

    fn create_valid_order() -> NewOrderRequest {
        NewOrderRequest {
            user_id: 1,
            symbol: Arc::from("BTC/USD"),
            order_type: OrderType::Buy,
            price: 50000,
            quantity: 10,
        }
    }

    #[test]
    fn test_valid_order() {
        let validator = OrderValidator::new();
        let order = create_valid_order();
        assert!(validator.validate(&order).is_ok());
    }

    #[test]
    fn test_zero_price() {
        let validator = OrderValidator::new();
        let mut order = create_valid_order();
        order.price = 0;

        let result = validator.validate(&order);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ValidationError::InvalidPrice(_)));
    }

    #[test]
    fn test_zero_quantity() {
        let validator = OrderValidator::new();
        let mut order = create_valid_order();
        order.quantity = 0;

        let result = validator.validate(&order);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ValidationError::InvalidQuantity(_)));
    }

    #[test]
    fn test_empty_symbol() {
        let validator = OrderValidator::new();
        let mut order = create_valid_order();
        order.symbol = Arc::from("");

        let result = validator.validate(&order);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ValidationError::InvalidSymbol(_)));
    }

    #[test]
    fn test_price_out_of_range() {
        let config = ValidationConfig {
            min_price: 100,
            max_price: 100000,
            ..Default::default()
        };
        let validator = OrderValidator::with_config(config);

        // Price too low
        let mut order = create_valid_order();
        order.price = 50;
        assert!(matches!(
            validator.validate(&order).unwrap_err(),
            ValidationError::PriceOutOfRange(_)
        ));

        // Price too high
        order.price = 200000;
        assert!(matches!(
            validator.validate(&order).unwrap_err(),
            ValidationError::PriceOutOfRange(_)
        ));
    }

    #[test]
    fn test_quantity_out_of_range() {
        let config = ValidationConfig {
            min_quantity: 1,
            max_quantity: 1000,
            ..Default::default()
        };
        let validator = OrderValidator::with_config(config);

        let mut order = create_valid_order();
        order.quantity = 2000;
        assert!(matches!(
            validator.validate(&order).unwrap_err(),
            ValidationError::QuantityOutOfRange(_)
        ));
    }

    #[test]
    fn test_allowed_symbols() {
        let config = ValidationConfig {
            allowed_symbols: vec![Arc::from("BTC/USD"), Arc::from("ETH/USD")],
            ..Default::default()
        };
        let validator = OrderValidator::with_config(config);

        // Valid symbol
        let order = create_valid_order();
        assert!(validator.validate(&order).is_ok());

        // Invalid symbol
        let mut order = create_valid_order();
        order.symbol = Arc::from("XRP/USD");
        assert!(matches!(
            validator.validate(&order).unwrap_err(),
            ValidationError::InvalidSymbol(_)
        ));
    }
}
