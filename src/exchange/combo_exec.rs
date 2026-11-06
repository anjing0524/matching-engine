//! Atomic two-leg combination execution against leg order books.
//!
//! A combination order has no order book of its own. This slice checks that
//! both leg books can fill the requested quantity, snapshots both books, and
//! executes the two legs as market sweeps. If either leg under-fills, both books
//! are restored, so the core never leaves leg risk.

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
}

struct LegSnapshot {
    orders: Vec<RestingOrder>,
    next_order_id: u64,
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
        let spread =
            i64::try_from(ask1).unwrap_or(i64::MAX) - i64::try_from(bid2).unwrap_or(i64::MAX);
        if spread > i64::try_from(spread_limit_ticks).unwrap_or(i64::MAX) {
            return Err(ComboExecError::SpreadNotSatisfied);
        }
        let available = self
            .available_at(&self.leg1, ask1, OrderType::Sell)
            .min(self.available_at(&self.leg2, bid2, OrderType::Buy));
        if available < quantity {
            return Err(ComboExecError::MissingLiquidity);
        }

        let snapshot1 = LegSnapshot {
            orders: self.leg1.snapshot_orders(),
            next_order_id: self.leg1.next_order_id(),
        };
        let snapshot2 = LegSnapshot {
            orders: self.leg2.snapshot_orders(),
            next_order_id: self.leg2.next_order_id(),
        };

        let filled1 = sweep(
            &mut self.leg1,
            OrderType::Buy,
            self.leg1_spec.max_price,
            quantity,
            1,
        );
        let filled2 = sweep(
            &mut self.leg2,
            OrderType::Sell,
            self.leg2_spec.min_price,
            quantity,
            2,
        );
        if filled1 != quantity || filled2 != quantity {
            self.restore(snapshot1, snapshot2);
            return Err(ComboExecError::LegUnderfilled);
        }

        Ok(ComboFill {
            quantity,
            leg1_price: ask1,
            leg2_price: bid2,
            spread_ticks: spread,
        })
    }

    fn available_at(&self, book: &TickBasedOrderBook, price: u64, side: OrderType) -> u64 {
        book.snapshot_orders()
            .into_iter()
            .filter(|order| order.price == price && order.side == side)
            .map(|order| order.quantity)
            .sum()
    }

    fn restore(&mut self, leg1: LegSnapshot, leg2: LegSnapshot) {
        if let Ok(book) =
            TickBasedOrderBook::restore(self.leg1_spec.clone(), &leg1.orders, leg1.next_order_id)
        {
            self.leg1 = book;
        }
        if let Ok(book) =
            TickBasedOrderBook::restore(self.leg2_spec.clone(), &leg2.orders, leg2.next_order_id)
        {
            self.leg2 = book;
        }
    }
}

fn sweep(
    book: &mut TickBasedOrderBook,
    side: OrderType,
    price: u64,
    quantity: u64,
    user_id: u64,
) -> u64 {
    let request = NewOrderRequest {
        user_id,
        client_order_id: 0,
        symbol: SymbolId::intern("COMBO-LEG"),
        order_type: side,
        price,
        quantity,
    };
    let (trades, _) = book.match_order(request).into_parts();
    trades.iter().map(|trade| trade.matched_quantity).sum()
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
}
