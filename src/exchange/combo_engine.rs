//! Combination queue + two-leg executor.
//!
//! Keeps resting combination orders in the logical spread-price/FIFO queue and
//! attempts atomic execution against the two leg books when the queue reaches a
//! marketable spread.

use crate::domain::orderbook::ContractSpec;
use crate::exchange::combo::ComboQueue;
use crate::exchange::combo_exec::{ComboExecError, ComboExecutor, ComboFill};
use crate::exchange::margin::ComboMarginConfig;
use crate::exchange::positions::{CloseKey, FrozenClose, PositionKey, Positions};
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

/// Why a combination order was rejected before entering the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComboRejectReason {
    UserQuantityLimit,
    MarginInsufficient,
    Leg1CloseTodayUnavailable,
    Leg1CloseYesterdayUnavailable,
    Leg2CloseTodayUnavailable,
    Leg2CloseYesterdayUnavailable,
    InvalidOrder,
}

fn leg1_close_reason(offset: Offset) -> ComboRejectReason {
    match offset {
        Offset::Close => ComboRejectReason::Leg1CloseYesterdayUnavailable,
        Offset::CloseToday | Offset::Open => ComboRejectReason::Leg1CloseTodayUnavailable,
    }
}

fn leg2_close_reason(offset: Offset) -> ComboRejectReason {
    match offset {
        Offset::Close => ComboRejectReason::Leg2CloseYesterdayUnavailable,
        Offset::CloseToday | Offset::Open => ComboRejectReason::Leg2CloseTodayUnavailable,
    }
}

impl ComboRejectReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserQuantityLimit => "combo open quantity limit exceeded",
            Self::MarginInsufficient => "insufficient combo margin",
            Self::Leg1CloseTodayUnavailable => "insufficient closeable leg1 today position",
            Self::Leg1CloseYesterdayUnavailable => "insufficient closeable leg1 yesterday position",
            Self::Leg2CloseTodayUnavailable => "insufficient closeable leg2 today position",
            Self::Leg2CloseYesterdayUnavailable => "insufficient closeable leg2 yesterday position",
            Self::InvalidOrder => "invalid or duplicate combo order",
        }
    }
}

/// Public projection of a combination fill: no member identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComboPublicFill {
    pub seq: u64,
    pub quantity: u64,
    pub leg1_price: u64,
    pub leg2_price: u64,
    pub spread_ticks: i64,
}

/// Private projection of a combination fill: routed back to the member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComboPrivateFill {
    pub seq: u64,
    pub order_id: u64,
    pub user_id: u64,
    pub side: Side,
    pub quantity: u64,
    pub leg1_price: u64,
    pub leg2_price: u64,
    pub spread_ticks: i64,
}

impl ComboFillEvent {
    #[must_use]
    pub fn public_fill(&self) -> ComboPublicFill {
        ComboPublicFill {
            seq: self.seq,
            quantity: self.fill.quantity,
            leg1_price: self.fill.leg1_price,
            leg2_price: self.fill.leg2_price,
            spread_ticks: self.fill.spread_ticks,
        }
    }

    #[must_use]
    pub fn private_fill(&self) -> ComboPrivateFill {
        ComboPrivateFill {
            seq: self.seq,
            order_id: self.order_id,
            user_id: self.user_id,
            side: self.side,
            quantity: self.fill.quantity,
            leg1_price: self.fill.leg1_price,
            leg2_price: self.fill.leg2_price,
            spread_ticks: self.fill.spread_ticks,
        }
    }
}

