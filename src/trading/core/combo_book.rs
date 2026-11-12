//! Base Combo Order book and direct matching for one Combo Pair.
//!
//! M1 rests only directly submitted base combo orders. Derived implied orders
//! are matching candidates and have no insertion path here, so the public combo
//! book never contains them. The book is keyed by spread price and time; spread
//! prices are signed because a combo quote may be negative.
//!
//! A submitted Combo Order acts as a taker against the opposite side in
//! price-time priority. Each execution is atomic: one equal quantity leaves the
//! buy and the sell Combo Order together, and neither side can fill alone. A
//! residual taker quantity rests. The book also tracks the last Combo trade
//! price, whether direct or implied-in, that the SHFE-style execution-price
//! rule consumes.
//!
//! The book has no public market-data projection. It stores the owner and
//! session metadata later tickets need for cancel, fills, and private replay.

use crate::shared::collections::U64MixMap;
use crate::trading::core::combo_margin::ComboMarginError;
use crate::trading::core::protocol::{
    Command, CommandType, Offset, PriceType, Side, TimeCondition, TriggerType, VolumeCondition,
};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::error::Error;
use std::fmt::{Display, Formatter, Result as FmtResult};

/// Owner and client identity preserved with a resting Combo Order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ComboOrderOwner {
    /// Owning user account.
    pub user_id: u64,
    /// Gateway session that submitted the order.
    pub session_id: u64,
    /// Client-assigned order id, preserved for private fill reporting.
    pub client_order_id: u64,
}

impl ComboOrderOwner {
    /// Creates the owner metadata stored with one Combo Order.
    #[must_use]
    pub const fn new(user_id: u64, session_id: u64, client_order_id: u64) -> Self {
        Self {
            user_id,
            session_id,
            client_order_id,
        }
    }
}

/// One atomic direct Combo Fill.
///
/// Both sides carry the same quantity. The resting order and the incoming
/// taker order never produce a single-sided execution: a direct combo trade
/// always consumes a buy Combo Order and a sell Combo Order together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ComboFill {
    /// Combo Pair this execution belongs to.
    pub pair_id: u32,
    /// Execution spread price, inside both limits.
    pub spread_price: i64,
    /// Equal quantity executed on the buy and sell Combo Orders.
    pub quantity: u64,
    /// Exchange id of the buy-side Combo Order.
    pub buy_order_id: u64,
    /// Exchange id of the sell-side Combo Order.
    pub sell_order_id: u64,
    /// Owner of the buy-side Combo Order.
    pub buy_owner: ComboOrderOwner,
    /// Owner of the sell-side Combo Order.
    pub sell_owner: ComboOrderOwner,
}

/// One atomic Combo Fill produced by implied-in matching.
///
/// An implied-in execution has only one base Combo Order: the taker that
/// consumes two resting leg orders. The realized spread is the first-leg price
/// minus the second-leg price, and both leg prices are the resting prices that
/// the taker consumed. No implied order is stored or published.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImpliedComboFill {
    /// Combo Pair this execution belongs to.
    pub pair_id: u32,
    /// Exchange id of the implied-in taker Combo Order.
    pub order_id: u64,
    /// Owner of the taker Combo Order.
    pub owner: ComboOrderOwner,
    /// Direction of the taker Combo Order.
    pub side: Side,
    /// Realized spread: first-leg price minus second-leg price.
    pub spread_price: i64,
    /// Equal quantity consumed from both leg books.
    pub quantity: u64,
    /// Resting first-leg price consumed by the taker.
    pub leg1_price_ticks: u64,
    /// Resting second-leg price consumed by the taker.
    pub leg2_price_ticks: u64,
}

/// One private Combo Fill delivered to a single Combo Order owner.
///
/// Every atomic Combo Fill produces one private fill per participating Combo
/// Order owner: two for direct matching and one for an implied-in taker. The
/// Combo Pair itself has no position; positions live on the two ordinary leg
/// Contracts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ComboPrivateFill {
    /// Registry-owned private sequence; each user's subsequence stays ordered.
    pub seq: u64,
    /// Combo Pair this execution belongs to.
    pub pair_id: u32,
    /// Exchange id of the owner's Combo Order.
    pub order_id: u64,
    /// Client-assigned id preserved from the owner's Combo Order.
    pub client_order_id: u64,
    /// Owning user account.
    pub user_id: u64,
    /// Gateway session that submitted the Combo Order.
    pub session_id: u64,
    /// Direction of the owner's Combo Order.
    pub side: Side,
    /// Quantity filled on this Combo Order.
    pub quantity: u64,
    /// Realized spread price of the execution.
    pub spread_price: i64,
    /// First-leg price used to value this execution.
    ///
    /// Implied-in fills report the resting first-leg prices consumed. Direct
    /// fills consume no leg liquidity, so this is the best crossing first-leg
    /// reference price at execution time (prior settlement when no quote
    /// exists).
    pub leg1_price_ticks: u64,
    /// Second-leg price used to value this execution.
    ///
    /// Implied-in fills report the resting second-leg prices consumed. Direct
    /// fills consume no leg liquidity, so this is the best crossing second-leg
    /// reference price at execution time (prior settlement when no quote
    /// exists).
    pub leg2_price_ticks: u64,
}

