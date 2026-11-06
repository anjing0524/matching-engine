//! Combination queue + two-leg executor.
//!
//! Keeps resting combination orders in the logical spread-price/FIFO queue and
//! attempts atomic execution against the two leg books when the queue reaches a
//! marketable spread.

use crate::domain::orderbook::ContractSpec;
use crate::exchange::combo::ComboQueue;
use crate::exchange::combo_exec::{ComboExecError, ComboExecutor, ComboFill};
use crate::exchange::positions::{PositionKey, Positions};
use crate::exchange::protocol::{Hedge, Offset, Side};

/// One completed combination fill, suitable for private/public projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComboFillEvent {
    pub seq: u64,
    pub order_id: u64,
    pub user_id: u64,
    pub side: Side,
    pub fill: ComboFill,
}

/// Combines the combo queue with the two-leg executor.
pub struct ComboEngine {
    queue: ComboQueue,
    executor: ComboExecutor,
    next_event_seq: u64,
    events: Vec<ComboFillEvent>,
    leg1_positions: Positions,
    leg2_positions: Positions,
}

impl ComboEngine {
    #[must_use]
    pub fn new(leg1_spec: ContractSpec, leg2_spec: ContractSpec) -> Self {
        Self {
            queue: ComboQueue::new(),
            executor: ComboExecutor::new(leg1_spec, leg2_spec),
            next_event_seq: 1,
            events: Vec::new(),
            leg1_positions: Positions::new(),
            leg2_positions: Positions::new(),
        }
    }

    pub fn queue(
        &mut self,
        order_id: u64,
        user_id: u64,
        side: Side,
        spread: u64,
        qty: u64,
        offset: Offset,
        hedge: Hedge,
    ) -> bool {
        self.queue
            .push(order_id, user_id, side, spread, qty, offset, hedge)
    }

