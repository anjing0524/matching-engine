//! Logical combination-order queue.
//!
//! Combination orders never enter a per-contract order book. They wait in a
//! spread-price/FIFO queue and are later executed atomically against the two
//! leg books. This slice implements the queue and cancellation only.

use crate::exchange::protocol::Side;
use std::collections::{BTreeMap, HashMap, VecDeque};

/// One resting combination order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComboOrder {
    pub order_id: u64,
    pub user_id: u64,
    pub side: Side,
    /// Spread price × tick, e.g. leg1 - leg2.
    pub spread_price_ticks: u64,
    pub quantity: u64,
    pub arrival_seq: u64,
}

/// Price-time queue for one combination definition.
#[derive(Debug, Default)]
pub struct ComboQueue {
    bids: BTreeMap<u64, VecDeque<ComboOrder>>,
    asks: BTreeMap<u64, VecDeque<ComboOrder>>,
    locations: HashMap<u64, (Side, u64)>,
    next_arrival: u64,
}

impl ComboQueue {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues an order, assigning its arrival sequence. Returns `false` for a
    /// duplicate order id or zero quantity.
    pub fn push(
        &mut self,
        order_id: u64,
        user_id: u64,
        side: Side,
        spread_price_ticks: u64,
        quantity: u64,
    ) -> bool {
        if quantity == 0 || self.locations.contains_key(&order_id) {
            return false;
        }
        let arrival_seq = self.next_arrival;
        self.next_arrival += 1;
        let order = ComboOrder {
            order_id,
            user_id,
            side,
            spread_price_ticks,
            quantity,
            arrival_seq,
        };
        let book = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        book.entry(spread_price_ticks).or_default().push_back(order);
        self.locations.insert(order_id, (side, spread_price_ticks));
        true
    }

    /// Cancels an order by id.
    pub fn cancel(&mut self, order_id: u64) -> Option<ComboOrder> {
        let (side, price) = self.locations.remove(&order_id)?;
        let book = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        let queue = book.get_mut(&price)?;
        let index = queue.iter().position(|order| order.order_id == order_id)?;
        let order = queue.remove(index)?;
        if queue.is_empty() {
            book.remove(&price);
        }
        Some(order)
    }

    #[must_use]
    pub fn best_bid(&self) -> Option<u64> {
        self.bids.keys().next_back().copied()
    }

    #[must_use]
    pub fn best_ask(&self) -> Option<u64> {
        self.asks.keys().next().copied()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.locations.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.locations.is_empty()
    }

    /// Returns the best-priority order without removing it.
    #[must_use]
    pub fn peek_best(&self, side: Side) -> Option<ComboOrder> {
        let price = match side {
            Side::Buy => self.best_bid()?,
            Side::Sell => self.best_ask()?,
        };
        let book = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };
        book.get(&price).and_then(|queue| queue.front()).copied()
    }

    /// Pops the best-priority order on one side.
    pub fn pop_best(&mut self, side: Side) -> Option<ComboOrder> {
        let price = match side {
            Side::Buy => self.best_bid()?,
            Side::Sell => self.best_ask()?,
        };
        let book = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        let queue = book.get_mut(&price)?;
        let order = queue.pop_front()?;
        if queue.is_empty() {
            book.remove(&price);
        }
        self.locations.remove(&order.order_id);
        Some(order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_then_time_priority() {
        let mut queue = ComboQueue::new();
        queue.push(1, 1, Side::Buy, 100, 5);
        queue.push(2, 1, Side::Buy, 110, 5);
        queue.push(3, 1, Side::Buy, 110, 5);
        assert_eq!(queue.best_bid(), Some(110));
        assert_eq!(queue.pop_best(Side::Buy).unwrap().order_id, 2);
        assert_eq!(queue.pop_best(Side::Buy).unwrap().order_id, 3);
        assert_eq!(queue.pop_best(Side::Buy).unwrap().order_id, 1);
    }

    #[test]
    fn cancel_removes_from_the_right_price_level() {
        let mut queue = ComboQueue::new();
        queue.push(1, 1, Side::Sell, 90, 5);
        queue.push(2, 1, Side::Sell, 80, 5);
        assert_eq!(queue.cancel(2).unwrap().order_id, 2);
        assert_eq!(queue.best_ask(), Some(90));
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn duplicate_and_zero_quantity_are_rejected() {
        let mut queue = ComboQueue::new();
        assert!(!queue.push(1, 1, Side::Buy, 100, 0));
        assert!(queue.push(1, 1, Side::Buy, 100, 5));
        assert!(!queue.push(1, 1, Side::Buy, 100, 5));
    }
}