/// One resting base Combo Order.
///
/// The exchange-assigned `order_id` is distinct from
/// [`ComboOrderOwner::client_order_id`]. `spread_price` is signed; `remaining`
/// starts at `quantity` and is reduced only by future matching.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ComboOrder {
    /// Exchange-assigned combo order id.
    pub order_id: u64,
    /// Combo Pair this order trades.
    pub pair_id: u32,
    /// Owner/session/client identity.
    pub owner: ComboOrderOwner,
    /// Buy or sell the spread.
    pub side: Side,
    /// Limit spread price; may be negative.
    pub spread_price: i64,
    /// Original order quantity.
    pub quantity: u64,
    /// Quantity still resting.
    pub remaining: u64,
    /// Time-priority sequence within this book. Assigned by [`ComboBook::rest`].
    pub arrival_seq: u64,
}

impl ComboOrder {
    /// Creates a base Combo Order before the book assigns its arrival sequence.
    #[must_use]
    pub const fn new(
        order_id: u64,
        pair_id: u32,
        owner: ComboOrderOwner,
        side: Side,
        spread_price: i64,
        quantity: u64,
    ) -> Self {
        Self {
            order_id,
            pair_id,
            owner,
            side,
            spread_price,
            quantity,
            remaining: quantity,
            arrival_seq: 0,
        }
    }
}

/// Serializable base Combo Order book state.
///
/// Orders are stored in price-time priority order per side, so restoring the
/// vectors rebuilds the same FIFO queues. `locations` is derived and therefore
/// not stored, while the arrival sequence counter and the last Combo trade price
/// are, so a restored book keeps both time priority and the direct execution
/// price anchor.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ComboBookSnapshot {
    /// Combo Pair whose book this state belongs to.
    pub pair_id: u32,
    /// Resting bid orders in price order, FIFO within a price.
    pub bids: Vec<ComboOrder>,
    /// Resting ask orders in price order, FIFO within a price.
    pub asks: Vec<ComboOrder>,
    /// Arrival sequence the next resting order will receive.
    pub next_arrival_seq: u64,
    /// Spread price of the last direct or implied-in Combo Fill.
    pub last_trade_price: Option<i64>,
}

/// Why a base Combo Order could not rest in or leave a [`ComboBook`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ComboBookError {
    /// An order with this exchange id is already resting.
    DuplicateOrderId {
        /// Duplicate exchange order id.
        order_id: u64,
    },
    /// Exchange-assigned Combo Order id zero is reserved.
    ZeroOrderId {
        /// Rejected exchange order id.
        order_id: u64,
    },
    /// Two snapshot orders carry the same arrival sequence.
    DuplicateArrivalSeq {
        /// Exchange order id carrying the duplicate sequence.
        order_id: u64,
        /// Duplicate arrival sequence.
        arrival_seq: u64,
    },
    /// A restored order sits on the wrong side list.
    SideMismatch {
        /// Exchange order id on the wrong list.
        order_id: u64,
        /// Side implied by the snapshot list.
        expected: Side,
        /// Side carried by the order.
        actual: Side,
    },
    /// A restored book would hand out an arrival sequence that is already used.
    InvalidNextArrivalSeq {
        /// Stored next arrival sequence.
        next_arrival_seq: u64,
        /// Highest arrival sequence among the restored orders.
        max_arrival_seq: u64,
    },
    /// A restored book has a bid at or above its best ask.
    CrossedBook {
        /// Best bid spread price in the snapshot.
        best_bid: i64,
        /// Best ask spread price in the snapshot.
        best_ask: i64,
    },
    /// A base Combo Order must carry a positive quantity.
    ZeroQuantity {
        /// Rejected exchange order id.
        order_id: u64,
    },
    /// The remaining quantity is inconsistent with the original quantity.
    InvalidRemaining {
        /// Rejected exchange order id.
        order_id: u64,
        /// Original order quantity.
        quantity: u64,
        /// Rejected remaining quantity.
        remaining: u64,
    },
    /// The order names a different pair than the book that would own it.
    PairMismatch {
        /// Pair id owned by the book.
        book_pair_id: u32,
        /// Pair id carried by the order.
        order_pair_id: u32,
    },
    /// Cancel named an order that is not resting in this book.
    UnknownOrder {
        /// Exchange order id that was not found.
        order_id: u64,
    },
    /// Cancel was attempted by a user that does not own the order.
    NotOrderOwner {
        /// Exchange order id being cancelled.
        order_id: u64,
        /// User id that attempted the cancel.
        user_id: u64,
    },
}

