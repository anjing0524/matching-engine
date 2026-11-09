//! Atomic two-leg combination execution against leg order books.
//!
//! A combination order has no order book of its own. The executor first asks
//! each book for a non-mutating trial fill (filled quantity and worst price),
//! checks the worst-case spread, then sweeps both books. The trial makes the
//! execution all-or-nothing without copying either book.

use crate::domain::orderbook::{ContractSpec, RestingOrder, TickBasedOrderBook};
use crate::shared::SymbolId;
use crate::shared::protocol::{NewOrderRequest, OrderType};

/// One atomic combination execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComboFill {
    pub quantity: u64,
    pub leg1_price: u64,
    pub leg2_price: u64,
    /// leg1_price - leg2_price, the realized spread.
    pub spread_ticks: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComboExecError {
    MissingLiquidity,
    SpreadNotSatisfied,
    LegUnderfilled,
    PositionUnavailable,
}

/// Two leg books plus their contract specs.
pub struct ComboExecutor {
    pub leg1: TickBasedOrderBook,
    pub leg2: TickBasedOrderBook,
    leg1_spec: ContractSpec,
    leg2_spec: ContractSpec,
}

impl ComboExecutor {
    #[must_use]
    pub fn new(leg1_spec: ContractSpec, leg2_spec: ContractSpec) -> Self {
        Self {
            leg1: TickBasedOrderBook::new(leg1_spec.clone()),
            leg2: TickBasedOrderBook::new(leg2_spec.clone()),
            leg1_spec,
            leg2_spec,
        }
    }

    /// Buys the spread (buy leg1, sell leg2) atomically.
    ///
    /// # Errors
    /// Returns an error without mutating either book when liquidity or spread
    /// is insufficient.
    pub fn execute_buy(
        &mut self,
        spread_limit_ticks: u64,
        quantity: u64,
    ) -> Result<ComboFill, ComboExecError> {
        let ask1 = self
            .leg1
            .best_ask()
            .ok_or(ComboExecError::MissingLiquidity)?;
        let bid2 = self
            .leg2
            .best_bid()
            .ok_or(ComboExecError::MissingLiquidity)?;
        let worst_leg1 = self
            .leg1
            .trial_execution(OrderType::Buy, self.leg1_spec.max_price, quantity)
            .ok_or(ComboExecError::MissingLiquidity)?;
        let worst_leg2 = self
            .leg2
            .trial_execution(OrderType::Sell, self.leg2_spec.min_price, quantity)
            .ok_or(ComboExecError::MissingLiquidity)?;
        let realized = i64::try_from(worst_leg1).unwrap_or(i64::MAX)
            - i64::try_from(worst_leg2).unwrap_or(i64::MAX);
        if realized > i64::try_from(spread_limit_ticks).unwrap_or(i64::MAX) {
            return Err(ComboExecError::SpreadNotSatisfied);
        }

        let (filled1, _) = sweep(
            &mut self.leg1,
            OrderType::Buy,
            self.leg1_spec.max_price,
            quantity,
            1,
        );
        let (filled2, _) = sweep(
            &mut self.leg2,
            OrderType::Sell,
            self.leg2_spec.min_price,
            quantity,
            2,
        );
        debug_assert_eq!(filled1, quantity, "leg1 trial fill mismatch");
        debug_assert_eq!(filled2, quantity, "leg2 trial fill mismatch");
        if filled1 != quantity || filled2 != quantity {
            return Err(ComboExecError::LegUnderfilled);
        }

        Ok(ComboFill {
            quantity,
            leg1_price: ask1,
            leg2_price: bid2,
            spread_ticks: i64::try_from(ask1).unwrap_or(i64::MAX)
                - i64::try_from(bid2).unwrap_or(i64::MAX),
        })
    }

