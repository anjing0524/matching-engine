//! The order book module.
//!
//! [`TickBasedOrderBook`] maps discrete futures prices directly to array
//! indices, giving O(1) price lookup and hardware-accelerated best-price
//! discovery through [`crate::shared::collections::fast_bitmap::FastBitmap`].
pub(crate) mod tick_based;

pub use tick_based::{
    ContractSpec, MatchOutcome, OrderNode, RestingFill, RestingOrder, TickBasedOrderBook,
};