impl Display for ComboBookError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::DuplicateOrderId { order_id } => {
                write!(formatter, "combo order {order_id} is already resting")
            }
            Self::ZeroOrderId { order_id } => {
                write!(formatter, "combo order id {order_id} is reserved")
            }
            Self::DuplicateArrivalSeq {
                order_id,
                arrival_seq,
            } => write!(
                formatter,
                "combo order {order_id} reuses arrival sequence {arrival_seq}"
            ),
            Self::SideMismatch {
                order_id,
                expected,
                actual,
            } => write!(
                formatter,
                "combo order {order_id} carries side {actual:?} on the {expected:?} snapshot list"
            ),
            Self::InvalidNextArrivalSeq {
                next_arrival_seq,
                max_arrival_seq,
            } => write!(
                formatter,
                "combo book next arrival sequence {next_arrival_seq} does not advance past {max_arrival_seq}"
            ),
            Self::CrossedBook { best_bid, best_ask } => write!(
                formatter,
                "combo book snapshot is crossed: bid {best_bid} at or above ask {best_ask}"
            ),
            Self::ZeroQuantity { order_id } => {
                write!(formatter, "combo order {order_id} has zero quantity")
            }
            Self::InvalidRemaining {
                order_id,
                quantity,
                remaining,
            } => write!(
                formatter,
                "combo order {order_id} has remaining {remaining} outside 1..={quantity}"
            ),
            Self::PairMismatch {
                book_pair_id,
                order_pair_id,
            } => write!(
                formatter,
                "combo order for pair {order_pair_id} cannot rest in pair {book_pair_id} book"
            ),
            Self::UnknownOrder { order_id } => {
                write!(formatter, "combo order {order_id} is not resting")
            }
            Self::NotOrderOwner { order_id, user_id } => {
                write!(
                    formatter,
                    "user {user_id} does not own combo order {order_id}"
                )
            }
        }
    }
}

impl Error for ComboBookError {}

/// Price-time priority book for base Combo Orders of one Combo Pair.
///
/// Bids sort highest spread first and asks sort lowest spread first. Orders at
/// the same spread price leave in arrival order.
#[derive(Clone, Debug)]
pub struct ComboBook {
    pair_id: u32,
    bids: BTreeMap<i64, VecDeque<ComboOrder>>,
    asks: BTreeMap<i64, VecDeque<ComboOrder>>,
    locations: U64MixMap<(Side, i64)>,
    next_arrival_seq: u64,
    last_trade_price: Option<i64>,
}

impl ComboBook {
    /// Creates an empty book for `pair_id`.
    #[must_use]
    pub fn new(pair_id: u32) -> Self {
        Self {
            pair_id,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            locations: U64MixMap::default(),
            next_arrival_seq: 0,
            last_trade_price: None,
        }
    }

    /// Returns the Combo Pair this book belongs to.
    #[must_use]
    pub fn pair_id(&self) -> u32 {
        self.pair_id
    }

    /// Adds one base Combo Order, assigning its arrival sequence.
    ///
    /// # Errors
    /// Returns [`ComboBookError`] for a duplicate id, zero quantity, an invalid
    /// remaining quantity, or an order built for another pair. No order rests
    /// in that case.
    pub fn rest(&mut self, order: ComboOrder) -> Result<ComboOrder, ComboBookError> {
        self.validate_order(&order)?;
        Ok(self.insert(order))
    }

    /// Submits one base Combo Order, matching it as a taker before resting.
    ///
    /// The incoming order walks the opposite side in price-time priority while
    /// its limit crosses. Every execution is one atomic [`ComboFill`] with the
    /// same quantity on the buy and sell Combo Orders. A partially filled
    /// incoming order rests with its residual quantity.
    ///
    /// # Errors
    /// Returns [`ComboBookError`] for a duplicate id, zero quantity, an invalid
    /// remaining quantity, or an order built for another pair. No book mutates
    /// in that case.
    pub fn submit(&mut self, mut order: ComboOrder) -> Result<Vec<ComboEvent>, ComboBookError> {
        self.validate_order(&order)?;
        let mut events = self.match_taker_unchecked(&mut order, false);
        if order.remaining > 0 {
            events.push(ComboEvent::Rested {
                order: self.insert(order),
            });
        }
        Ok(events)
    }

    /// Matches `order` as a taker against the opposite side without resting it.
    ///
    /// The order's `remaining` is reduced by every atomic direct fill. The
    /// caller decides whether the residual rests. This split lets the registry
    /// run implied-in matching between direct matching and resting, while the
    /// public combo book still holds only base Combo Orders.
    ///
    /// # Errors
    /// Returns [`ComboBookError`] for a duplicate id, zero quantity, an invalid
    /// remaining quantity, or an order built for another pair. No book mutates
    /// in that case.
    pub fn match_taker(
        &mut self,
        order: &mut ComboOrder,
        market: bool,
    ) -> Result<Vec<ComboEvent>, ComboBookError> {
        self.validate_order(order)?;
        Ok(self.match_taker_unchecked(order, market))
    }