    /// Sells the spread (sell leg1, buy leg2) atomically.
    ///
    /// # Errors
    /// Returns an error without mutating either book when liquidity or spread
    /// is insufficient.
    pub fn execute_sell(
        &mut self,
        spread_limit_ticks: u64,
        quantity: u64,
    ) -> Result<ComboFill, ComboExecError> {
        let bid1 = self
            .leg1
            .best_bid()
            .ok_or(ComboExecError::MissingLiquidity)?;
        let ask2 = self
            .leg2
            .best_ask()
            .ok_or(ComboExecError::MissingLiquidity)?;
        let worst_leg1 = self
            .leg1
            .trial_execution(OrderType::Sell, self.leg1_spec.min_price, quantity)
            .ok_or(ComboExecError::MissingLiquidity)?;
        let worst_leg2 = self
            .leg2
            .trial_execution(OrderType::Buy, self.leg2_spec.max_price, quantity)
            .ok_or(ComboExecError::MissingLiquidity)?;
        let realized = i64::try_from(worst_leg1).unwrap_or(i64::MIN)
            - i64::try_from(worst_leg2).unwrap_or(i64::MAX);
        if realized < i64::try_from(spread_limit_ticks).unwrap_or(i64::MIN) {
            return Err(ComboExecError::SpreadNotSatisfied);
        }

        let (filled1, _) = sweep(
            &mut self.leg1,
            OrderType::Sell,
            self.leg1_spec.min_price,
            quantity,
            1,
        );
        let (filled2, _) = sweep(
            &mut self.leg2,
            OrderType::Buy,
            self.leg2_spec.max_price,
            quantity,
            2,
        );
        debug_assert_eq!(filled1, quantity, "leg1 trial fill mismatch");
        debug_assert_eq!(filled2, quantity, "leg2 trial fill mismatch");
        if filled1 != quantity || filled2 != quantity {
            return Err(ComboExecError::LegUnderfilled);
        }

        Ok(ComboFill {
            quantity,
            leg1_price: bid1,
            leg2_price: ask2,
            spread_ticks: i64::try_from(bid1).unwrap_or(i64::MAX)
                - i64::try_from(ask2).unwrap_or(i64::MAX),
        })
    }

    #[must_use]
    pub fn snapshot_books(&self) -> (Vec<RestingOrder>, Vec<RestingOrder>, u64, u64) {
        (
            self.leg1.snapshot_orders(),
            self.leg2.snapshot_orders(),
            self.leg1.next_order_id(),
            self.leg2.next_order_id(),
        )
    }

    /// Restores both leg books from a snapshot.
    ///
    /// # Errors
    /// Returns a reason when either book rejects its orders.
    pub fn restore_books(
        &mut self,
        orders1: &[RestingOrder],
        orders2: &[RestingOrder],
        next1: u64,
        next2: u64,
    ) -> Result<(), String> {
        self.leg1 = TickBasedOrderBook::restore(self.leg1_spec.clone(), orders1, next1)?;
        self.leg2 = TickBasedOrderBook::restore(self.leg2_spec.clone(), orders2, next2)?;
        Ok(())
    }
}

