//! Reference combination-margin calculation.
//!
//! The exchange core does not own member balances. This module is the canonical
//! margin formula used by middleware/risk adapters: margin is charged on the
//! combination's spread notional, with a contract multiplier and a basis-point
//! rate.

/// Margin parameters for one combination product.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComboMarginConfig {
    pub multiplier: u64,
    pub rate_bps: u64,
}

impl ComboMarginConfig {
    #[must_use]
    pub fn new(multiplier: u64, rate_bps: u64) -> Self {
        Self {
            multiplier,
            rate_bps,
        }
    }

    /// Required margin in price-tick × multiplier units.
    #[must_use]
    pub fn required(&self, spread_ticks: u64, quantity: u64) -> u128 {
        let notional = u128::from(spread_ticks)
            .saturating_mul(u128::from(self.multiplier))
            .saturating_mul(u128::from(quantity));
        notional.saturating_mul(u128::from(self.rate_bps)) / 10_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_spread_notional_margin() {
        let config = ComboMarginConfig::new(10, 1_000); // 10%, multiplier 10
        assert_eq!(config.required(1_000, 5), 5_000);
    }

    #[test]
    fn zero_quantity_has_zero_margin() {
        let config = ComboMarginConfig::new(10, 1_000);
        assert_eq!(config.required(1_000, 0), 0);
    }

    #[test]
    fn saturates_instead_of_overflowing() {
        let config = ComboMarginConfig::new(u64::MAX, u64::MAX);
        assert_eq!(config.required(u64::MAX, u64::MAX), u128::MAX / 10_000);
    }
}