    fn match_taker_unchecked(&mut self, order: &mut ComboOrder, market: bool) -> Vec<ComboEvent> {
        let mut events = Vec::new();
        let resting_side = order.side.opposite();
        while order.remaining > 0 {
            let Some(resting) = self.best_order(resting_side).copied() else {
                break;
            };
            if !market && !crosses(order.side, order.spread_price, resting.spread_price) {
                break;
            }
            let quantity = order.remaining.min(resting.remaining);
            let spread_price = if market {
                resting.spread_price
            } else {
                self.execution_price(order.side, order.spread_price, resting.spread_price)
            };
            let _consumed = self
                .consume_resting(resting.order_id, quantity)
                .expect("resting combo order selected from the book must consume");
            order.remaining -= quantity;
            self.last_trade_price = Some(spread_price);
            let (buy_order_id, buy_owner, sell_order_id, sell_owner) = match order.side {
                Side::Buy => (order.order_id, order.owner, resting.order_id, resting.owner),
                Side::Sell => (resting.order_id, resting.owner, order.order_id, order.owner),
            };
            events.push(ComboEvent::Filled {
                fill: ComboFill {
                    pair_id: self.pair_id,
                    spread_price,
                    quantity,
                    buy_order_id,
                    sell_order_id,
                    buy_owner,
                    sell_owner,
                },
            });
        }
        events
    }

    /// Applies the quantity an implied-in execution consumed from a resting
    /// order, returning the reduced order or `None` when it is fully consumed.
    ///
    /// The registry uses this after the shared leg books have accepted an
    /// atomic implied-in fill for an already-resting Combo Order, so a resting
    /// order can act as a taker when leg liquidity appears later.
    ///
    /// # Errors
    /// Returns [`ComboBookError::UnknownOrder`] when `order_id` is not resting.
    pub fn apply_implied_fill(
        &mut self,
        order_id: u64,
        quantity: u64,
    ) -> Result<Option<ComboOrder>, ComboBookError> {
        self.consume_resting(order_id, quantity)
    }

    /// Returns resting base orders in combined execution priority order.
    ///
    /// Bids come first, highest spread first, then asks, lowest spread first;
    /// orders at the same spread keep arrival order. A resting Combo Order
    /// re-evaluated against leg liquidity walks this list, so the same
    /// price-time rule applies to both direct and implied-in execution.
    #[must_use]
    pub fn orders_by_priority(&self) -> Vec<ComboOrder> {
        let mut orders = Vec::with_capacity(self.locations.len());
        for queue in self.bids.values().rev() {
            orders.extend(queue.iter().copied());
        }
        for queue in self.asks.values() {
            orders.extend(queue.iter().copied());
        }
        orders
    }

    /// Records the realized spread of an implied-in Combo Fill.
    pub(crate) fn record_trade_price(&mut self, spread_price: i64) {
        self.last_trade_price = Some(spread_price);
    }

    /// Returns the spread price of the last Combo Fill on this pair.
    #[must_use]
    pub fn last_trade_price(&self) -> Option<i64> {
        self.last_trade_price
    }

    /// Captures resting base orders in price-time priority order.
    ///
    /// Orders are emitted in price order, FIFO within each price, so restoring
    /// the snapshot rebuilds identical queues. The derived order-id location
    /// index is not stored.
    #[must_use]
    pub fn snapshot(&self) -> ComboBookSnapshot {
        ComboBookSnapshot {
            pair_id: self.pair_id,
            bids: self
                .bids
                .values()
                .flat_map(|queue| queue.iter().copied())
                .collect(),
            asks: self
                .asks
                .values()
                .flat_map(|queue| queue.iter().copied())
                .collect(),
            next_arrival_seq: self.next_arrival_seq,
            last_trade_price: self.last_trade_price,
        }
    }

