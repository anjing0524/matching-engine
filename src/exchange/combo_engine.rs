//! Combination queue + two-leg executor.
//!
//! Keeps resting combination orders in the logical spread-price/FIFO queue and
//! attempts atomic execution against the two leg books when the queue reaches a
//! marketable spread.

use crate::domain::orderbook::ContractSpec;
use crate::exchange::combo::ComboQueue;
use crate::exchange::combo_exec::{ComboExecError, ComboExecutor, ComboFill};
use crate::exchange::protocol::Side;

/// Combines the combo queue with the two-leg executor.
pub struct ComboEngine {
    queue: ComboQueue,
    executor: ComboExecutor,
}

impl ComboEngine {
    #[must_use]
    pub fn new(leg1_spec: ContractSpec, leg2_spec: ContractSpec) -> Self {
        Self {
            queue: ComboQueue::new(),
            executor: ComboExecutor::new(leg1_spec, leg2_spec),
        }
    }

    pub fn queue(
        &mut self,
        order_id: u64,
        user_id: u64,
        side: Side,
        spread: u64,
        qty: u64,
    ) -> bool {
        self.queue.push(order_id, user_id, side, spread, qty)
    }

    /// Attempts to execute the best resting order on `side`.
    ///
    /// The order stays queued if the leg books cannot fill it at its spread.
    pub fn try_execute_best(&mut self, side: Side) -> Option<Result<ComboFill, ComboExecError>> {
        let order = self.queue.peek_best(side)?;
        let result = match side {
            Side::Buy => self
                .executor
                .execute_buy(order.spread_price_ticks, order.quantity),
            Side::Sell => self
                .executor
                .execute_sell(order.spread_price_ticks, order.quantity),
        };
        match result {
            Ok(fill) => {
                self.queue.cancel(order.order_id);
                Some(Ok(fill))
            }
            Err(error) => Some(Err(error)),
        }
    }

    /// Repeatedly executes the best bid and ask until neither can proceed.
    pub fn run(&mut self) -> Vec<ComboFill> {
        let mut fills = Vec::new();
        loop {
            let mut progressed = false;
            for side in [Side::Buy, Side::Sell] {
                if let Some(Ok(fill)) = self.try_execute_best(side) {
                    fills.push(fill);
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
        fills
    }

    #[must_use]
    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    #[must_use]
    pub fn executor(&self) -> &ComboExecutor {
        &self.executor
    }

    pub fn executor_mut(&mut self) -> &mut ComboExecutor {
        &mut self.executor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::protocol::OrderType;

    fn engine() -> ComboEngine {
        let mut engine = ComboEngine::new(
            ContractSpec::new("LEG1", 1, 1_000, 100_000),
            ContractSpec::new("LEG2", 1, 1, 100_000),
        );
        engine
            .executor_mut()
            .leg1
            .insert_resting_order(2, OrderType::Sell, 50_000, 5, 5, 1)
            .unwrap();
        engine
            .executor_mut()
            .leg2
            .insert_resting_order(3, OrderType::Buy, 100, 5, 5, 2)
            .unwrap();
        engine
    }

    #[test]
    fn executes_marketable_buy_from_queue() {
        let mut engine = engine();
        assert!(engine.queue(10, 1, Side::Buy, 49_900, 5));
        let fill = engine.try_execute_best(Side::Buy).unwrap().unwrap();
        assert_eq!(fill.quantity, 5);
        assert_eq!(engine.queue_len(), 0);
    }

    #[test]
    fn leaves_order_queued_when_spread_fails() {
        let mut engine = engine();
        assert!(engine.queue(10, 1, Side::Buy, 49_000, 5));
        let result = engine.try_execute_best(Side::Buy).unwrap();
        assert_eq!(result, Err(ComboExecError::SpreadNotSatisfied));
        assert_eq!(engine.queue_len(), 1);
    }

    #[test]
    fn run_executes_both_sides() {
        let mut engine = engine();
        engine.queue(10, 1, Side::Buy, 49_900, 5);
        let fills = engine.run();
        assert_eq!(fills.len(), 1);
        assert_eq!(engine.queue_len(), 0);
    }
}