    /// Attempts to execute the best resting order on `side`.
    ///
    /// The order stays queued if the leg books cannot fill it at its spread.
    pub fn try_execute_best(&mut self, side: Side) -> Option<Result<ComboFill, ComboExecError>> {
        let order = self.queue.peek_best(side)?;
        if order.offset != Offset::Open {
            let (leg1_position, leg2_position) = match side {
                Side::Buy => (Side::Sell, Side::Buy),
                Side::Sell => (Side::Buy, Side::Sell),
            };
            let leg1_key = PositionKey::new(order.user_id, leg1_position, order.hedge);
            let leg2_key = PositionKey::new(order.user_id, leg2_position, order.hedge);
            if self.leg1_positions.available_close(leg1_key, order.offset) < order.quantity
                || self.leg2_positions.available_close(leg2_key, order.offset) < order.quantity
            {
                return Some(Err(ComboExecError::PositionUnavailable));
            }
        }
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
                let seq = self.next_event_seq;
                self.next_event_seq += 1;
                self.events.push(ComboFillEvent {
                    seq,
                    order_id: order.order_id,
                    user_id: order.user_id,
                    side,
                    fill,
                });
                self.apply_combo_positions(&order, fill.quantity);
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

    fn apply_combo_positions(&mut self, order: &crate::exchange::combo::ComboOrder, quantity: u64) {
        let (leg1_side, leg2_side) = match order.side {
            Side::Buy => (Side::Buy, Side::Sell),
            Side::Sell => (Side::Sell, Side::Buy),
        };
        if order.offset == Offset::Open {
            let _ = self.leg1_positions.apply_open(
                PositionKey::new(order.user_id, leg1_side, order.hedge),
                quantity,
            );
            let _ = self.leg2_positions.apply_open(
                PositionKey::new(order.user_id, leg2_side, order.hedge),
                quantity,
            );
        } else {
            let leg1_position = leg1_side.opposite();
            let leg2_position = leg2_side.opposite();
            let _ = self.leg1_positions.apply_close(
                PositionKey::new(order.user_id, leg1_position, order.hedge),
                order.offset,
                quantity,
            );
            let _ = self.leg2_positions.apply_close(
                PositionKey::new(order.user_id, leg2_position, order.hedge),
                order.offset,
                quantity,
            );
        }
    }

    pub fn drain_events(&mut self) -> Vec<ComboFillEvent> {
        std::mem::take(&mut self.events)
    }

    #[must_use]
    pub fn events(&self) -> &[ComboFillEvent] {
        &self.events
    }

    #[must_use]
    pub fn leg1_positions(&self) -> &Positions {
        &self.leg1_positions
    }

    #[must_use]
    pub fn leg2_positions(&self) -> &Positions {
        &self.leg2_positions
    }

    pub fn leg1_positions_mut(&mut self) -> &mut Positions {
        &mut self.leg1_positions
    }

    pub fn leg2_positions_mut(&mut self) -> &mut Positions {
        &mut self.leg2_positions
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
        assert!(engine.queue(10, 1, Side::Buy, 49_900, 5, Offset::Open, Hedge::Spec));
        let fill = engine.try_execute_best(Side::Buy).unwrap().unwrap();
        assert_eq!(fill.quantity, 5);
        assert_eq!(engine.queue_len(), 0);
        assert_eq!(
            engine
                .leg1_positions()
                .get(PositionKey::new(1, Side::Buy, Hedge::Spec))
                .today,
            5
        );
        assert_eq!(
            engine
                .leg2_positions()
                .get(PositionKey::new(1, Side::Sell, Hedge::Spec))
                .today,
            5
        );
        let events = engine.drain_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].order_id, 10);
        assert_eq!(events[0].side, Side::Buy);
        assert_eq!(events[0].fill.quantity, 5);
    }

    #[test]
    fn closes_both_leg_positions_atomically() {
        use crate::exchange::positions::PositionKey;
        use crate::exchange::protocol::{Hedge, Offset};

        let mut engine = ComboEngine::new(
            ContractSpec::new("LEG1", 1, 1_000, 100_000),
            ContractSpec::new("LEG2", 1, 1, 100_000),
        );
        engine
            .executor_mut()
            .leg1
            .insert_resting_order(2, OrderType::Buy, 50_000, 5, 5, 1)
            .unwrap();
        engine
            .executor_mut()
            .leg2
            .insert_resting_order(3, OrderType::Sell, 100, 5, 5, 2)
            .unwrap();
        let leg1_key = PositionKey::new(1, Side::Buy, Hedge::Spec);
        let leg2_key = PositionKey::new(1, Side::Sell, Hedge::Spec);
        engine.leg1_positions_mut().apply_open(leg1_key, 5).unwrap();
        engine.leg2_positions_mut().apply_open(leg2_key, 5).unwrap();

        assert!(engine.queue(
            10,
            1,
            Side::Sell,
            49_900,
            5,
            Offset::CloseToday,
            Hedge::Spec
        ));
        let fill = engine.try_execute_best(Side::Sell).unwrap().unwrap();
        assert_eq!(fill.quantity, 5);
        assert_eq!(engine.leg1_positions().get(leg1_key).total(), 0);
        assert_eq!(engine.leg2_positions().get(leg2_key).total(), 0);
    }

    #[test]
    fn rejects_close_without_leg_positions() {
        use crate::exchange::protocol::{Hedge, Offset};

        let mut engine = engine();
        assert!(engine.queue(10, 1, Side::Buy, 49_900, 5, Offset::CloseToday, Hedge::Spec));
        let result = engine.try_execute_best(Side::Buy).unwrap();
        assert_eq!(result, Err(ComboExecError::PositionUnavailable));
        assert_eq!(engine.queue_len(), 1);
    }

    #[test]
    fn leaves_order_queued_when_spread_fails() {
        let mut engine = engine();
        assert!(engine.queue(10, 1, Side::Buy, 49_000, 5, Offset::Open, Hedge::Spec));
        let result = engine.try_execute_best(Side::Buy).unwrap();
        assert_eq!(result, Err(ComboExecError::SpreadNotSatisfied));
        assert_eq!(engine.queue_len(), 1);
    }

    #[test]
    fn run_executes_both_sides() {
        let mut engine = engine();
        engine.queue(10, 1, Side::Buy, 49_900, 5, Offset::Open, Hedge::Spec);
        let fills = engine.run();
        assert_eq!(fills.len(), 1);
        assert_eq!(engine.queue_len(), 0);
    }
}