    /// Replaces this book with a captured snapshot.
    ///
    /// Restoring is all-or-none: every order is validated against the snapshot
    /// pair and arrival sequence before any state changes. A book that is not
    /// empty is replaced rather than merged.
    ///
    /// # Errors
    /// Returns [`ComboBookError`] for a pair mismatch, a zero order id, an
    /// invalid remaining quantity, a duplicate order id or arrival sequence, a
    /// side-list mismatch, or a next arrival sequence that would collide.
    pub fn restore_snapshot(&mut self, snapshot: ComboBookSnapshot) -> Result<(), ComboBookError> {
        let mut bids = BTreeMap::new();
        let mut asks = BTreeMap::new();
        let mut locations = U64MixMap::default();
        let mut arrivals = HashSet::new();
        let mut max_arrival_seq = None;
        for (expected_side, orders) in [(Side::Buy, snapshot.bids), (Side::Sell, snapshot.asks)] {
            let levels = match expected_side {
                Side::Buy => &mut bids,
                Side::Sell => &mut asks,
            };
            for order in orders {
                validate_restored_order(&order, snapshot.pair_id, expected_side, &locations)?;
                if !arrivals.insert(order.arrival_seq) {
                    return Err(ComboBookError::DuplicateArrivalSeq {
                        order_id: order.order_id,
                        arrival_seq: order.arrival_seq,
                    });
                }
                max_arrival_seq = Some(
                    max_arrival_seq
                        .map_or(order.arrival_seq, |max: u64| max.max(order.arrival_seq)),
                );
                levels
                    .entry(order.spread_price)
                    .or_insert_with(VecDeque::new)
                    .push_back(order);
                locations.insert(order.order_id, (expected_side, order.spread_price));
            }
        }
        if let Some(max_arrival_seq) = max_arrival_seq
            && snapshot.next_arrival_seq <= max_arrival_seq
        {
            return Err(ComboBookError::InvalidNextArrivalSeq {
                next_arrival_seq: snapshot.next_arrival_seq,
                max_arrival_seq,
            });
        }
        if let Some(&best_bid) = bids.last_key_value().map(|(price, _)| price)
            && let Some(&best_ask) = asks.first_key_value().map(|(price, _)| price)
            && best_bid >= best_ask
        {
            return Err(ComboBookError::CrossedBook { best_bid, best_ask });
        }
        self.bids = bids;
        self.asks = asks;
        self.locations = locations;
        self.next_arrival_seq = snapshot.next_arrival_seq;
        self.last_trade_price = snapshot.last_trade_price;
        Ok(())
    }

    fn validate_order(&self, order: &ComboOrder) -> Result<(), ComboBookError> {
        if order.order_id == 0 {
            return Err(ComboBookError::ZeroOrderId {
                order_id: order.order_id,
            });
        }
        if order.pair_id != self.pair_id {
            return Err(ComboBookError::PairMismatch {
                book_pair_id: self.pair_id,
                order_pair_id: order.pair_id,
            });
        }
        if order.quantity == 0 {
            return Err(ComboBookError::ZeroQuantity {
                order_id: order.order_id,
            });
        }
        if order.remaining == 0 || order.remaining > order.quantity {
            return Err(ComboBookError::InvalidRemaining {
                order_id: order.order_id,
                quantity: order.quantity,
                remaining: order.remaining,
            });
        }
        if self.locations.contains_key(&order.order_id) {
            return Err(ComboBookError::DuplicateOrderId {
                order_id: order.order_id,
            });
        }
        Ok(())
    }

    fn insert(&mut self, mut order: ComboOrder) -> ComboOrder {
        order.arrival_seq = self.next_arrival_seq;
        self.next_arrival_seq += 1;
        self.levels_mut(order.side)
            .entry(order.spread_price)
            .or_default()
            .push_back(order);
        self.locations
            .insert(order.order_id, (order.side, order.spread_price));
        order
    }

    /// Reduces a resting order by `quantity`, removing it when exhausted.
    ///
    /// Returns the reduced order, or `None` when the order left the book.
    fn consume_resting(
        &mut self,
        order_id: u64,
        quantity: u64,
    ) -> Result<Option<ComboOrder>, ComboBookError> {
        let Some(&(side, spread_price)) = self.locations.get(&order_id) else {
            return Err(ComboBookError::UnknownOrder { order_id });
        };
        let (remaining, level_is_empty) = {
            let levels = self.levels_mut(side);
            let Some(queue) = levels.get_mut(&spread_price) else {
                return Err(ComboBookError::UnknownOrder { order_id });
            };
            let Some(index) = queue.iter().position(|order| order.order_id == order_id) else {
                return Err(ComboBookError::UnknownOrder { order_id });
            };
            let Some(resting) = queue.get_mut(index) else {
                return Err(ComboBookError::UnknownOrder { order_id });
            };
            debug_assert!(quantity <= resting.remaining);
            resting.remaining -= quantity;
            if resting.remaining == 0 {
                queue.remove(index);
                (None, queue.is_empty())
            } else {
                (Some(*resting), false)
            }
        };
        if level_is_empty {
            self.levels_mut(side).remove(&spread_price);
        }
        if remaining.is_none() {
            self.locations.remove(&order_id);
        }
        Ok(remaining)
    }

    /// Returns the direct trade price for a crossed pair of Combo Orders.
    ///
    /// The exchange rule takes the median of the buy limit, sell limit, and
    /// last Combo trade price. With no previous trade it uses the midpoint of
    /// the two limits, rounded toward the resting order's limit: down for a buy
    /// taker and up for a sell taker. Both limits are tick counts, so the
    /// rounded midpoint is a deterministic, valid spread tick.
    fn execution_price(&self, taker_side: Side, taker_limit: i64, resting_limit: i64) -> i64 {
        let (buy_limit, sell_limit) = match taker_side {
            Side::Buy => (taker_limit, resting_limit),
            Side::Sell => (resting_limit, taker_limit),
        };
        if let Some(last) = self.last_trade_price {
            return last.clamp(sell_limit, buy_limit);
        }
        let sum = i128::from(buy_limit) + i128::from(sell_limit);
        let floor = sum.div_euclid(2);
        let midpoint = match taker_side {
            Side::Buy => floor,
            Side::Sell => floor + sum.rem_euclid(2),
        };
        i64::try_from(midpoint).unwrap_or(buy_limit)
    }

