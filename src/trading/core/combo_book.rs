//! Base Combo Order book for one Combo Pair.
//!
//! M1 rests only directly submitted base combo orders. Derived implied orders
//! are matching candidates and have no insertion path here, so the public combo
//! book never contains them. The book is keyed by spread price and time; spread
//! prices are signed because a combo quote may be negative.
//!
//! The book has no matching and no public market-data projection. It stores the
//! owner and session metadata later tickets need for cancel, fills, and private
//! replay.

use crate::shared::collections::U64MixMap;
use crate::trading::core::protocol::{
    Command, CommandType, PriceType, Side, TimeCondition, TriggerType, VolumeCondition,
};
use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::fmt::{Display, Formatter, Result as FmtResult};

/// Owner and client identity preserved with a resting Combo Order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// One resting base Combo Order.
///
/// The exchange-assigned `order_id` is distinct from
/// [`ComboOrderOwner::client_order_id`]. `spread_price` is signed; `remaining`
/// starts at `quantity` and is reduced only by future matching.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// Why a base Combo Order could not rest in or leave a [`ComboBook`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ComboBookError {
    /// An order with this exchange id is already resting.
    DuplicateOrderId {
        /// Duplicate exchange order id.
        order_id: u64,
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
}

impl Display for ComboBookError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::DuplicateOrderId { order_id } => {
                write!(formatter, "combo order {order_id} is already resting")
            }
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
    pub fn rest(&mut self, mut order: ComboOrder) -> Result<ComboOrder, ComboBookError> {
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

        order.arrival_seq = self.next_arrival_seq;
        self.next_arrival_seq += 1;
        self.levels_mut(order.side)
            .entry(order.spread_price)
            .or_default()
            .push_back(order);
        self.locations
            .insert(order.order_id, (order.side, order.spread_price));
        Ok(order)
    }

    /// Removes and returns a resting order by exchange id.
    ///
    /// # Errors
    /// Returns [`ComboBookError::UnknownOrder`] when the order is not resting.
    /// A repeated cancel of the same id therefore fails explicitly.
    pub fn cancel(&mut self, order_id: u64) -> Result<ComboOrder, ComboBookError> {
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

/// One successful Combo Book state transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ComboEvent {
    /// A base order was accepted and now rests in its pair book.
    Rested {
        /// Stored order, including its exchange id and arrival sequence.
        order: ComboOrder,
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
    /// M1 combo orders are limit orders only.
    UnsupportedPriceType {
        /// Rejected price type.
        price_type: PriceType,
    },
    /// M1 combo orders are GFD only.
    UnsupportedTimeCondition {
        /// Rejected time condition.
        time_condition: TimeCondition,
    },
    /// M1 combo orders use any-quantity volume semantics only.
    UnsupportedVolumeCondition {
        /// Rejected volume condition.
        volume_condition: VolumeCondition,
    },
    /// M1 combo orders do not support stop triggers.
    UnsupportedTriggerType {
        /// Rejected trigger type.
        trigger_type: TriggerType,
    },
    /// A combo order must request a positive quantity.
    ZeroQuantity {
        /// Pair id carried by the rejected command.
        pair_id: u32,
    },
    /// Cancel did not carry an exchange order id.
    MissingCancelOrderId,
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
            Self::ZeroQuantity { pair_id } => {
                write!(formatter, "combo pair {pair_id} order has zero quantity")
            }
            Self::MissingCancelOrderId => formatter.write_str("combo cancel has no order id"),
            Self::Book(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for ComboCommandError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
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

/// Rejects command shapes M1 combo trading does not support.
///
/// This is deliberately explicit: a market, FOK/FAK, or stop combo request
/// never degrades into a GFD limit order.
pub(crate) fn validate_new_combo_order(command: &Command) -> Result<(), ComboCommandError> {
    if command.price_type != PriceType::Limit {
        return Err(ComboCommandError::UnsupportedPriceType {
            price_type: command.price_type,
        });
    }
    if command.time_condition != TimeCondition::Gfd {
        return Err(ComboCommandError::UnsupportedTimeCondition {
            time_condition: command.time_condition,
        });
    }
    if command.volume_condition != VolumeCondition::Any {
        return Err(ComboCommandError::UnsupportedVolumeCondition {
            volume_condition: command.volume_condition,
        });
    }
    if command.trigger_type != TriggerType::None {
        return Err(ComboCommandError::UnsupportedTriggerType {
            trigger_type: command.trigger_type,
        });
    }
    if command.quantity == 0 {
        return Err(ComboCommandError::ZeroQuantity {
            pair_id: command.combo_id,
        });
    }
    Ok(())
}