/// Combines the combo queue with the two-leg executor.
pub struct ComboEngine {
    queue: ComboQueue,
    executor: ComboExecutor,
    next_event_seq: u64,
    events: Vec<ComboFillEvent>,
    leg1_positions: Positions,
    leg2_positions: Positions,
    leg1_frozen: FrozenClose,
    leg2_frozen: FrozenClose,
    close_reservations: std::collections::HashMap<u64, (CloseKey, CloseKey, u64)>,
    max_open_qty_per_user: Option<u64>,
    open_qty_per_user: std::collections::HashMap<u64, u64>,
    margin_config: Option<ComboMarginConfig>,
    margin_balance: std::collections::HashMap<u64, u128>,
    margin_reservations: std::collections::HashMap<u64, u128>,
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
            leg1_frozen: FrozenClose::new(),
            leg2_frozen: FrozenClose::new(),
            close_reservations: std::collections::HashMap::new(),
            max_open_qty_per_user: None,
            open_qty_per_user: std::collections::HashMap::new(),
            margin_config: None,
            margin_balance: std::collections::HashMap::new(),
            margin_reservations: std::collections::HashMap::new(),
        }
    }

    #[must_use]
    pub fn with_margin_config(mut self, config: ComboMarginConfig) -> Self {
        self.margin_config = Some(config);
        self
    }

    pub fn set_margin_balance(&mut self, user_id: u64, amount: u128) {
        self.margin_balance.insert(user_id, amount);
    }

    #[must_use]
    pub fn available_margin(&self, user_id: u64) -> u128 {
        let balance = self.margin_balance.get(&user_id).copied().unwrap_or(0);
        let reserved = self.margin_reservations.values().sum::<u128>();
        balance.saturating_sub(reserved)
    }

    #[must_use]
    pub fn reserved_margin(&self) -> u128 {
        self.margin_reservations.values().sum()
    }

    fn reserve_margin(&mut self, order_id: u64, user_id: u64, spread: u64, qty: u64) -> bool {
        let Some(config) = self.margin_config else {
            return true;
        };
        let required = config.required(spread, qty);
        if required > self.available_margin(user_id) {
            return false;
        }
        self.margin_reservations.insert(order_id, required);
        true
    }

    fn release_margin(&mut self, order_id: u64) {
        self.margin_reservations.remove(&order_id);
    }

    #[allow(clippy::too_many_arguments)]
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
        self.queue_checked(order_id, user_id, side, spread, qty, offset, hedge)
            .is_ok()
    }

    /// Queues a combo order and returns a precise rejection reason.
    #[allow(clippy::too_many_arguments)]
    pub fn queue_checked(
        &mut self,
        order_id: u64,
        user_id: u64,
        side: Side,
        spread: u64,
        qty: u64,
        offset: Offset,
        hedge: Hedge,
    ) -> Result<(), ComboRejectReason> {
        if self.exceeds_limit(user_id, qty) {
            return Err(ComboRejectReason::UserQuantityLimit);
        }
        if !self.reserve_margin(order_id, user_id, spread, qty) {
            return Err(ComboRejectReason::MarginInsufficient);
        }
        if offset == Offset::Open {
            let accepted = self
                .queue
                .push(order_id, user_id, side, spread, qty, offset, hedge);
            if accepted {
                self.add_open_qty(user_id, qty);
                return Ok(());
            }
            self.release_margin(order_id);
            return Err(ComboRejectReason::InvalidOrder);
        }
        let (leg1_position, leg2_position) = match side {
            Side::Buy => (Side::Sell, Side::Buy),
            Side::Sell => (Side::Buy, Side::Sell),
        };
        let leg1_key = CloseKey::new(PositionKey::new(user_id, leg1_position, hedge), offset);
        let leg2_key = CloseKey::new(PositionKey::new(user_id, leg2_position, hedge), offset);
        if self.leg1_frozen.available(leg1_key, &self.leg1_positions) < qty {
            self.release_margin(order_id);
            return Err(leg1_close_reason(offset));
        }
        if self
            .leg1_frozen
            .reserve(leg1_key, &self.leg1_positions, qty)
            .is_err()
        {
            self.release_margin(order_id);
            return Err(leg1_close_reason(offset));
        }
        if self.leg2_frozen.available(leg2_key, &self.leg2_positions) < qty {
            self.leg1_frozen.release(leg1_key, qty);
            self.release_margin(order_id);
            return Err(leg2_close_reason(offset));
        }
        if self
            .leg2_frozen
            .reserve(leg2_key, &self.leg2_positions, qty)
            .is_err()
        {
            self.leg1_frozen.release(leg1_key, qty);
            self.release_margin(order_id);
            return Err(leg2_close_reason(offset));
        }
        if !self
            .queue
            .push(order_id, user_id, side, spread, qty, offset, hedge)
        {
            self.leg1_frozen.release(leg1_key, qty);
            self.leg2_frozen.release(leg2_key, qty);
            self.release_margin(order_id);
            return Err(ComboRejectReason::InvalidOrder);
        }
        self.close_reservations
            .insert(order_id, (leg1_key, leg2_key, qty));
        self.add_open_qty(user_id, qty);
        Ok(())
    }

    /// Cancels a queued combo order and releases any close reservation.
    pub fn cancel(&mut self, order_id: u64) -> bool {
        let reservation = self.close_reservations.remove(&order_id);
        let Some(cancelled) = self.queue.cancel(order_id) else {
            if let Some(reservation) = reservation {
                self.close_reservations.insert(order_id, reservation);
            }
            return false;
        };
        if let Some((leg1, leg2, qty)) = reservation {
            self.leg1_frozen.release(leg1, qty);
            self.leg2_frozen.release(leg2, qty);
        }
        self.remove_open_qty(cancelled.user_id, cancelled.quantity);
        self.release_margin(order_id);
        true
    }

    /// Attempts to execute the best resting order on `side`.
    ///
    /// The order stays queued if the leg books cannot fill it at its spread.
    pub fn try_execute_best(&mut self, side: Side) -> Option<Result<ComboFill, ComboExecError>> {
        let order = self.queue.peek_best(side)?;
        if order.offset != Offset::Open
            && !self
                .close_reservations
                .get(&order.order_id)
                .is_some_and(|(_, _, qty)| *qty >= order.quantity)
        {
            return Some(Err(ComboExecError::PositionUnavailable));
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
                self.remove_open_qty(order.user_id, order.quantity);
                self.release_margin(order.order_id);
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
                if let Some((leg1, leg2, qty)) = self.close_reservations.remove(&order.order_id) {
                    self.leg1_frozen.release(leg1, qty);
                    self.leg2_frozen.release(leg2, qty);
                }
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
    pub fn with_max_open_qty_per_user(mut self, max_qty: u64) -> Self {
        self.max_open_qty_per_user = Some(max_qty);
        self
    }

    fn exceeds_limit(&self, user_id: u64, qty: u64) -> bool {
        self.max_open_qty_per_user.is_some_and(|max_qty| {
            self.open_qty_per_user
                .get(&user_id)
                .copied()
                .unwrap_or(0)
                .saturating_add(qty)
                > max_qty
        })
    }

    fn add_open_qty(&mut self, user_id: u64, qty: u64) {
        *self.open_qty_per_user.entry(user_id).or_insert(0) += qty;
    }

    fn remove_open_qty(&mut self, user_id: u64, qty: u64) {
        if let Some(open) = self.open_qty_per_user.get_mut(&user_id) {
            *open = open.saturating_sub(qty);
        }
    }

    #[must_use]
    pub fn open_qty(&self, user_id: u64) -> u64 {
        self.open_qty_per_user.get(&user_id).copied().unwrap_or(0)
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
        let public = events[0].public_fill();
        assert_eq!(public.quantity, 5);
        assert_eq!(public.leg1_price, 50_000);
        let private = events[0].private_fill();
        assert_eq!(private.user_id, 1);
        assert_eq!(private.spread_ticks, 49_900);
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
    fn queue_checked_reports_reason() {
        use crate::exchange::margin::ComboMarginConfig;
        use crate::exchange::protocol::{Hedge, Offset};

        let mut engine = ComboEngine::new(
            ContractSpec::new("LEG1", 1, 1_000, 100_000),
            ContractSpec::new("LEG2", 1, 1, 100_000),
        )
        .with_max_open_qty_per_user(1)
        .with_margin_config(ComboMarginConfig::new(10, 1_000));
        engine.set_margin_balance(1, 100_000);
        assert_eq!(
            engine.queue_checked(1, 1, Side::Buy, 1_000, 2, Offset::Open, Hedge::Spec),
            Err(ComboRejectReason::UserQuantityLimit)
        );
        assert_eq!(
            engine.queue_checked(1, 1, Side::Buy, 1_000, 1, Offset::CloseToday, Hedge::Spec),
            Err(ComboRejectReason::Leg1CloseTodayUnavailable)
        );
    }

    #[test]
    fn close_reason_distinguishes_leg_and_day() {
        use crate::exchange::positions::PositionKey;
        use crate::exchange::protocol::{Hedge, Offset};

        let mut engine = ComboEngine::new(
            ContractSpec::new("LEG1", 1, 1_000, 100_000),
            ContractSpec::new("LEG2", 1, 1, 100_000),
        );
        assert_eq!(
            engine.queue_checked(1, 1, Side::Buy, 1_000, 1, Offset::Close, Hedge::Spec),
            Err(ComboRejectReason::Leg1CloseYesterdayUnavailable)
        );
        engine
            .leg1_positions_mut()
            .apply_open(PositionKey::new(1, Side::Sell, Hedge::Spec), 5)
            .unwrap();
        assert_eq!(
            engine.queue_checked(2, 1, Side::Buy, 1_000, 1, Offset::CloseToday, Hedge::Spec),
            Err(ComboRejectReason::Leg2CloseTodayUnavailable)
        );
    }

    #[test]
    fn margin_check_rejects_and_releases() {
        use crate::exchange::margin::ComboMarginConfig;
        use crate::exchange::protocol::{Hedge, Offset};

        let mut engine = ComboEngine::new(
            ContractSpec::new("LEG1", 1, 1_000, 100_000),
            ContractSpec::new("LEG2", 1, 1, 100_000),
        )
        .with_margin_config(ComboMarginConfig::new(10, 1_000));
        engine.set_margin_balance(1, 4_999);
        assert!(!engine.queue(10, 1, Side::Buy, 1_000, 5, Offset::Open, Hedge::Spec));
        engine.set_margin_balance(1, 5_000);
        assert!(engine.queue(10, 1, Side::Buy, 1_000, 5, Offset::Open, Hedge::Spec));
        assert_eq!(engine.reserved_margin(), 5_000);
        assert!(engine.cancel(10));
        assert_eq!(engine.reserved_margin(), 0);
        assert_eq!(engine.available_margin(1), 5_000);
    }

    #[test]
    fn combo_limit_rejects_excess_open_quantity() {
        use crate::exchange::protocol::{Hedge, Offset};

        let mut engine = ComboEngine::new(
            ContractSpec::new("LEG1", 1, 1_000, 100_000),
            ContractSpec::new("LEG2", 1, 1, 100_000),
        )
        .with_max_open_qty_per_user(10);
        assert!(engine.queue(10, 1, Side::Buy, 49_900, 6, Offset::Open, Hedge::Spec));
        assert!(!engine.queue(11, 1, Side::Buy, 49_900, 5, Offset::Open, Hedge::Spec));
        assert_eq!(engine.open_qty(1), 6);
        assert!(engine.cancel(10));
        assert_eq!(engine.open_qty(1), 0);
        assert!(engine.queue(12, 1, Side::Buy, 49_900, 5, Offset::Open, Hedge::Spec));
    }

    #[test]
    fn frozen_close_prevents_double_reservation() {
        use crate::exchange::positions::PositionKey;
        use crate::exchange::protocol::{Hedge, Offset};

        let mut engine = ComboEngine::new(
            ContractSpec::new("LEG1", 1, 1_000, 100_000),
            ContractSpec::new("LEG2", 1, 1, 100_000),
        );
        engine
            .leg1_positions_mut()
            .apply_open(PositionKey::new(1, Side::Buy, Hedge::Spec), 5)
            .unwrap();
        engine
            .leg2_positions_mut()
            .apply_open(PositionKey::new(1, Side::Sell, Hedge::Spec), 5)
            .unwrap();
        assert!(engine.queue(
            10,
            1,
            Side::Sell,
            49_900,
            5,
            Offset::CloseToday,
            Hedge::Spec
        ));
        assert!(!engine.queue(
            11,
            1,
            Side::Sell,
            49_900,
            5,
            Offset::CloseToday,
            Hedge::Spec
        ));
        assert!(engine.cancel(10));
        assert!(engine.queue(
            12,
            1,
            Side::Sell,
            49_900,
            5,
            Offset::CloseToday,
            Hedge::Spec
        ));
    }

    #[test]
    fn rejects_close_without_leg_positions() {
        use crate::exchange::protocol::{Hedge, Offset};

        let mut engine = engine();
        assert!(!engine.queue(10, 1, Side::Buy, 49_900, 5, Offset::CloseToday, Hedge::Spec));
        assert_eq!(engine.try_execute_best(Side::Buy), None);
        assert_eq!(engine.queue_len(), 0);
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