    /// Removes and returns a resting order owned by `user_id`.
    ///
    /// # Errors
    /// Returns [`ComboBookError::UnknownOrder`] when the order is not resting,
    /// or [`ComboBookError::NotOrderOwner`] when another user owns it.
    pub fn cancel_for(
        &mut self,
        user_id: u64,
        order_id: u64,
    ) -> Result<ComboOrder, ComboBookError> {
        let owner = self
            .order(order_id)
            .map(|order| order.owner.user_id)
            .ok_or(ComboBookError::UnknownOrder { order_id })?;
        if owner != user_id {
            return Err(ComboBookError::NotOrderOwner { order_id, user_id });
        }
        self.remove_order(order_id)
    }

    fn remove_order(&mut self, order_id: u64) -> Result<ComboOrder, ComboBookError> {
        let (side, spread_price) = self
            .locations
            .get(&order_id)
            .copied()
            .ok_or(ComboBookError::UnknownOrder { order_id })?;
        let levels = self.levels_mut(side);
        let queue = levels
            .get_mut(&spread_price)
            .ok_or(ComboBookError::UnknownOrder { order_id })?;
        let Some(index) = queue.iter().position(|order| order.order_id == order_id) else {
            return Err(ComboBookError::UnknownOrder { order_id });
        };
        let Some(order) = queue.remove(index) else {
            return Err(ComboBookError::UnknownOrder { order_id });
        };
        if queue.is_empty() {
            levels.remove(&spread_price);
        }
        self.locations.remove(&order_id);
        Ok(order)
    }

    /// Returns the best bid spread price.
    #[must_use]
    pub fn best_bid(&self) -> Option<i64> {
        self.bids.keys().next_back().copied()
    }

    /// Returns the best ask spread price.
    #[must_use]
    pub fn best_ask(&self) -> Option<i64> {
        self.asks.keys().next().copied()
    }

    /// Returns the highest-priority resting order on `side`.
    #[must_use]
    pub fn best_order(&self, side: Side) -> Option<&ComboOrder> {
        let levels = self.levels(side);
        let queue = match side {
            Side::Buy => levels.last_key_value().map(|(_, queue)| queue),
            Side::Sell => levels.first_key_value().map(|(_, queue)| queue),
        }?;
        queue.front()
    }

    /// Looks up a resting order by exchange id.
    #[must_use]
    pub fn order(&self, order_id: u64) -> Option<&ComboOrder> {
        let &(side, spread_price) = self.locations.get(&order_id)?;
        self.levels(side)
            .get(&spread_price)?
            .iter()
            .find(|order| order.order_id == order_id)
    }

    /// Returns the number of resting orders across both sides.
    #[must_use]
    pub fn len(&self) -> usize {
        self.locations.len()
    }

    /// Returns true when no base order is resting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.locations.is_empty()
    }

    fn levels(&self, side: Side) -> &BTreeMap<i64, VecDeque<ComboOrder>> {
        match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        }
    }

    fn levels_mut(&mut self, side: Side) -> &mut BTreeMap<i64, VecDeque<ComboOrder>> {
        match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        }
    }
}

/// Validates one order before it is placed into a restored book.
fn validate_restored_order(
    order: &ComboOrder,
    pair_id: u32,
    expected_side: Side,
    locations: &U64MixMap<(Side, i64)>,
) -> Result<(), ComboBookError> {
    if order.order_id == 0 {
        return Err(ComboBookError::ZeroOrderId {
            order_id: order.order_id,
        });
    }
    if order.pair_id != pair_id {
        return Err(ComboBookError::PairMismatch {
            book_pair_id: pair_id,
            order_pair_id: order.pair_id,
        });
    }
    if order.quantity == 0 {
        return Err(ComboBookError::ZeroQuantity {
            order_id: order.order_id,
        });
    }
    if order.remaining == 0 || order.remaining > order.quantity {
        return Err(ComboBookError::InvalidRemaining {
            order_id: order.order_id,
            quantity: order.quantity,
            remaining: order.remaining,
        });
    }
    if order.side != expected_side {
        return Err(ComboBookError::SideMismatch {
            order_id: order.order_id,
            expected: expected_side,
            actual: order.side,
        });
    }
    if locations.contains_key(&order.order_id) {
        return Err(ComboBookError::DuplicateOrderId {
            order_id: order.order_id,
        });
    }
    Ok(())
}

/// Returns true when `taker_limit` accepts a trade at `resting_price`.
fn crosses(taker_side: Side, taker_limit: i64, resting_price: i64) -> bool {
    match taker_side {
        Side::Buy => resting_price <= taker_limit,
        Side::Sell => resting_price >= taker_limit,
    }
}