fn sweep(
    book: &mut TickBasedOrderBook,
    side: OrderType,
    price: u64,
    quantity: u64,
    user_id: u64,
) -> (u64, Vec<(u64, u64)>) {
    let request = NewOrderRequest {
        user_id,
        client_order_id: 0,
        symbol: SymbolId::intern("COMBO-LEG"),
        order_type: side,
        price,
        quantity,
    };
    let (trades, _) = book.match_order(request).into_parts();
    let total = trades.iter().map(|trade| trade.matched_quantity).sum();
    let prices = trades
        .iter()
        .map(|trade| (trade.matched_price, trade.matched_quantity))
        .collect();
    (total, prices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::orderbook::ContractSpec;

    fn executor() -> ComboExecutor {
        let leg1 = ContractSpec::new("LEG1", 1, 1_000, 100_000);
        let leg2 = ContractSpec::new("LEG2", 1, 1, 100_000);
        let mut executor = ComboExecutor::new(leg1, leg2);
        executor
            .leg1
            .insert_resting_order(2, OrderType::Sell, 50_000, 5, 5, 1)
            .unwrap();
        executor
            .leg2
            .insert_resting_order(3, OrderType::Buy, 100, 5, 5, 2)
            .unwrap();
        executor
    }

    fn sell_executor() -> ComboExecutor {
        let leg1 = ContractSpec::new("LEG1", 1, 1_000, 100_000);
        let leg2 = ContractSpec::new("LEG2", 1, 1, 100_000);
        let mut executor = ComboExecutor::new(leg1, leg2);
        executor
            .leg1
            .insert_resting_order(2, OrderType::Buy, 50_000, 5, 5, 1)
            .unwrap();
        executor
            .leg2
            .insert_resting_order(3, OrderType::Sell, 100, 5, 5, 2)
            .unwrap();
        executor
    }

    #[test]
    fn sells_both_legs_atomically() {
        let mut executor = sell_executor();
        let fill = executor.execute_sell(49_000, 5).unwrap();
        assert_eq!(fill.quantity, 5);
        assert_eq!(fill.spread_ticks, 49_900);
        assert_eq!(executor.leg1.best_bid(), None);
        assert_eq!(executor.leg2.best_ask(), None);
    }

    #[test]
    fn rejects_sell_when_spread_is_too_narrow() {
        let mut executor = sell_executor();
        let error = executor.execute_sell(50_000, 5).unwrap_err();
        assert_eq!(error, ComboExecError::SpreadNotSatisfied);
        assert_eq!(executor.leg1.best_bid(), Some(50_000));
        assert_eq!(executor.leg2.best_ask(), Some(100));
    }

    #[test]
    fn buys_both_legs_atomically() {
        let mut executor = executor();
        let fill = executor.execute_buy(49_900, 5).unwrap();
        assert_eq!(fill.quantity, 5);
        assert_eq!(fill.leg1_price, 50_000);
        assert_eq!(fill.leg2_price, 100);
        assert_eq!(fill.spread_ticks, 49_900);
        assert_eq!(executor.leg1.best_ask(), None);
        assert_eq!(executor.leg2.best_bid(), None);
    }

    #[test]
    fn sweeps_multiple_leg_levels() {
        let leg1 = ContractSpec::new("LEG1", 1, 1_000, 100_000);
        let leg2 = ContractSpec::new("LEG2", 1, 1, 100_000);
        let mut executor = ComboExecutor::new(leg1, leg2);
        executor
            .leg1
            .insert_resting_order(2, OrderType::Sell, 50_000, 1, 1, 1)
            .unwrap();
        executor
            .leg1
            .insert_resting_order(2, OrderType::Sell, 50_010, 4, 4, 2)
            .unwrap();
        executor
            .leg2
            .insert_resting_order(3, OrderType::Buy, 100, 5, 5, 3)
            .unwrap();
        let fill = executor.execute_buy(49_910, 5).unwrap();
        assert_eq!(fill.quantity, 5);
        assert_eq!(executor.leg1.best_ask(), None);
        assert_eq!(executor.leg2.best_bid(), None);
    }

    #[test]
    fn rejects_when_spread_is_too_wide_without_mutation() {
        let mut executor = executor();
        let error = executor.execute_buy(49_000, 5).unwrap_err();
        assert_eq!(error, ComboExecError::SpreadNotSatisfied);
        assert_eq!(executor.leg1.best_ask(), Some(50_000));
        assert_eq!(executor.leg2.best_bid(), Some(100));
    }

    #[test]
    fn rejects_when_quantity_exceeds_top_liquidity() {
        let mut executor = executor();
        let error = executor.execute_buy(49_900, 6).unwrap_err();
        assert_eq!(error, ComboExecError::MissingLiquidity);
        assert_eq!(executor.leg1.best_ask(), Some(50_000));
    }
    #[test]
    fn rejects_multi_level_worst_spread_without_mutation() {
        let leg1 = ContractSpec::new("LEG1", 1, 1_000, 100_000);
        let leg2 = ContractSpec::new("LEG2", 1, 1, 100_000);
        let mut executor = ComboExecutor::new(leg1, leg2);
        executor
            .leg1
            .insert_resting_order(2, OrderType::Sell, 50_000, 1, 1, 1)
            .unwrap();
        executor
            .leg1
            .insert_resting_order(2, OrderType::Sell, 50_010, 4, 4, 2)
            .unwrap();
        executor
            .leg2
            .insert_resting_order(3, OrderType::Buy, 100, 5, 5, 3)
            .unwrap();
        let error = executor.execute_buy(49_905, 5).unwrap_err();
        assert_eq!(error, ComboExecError::SpreadNotSatisfied);
        assert_eq!(executor.leg1.best_ask(), Some(50_000));
        assert_eq!(executor.leg1.side_quantity(OrderType::Sell), 5);
        assert_eq!(executor.leg2.side_quantity(OrderType::Buy), 5);
    }
}