/// One successful Combo Book state transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ComboEvent {
    /// A base order was accepted and now rests in its pair book.
    Rested {
        /// Stored order, including its exchange id and arrival sequence.
        order: ComboOrder,
    },
    /// Two opposite base orders executed atomically.
    Filled {
        /// The atomic Combo Fill.
        fill: ComboFill,
    },
    /// An implied-in taker consumed two resting leg orders atomically.
    ImpliedFilled {
        /// The atomic implied-in Combo Fill.
        fill: ImpliedComboFill,
    },
    /// A resting base order was removed by cancel.
    Canceled {
        /// Removed order.
        order: ComboOrder,
    },
}

/// Why the command translation seam refused a combo command.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ComboCommandError {
    /// The command named a pair id with no registered Combo Pair.
    UnknownPair {
        /// Pair id carried by the rejected command.
        pair_id: u32,
    },
    /// Only new orders and cancels are combo commands.
    UnsupportedCommandType {
        /// Rejected command type.
        command_type: CommandType,
    },
    /// Combo orders are only accepted in continuous trading.
    OutsideContinuousTrading {
        /// Current venue phase.
        phase: crate::trading::core::public_events::TradingPhase,
    },
    /// M1 combo orders are limit orders only.
    UnsupportedPriceType {
        /// Rejected price type.
        price_type: PriceType,
    },
    /// Combo orders support GFD, IOC-Any (FAK), and IOC-All (FOK).
    UnsupportedTimeCondition {
        /// Rejected time condition.
        time_condition: TimeCondition,
    },
    /// GFD combos use Any quantity; IOC combos use Any or All.
    UnsupportedVolumeCondition {
        /// Rejected volume condition.
        volume_condition: VolumeCondition,
    },
    /// M1 combo orders do not support stop triggers.
    UnsupportedTriggerType {
        /// Rejected trigger type.
        trigger_type: TriggerType,
    },
    /// M1 combo fills open leg positions only.
    UnsupportedOffset {
        /// Rejected leg offset.
        offset: Offset,
    },
    /// A combo order must request a positive quantity.
    ZeroQuantity {
        /// Pair id carried by the rejected command.
        pair_id: u32,
    },
    /// The command is outside the pair's valid trading window.
    OutsidePairWindow {
        /// Pair whose window was violated.
        pair_id: u32,
        /// Command trading timestamp.
        trading_ts_ns: u64,
    },
    /// The spread price is not on the pair's spread tick.
    OffTickSpreadPrice {
        /// Rejected spread price.
        price_ticks: i64,
        /// Pair spread tick.
        spread_tick: u64,
    },
    /// A FOK combo order cannot fill its full quantity.
    FokUnfillable {
        /// Pair id carried by the rejected command.
        pair_id: u32,
        /// Requested quantity.
        quantity: u64,
    },
    /// Cancel did not carry an exchange order id.
    MissingCancelOrderId,
    /// The account cannot back both legs' estimated margin.
    Margin(ComboMarginError),
    /// The underlying book refused the transition.
    Book(ComboBookError),
}

impl Display for ComboCommandError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::UnknownPair { pair_id } => {
                write!(formatter, "combo pair {pair_id} is not registered")
            }
            Self::UnsupportedCommandType { command_type } => {
                write!(formatter, "unsupported combo command type {command_type:?}")
            }
            Self::OutsideContinuousTrading { phase } => {
                write!(
                    formatter,
                    "combo orders are not accepted in phase {phase:?}"
                )
            }
            Self::UnsupportedPriceType { price_type } => {
                write!(formatter, "unsupported combo price type {price_type:?}")
            }
            Self::UnsupportedTimeCondition { time_condition } => {
                write!(
                    formatter,
                    "unsupported combo time condition {time_condition:?}"
                )
            }
            Self::UnsupportedVolumeCondition { volume_condition } => {
                write!(
                    formatter,
                    "unsupported combo volume condition {volume_condition:?}"
                )
            }
            Self::UnsupportedTriggerType { trigger_type } => {
                write!(formatter, "unsupported combo trigger type {trigger_type:?}")
            }
            Self::UnsupportedOffset { offset } => {
                write!(formatter, "unsupported combo leg offset {offset:?}")
            }
            Self::ZeroQuantity { pair_id } => {
                write!(formatter, "combo pair {pair_id} order has zero quantity")
            }
            Self::MissingCancelOrderId => formatter.write_str("combo cancel has no order id"),
            Self::Margin(error) => Display::fmt(error, formatter),
            Self::OutsidePairWindow {
                pair_id,
                trading_ts_ns,
            } => write!(
                formatter,
                "combo pair {pair_id} is not valid at {trading_ts_ns}"
            ),
            Self::OffTickSpreadPrice {
                price_ticks,
                spread_tick,
            } => write!(
                formatter,
                "spread price {price_ticks} is not a multiple of tick {spread_tick}"
            ),
            Self::FokUnfillable { pair_id, quantity } => write!(
                formatter,
                "combo pair {pair_id} cannot fill FOK quantity {quantity}"
            ),
            Self::Book(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for ComboCommandError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Margin(error) => Some(error),
            Self::Book(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ComboBookError> for ComboCommandError {
    fn from(error: ComboBookError) -> Self {
        Self::Book(error)
    }
}

impl From<ComboMarginError> for ComboCommandError {
    fn from(error: ComboMarginError) -> Self {
        Self::Margin(error)
    }
}

/// Rejects command shapes M1 combo trading does not support.
///
/// This is deliberately explicit: a market, FOK/FAK, stop, or close-offset
/// combo request never degrades into an opening GFD limit order.
pub(crate) fn validate_new_combo_order(command: &Command) -> Result<(), ComboCommandError> {
    if !matches!(command.price_type, PriceType::Limit | PriceType::Market) {
        return Err(ComboCommandError::UnsupportedPriceType {
            price_type: command.price_type,
        });
    }
    if !matches!(
        command.time_condition,
        TimeCondition::Gfd | TimeCondition::Ioc
    ) {
        return Err(ComboCommandError::UnsupportedTimeCondition {
            time_condition: command.time_condition,
        });
    }
    let valid_volume = matches!(
        (command.time_condition, command.volume_condition),
        (TimeCondition::Gfd, VolumeCondition::Any)
            | (
                TimeCondition::Ioc,
                VolumeCondition::Any | VolumeCondition::All
            )
    );
    if !valid_volume {
        return Err(ComboCommandError::UnsupportedVolumeCondition {
            volume_condition: command.volume_condition,
        });
    }
    if command.trigger_type != TriggerType::None {
        return Err(ComboCommandError::UnsupportedTriggerType {
            trigger_type: command.trigger_type,
        });
    }
    if command.offset != Offset::Open {
        return Err(ComboCommandError::UnsupportedOffset {
            offset: command.offset,
        });
    }
    if command.leg2_offset != Offset::Open {
        return Err(ComboCommandError::UnsupportedOffset {
            offset: command.leg2_offset,
        });
    }
    if command.quantity == 0 {
        return Err(ComboCommandError::ZeroQuantity {
            pair_id: command.combo_id,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(order_id: u64, side: Side, spread_price: i64, quantity: u64) -> ComboOrder {
        ComboOrder::new(
            order_id,
            10,
            ComboOrderOwner::new(1, 2, order_id),
            side,
            spread_price,
            quantity,
        )
    }

    fn book_with_two_orders() -> ComboBook {
        let mut book = ComboBook::new(10);
        book.rest(order(1, Side::Buy, 90, 3)).unwrap();
        book.rest(order(2, Side::Buy, 90, 4)).unwrap();
        book.rest(order(3, Side::Sell, 110, 5)).unwrap();
        book
    }

    #[test]
    fn snapshot_restore_preserves_queue_order_and_trade_price() {
        let book = book_with_two_orders();
        let snapshot = book.snapshot();
        let mut restored = ComboBook::new(10);
        restored.restore_snapshot(snapshot.clone()).unwrap();

        assert_eq!(restored.snapshot(), snapshot);
        assert_eq!(
            restored.best_order(Side::Buy).map(|order| order.order_id),
            Some(1)
        );
        restored
            .submit(order(4, Side::Sell, 90, 1))
            .expect("a crossing order must execute");
        assert_eq!(restored.last_trade_price(), Some(90));
    }

    #[test]
    fn restore_rejects_duplicate_arrival_sequence() {
        let mut book = book_with_two_orders();
        let mut snapshot = book.snapshot();
        let mut duplicate = snapshot.bids[0];
        duplicate.order_id = 9;
        snapshot.bids.push(duplicate);

        assert_eq!(
            book.restore_snapshot(snapshot).unwrap_err(),
            ComboBookError::DuplicateArrivalSeq {
                order_id: 9,
                arrival_seq: 0,
            }
        );
    }

    #[test]
    fn restore_rejects_a_stale_next_arrival_sequence() {
        let mut book = book_with_two_orders();
        let mut snapshot = book.snapshot();
        snapshot.next_arrival_seq = 0;

        assert_eq!(
            book.restore_snapshot(snapshot).unwrap_err(),
            ComboBookError::InvalidNextArrivalSeq {
                next_arrival_seq: 0,
                max_arrival_seq: 2,
            }
        );
    }

    #[test]
    fn restore_rejects_a_crossed_book() {
        let mut book = book_with_two_orders();
        let mut snapshot = book.snapshot();
        snapshot.asks[0].spread_price = 90;

        assert_eq!(
            book.restore_snapshot(snapshot).unwrap_err(),
            ComboBookError::CrossedBook {
                best_bid: 90,
                best_ask: 90,
            }
        );
    }
}
